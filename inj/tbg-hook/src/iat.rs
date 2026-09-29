//! IAT 重定向（任务 34，docs/plan.md v2 §5 清室架构核心）。
//!
//! 枚举本进程（explorer）已加载模块，解析每个模块的 PE 导入表，把
//! **静态导入** `shell32.dll!SHGetPropertyStoreForWindow` 的 IAT 槽位
//! （`IMAGE_IMPORT_DESCRIPTOR.FirstThunk` 数组元素）替换为我们的桩函数
//! 地址；原值保存供透传调用与摘钩恢复。
//!
//! 为什么是 IAT 而不是 inline hook：不改任何代码字节，只改数据指针
//! （x64 对齐 8 字节写原子），无指令长度反汇编需求，无私有符号依赖；
//! 导入名（`SHGetPropertyStoreForWindow` / `SHELL32.dll`）是稳定 ABI。
//!
//! 已知边界（plan v2 §5 已知限制①）：delay-load 导入、运行时
//! `GetProcAddress` 自解析、无 OriginalFirstThunk（INT 缺失）的模块
//! 不在拦截面内——统计字段直接暴露命中情况。

use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::Ordering;
use std::sync::Mutex;

use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Module32FirstW, Module32NextW, MODULEENTRY32W, TH32CS_SNAPMODULE,
    TH32CS_SNAPMODULE32,
};
use windows::Win32::System::Memory::VirtualProtect;

use tbg_proto::{PatchedModule, SharedState, ERR_SNAPSHOT};

// ---------------- PE 结构（x64，手写最小集；单元测试钉布局） ----------------

/// DOS 头（仅用 e_magic / e_lfanew；64 字节）。
#[repr(C)]
struct ImageDosHeader {
    e_magic: u16,      // 'MZ' = 0x5A4D
    _rest: [u8; 58],   // 偏移 2..60
    e_lfanew: i32,     // 偏移 60：NT 头 RVA
}

/// COFF 文件头（20 字节）。
#[repr(C)]
struct ImageFileHeader {
    machine: u16,
    number_of_sections: u16,
    time_date_stamp: u32,
    pointer_to_symbol_table: u32,
    number_of_symbols: u32,
    size_of_optional_header: u16,
    characteristics: u16,
}

/// PE32+ 可选头（240 字节；仅声明用到的字段，其余对齐占位——偏移由
/// 单元测试钉死，读取走 `opt.add(OFFSET)` 原始偏移）。
#[repr(C)]
struct ImageOptionalHeader64 {
    magic: u16,                               // 偏移 0（0x20B）
    _r0: [u8; 54],                            // 2..56
    size_of_image: u32,                       // 偏移 56
    _r1: [u8; 52],                            // 60..112
    data_directory: [ImageDataDirectory; 16], // 偏移 112..240
}

#[repr(C)]
struct ImageDataDirectory {
    virtual_address: u32,
    _size: u32,
}

/// 导入描述符（20 字节，导入目录表项）。
#[repr(C)]
struct ImageImportDescriptor {
    original_first_thunk: u32, // INT（按名导入信息）
    _time_date_stamp: u32,
    _forwarder_chain: u32,
    name_rva: u32,             // 导入 DLL 名（如 "SHELL32.dll"）
    first_thunk_rva: u32,      // IAT（运行时槽位数组）
}

const IMAGE_DOS_SIGNATURE: u16 = 0x5A4D;
const IMAGE_NT_SIGNATURE: u32 = 0x0000_4550;
const IMAGE_OPTIONAL_MAGIC_PE32PLUS: u16 = 0x020B;
/// PE32+ 可选头内 DataDirectory[1]（导入表）的偏移。
const IMPORT_DIRECTORY_OFFSET: usize = 112 + 8;
/// PE32+ 可选头内 SizeOfImage 字段偏移。
const SIZE_OF_IMAGE_OFFSET: usize = 56;
/// x64 序号导入标志位（Thunk 高位）。
const IMAGE_ORDINAL_FLAG64: u64 = 0x8000_0000_0000_0000;

// ---------------- 补丁登记（init 写入 / stop / DETACH 恢复读取） ----------------

/// (槽位地址, 原始值)。LIFO 恢复。
static SLOTS: Mutex<Vec<(usize, u64)>> = Mutex::new(Vec::new());

/// 目标导入名（winuser 文档化导出，全 ABI 稳定）。
const TARGET_FN: &[u8] = b"SHGetPropertyStoreForWindow";
/// 目标 DLL 名前缀（大小写不敏感匹配 "SHELL32.dll" 等）。
const TARGET_DLL: &[u8] = b"shell32";

/// 安装：扫描全部模块并重定向。返回 false 仅当模块快照失败。
///
/// * `hook_addr` —— 桩函数地址（lib.rs 传入）
/// * `original_out` —— 输出原始函数地址（lib.rs 的 ORIGINAL_FN）
/// * `s` —— 共享状态（统计写入）
pub(crate) unsafe fn install(hook_addr: usize, original_out: *mut usize, s: *mut SharedState) -> bool {
    let snap = match CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, 0) {
        Ok(h) => h,
        Err(_) => {
            (*s).err = ERR_SNAPSHOT;
            return false;
        }
    };
    let self_base = super::SELF_MODULE.load(Ordering::Relaxed);
    let mut scanned: u32 = 0;
    let mut patched_total: u32 = 0;
    let mut first_original: usize = 0;

    let mut me = MODULEENTRY32W {
        dwSize: std::mem::size_of::<MODULEENTRY32W>() as u32,
        ..Default::default()
    };
    if Module32FirstW(snap, &mut me).is_ok() {
        loop {
            let base = me.modBaseAddr as usize;
            if base != 0 && base != self_base {
                scanned += 1;
                let (hits, original) = scan_module(base, hook_addr);
                if hits > 0 {
                    if first_original == 0 {
                        first_original = original;
                    }
                    patched_total += hits;
                    record_module(s, &me.szModule, hits);
                }
            }
            if Module32NextW(snap, &mut me).is_err() {
                break;
            }
        }
    }
    let _ = CloseHandle(snap);

    (*s).modules_scanned = scanned;
    (*s).slots_patched = patched_total;
    if first_original != 0 {
        ptr::write_volatile(original_out, first_original);
    }
    true
}

/// 摘钩：恢复所有 IAT 槽（幂等；DLL_PROCESS_DETACH 兜底路径复用）。
pub(crate) unsafe fn remove() {
    let mut guard = match SLOTS.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    while let Some((slot, original)) = guard.pop() {
        let p = slot as *mut u64;
        let newp = windows::Win32::System::Memory::PAGE_READWRITE;
        let mut old = newp;
        if VirtualProtect(slot as *const c_void, 8, newp, &mut old).is_ok() {
            ptr::write_volatile(p, original);
            let restore = old;
            let mut dummy = restore;
            let _ = VirtualProtect(slot as *const c_void, 8, restore, &mut dummy);
        }
    }
}

/// 解析单个模块的导入表；返回 (命中槽位数, 首个原始函数地址)。
unsafe fn scan_module(base: usize, hook_addr: usize) -> (u32, usize) {
    // DOS 头
    let dos = ptr::read_unaligned(base as *const ImageDosHeader);
    if dos.e_magic != IMAGE_DOS_SIGNATURE {
        return (0, 0);
    }
    if dos.e_lfanew <= 0 || dos.e_lfanew as usize > 0x1000 {
        return (0, 0);
    }
    let nt = base + dos.e_lfanew as usize;
    let sig = ptr::read_unaligned(nt as *const u32);
    if sig != IMAGE_NT_SIGNATURE {
        return (0, 0);
    }
    // 可选头起点 = NT 头 + 4（签名） + 20（文件头）
    let opt = nt + 4 + std::mem::size_of::<ImageFileHeader>();
    let magic = ptr::read_unaligned(opt as *const u16);
    if magic != IMAGE_OPTIONAL_MAGIC_PE32PLUS {
        return (0, 0); // 仅支持 x64 PE32+（与目标平台一致）
    }
    let size_of_image = ptr::read_unaligned((opt + SIZE_OF_IMAGE_OFFSET) as *const u32) as usize;
    if size_of_image == 0 {
        return (0, 0);
    }
    let imp_rva =
        ptr::read_unaligned((opt + IMPORT_DIRECTORY_OFFSET) as *const u32) as usize;
    if imp_rva == 0 || imp_rva >= size_of_image {
        return (0, 0);
    }

    let mut hits: u32 = 0;
    let mut first_original: usize = 0;
    let mut desc = (base + imp_rva) as *const ImageImportDescriptor;
    loop {
        let d = ptr::read_unaligned(desc);
        if d.original_first_thunk == 0 && d.first_thunk_rva == 0 && d.name_rva == 0 {
            break; // 目录表结束哨兵
        }
        // INT 缺失（纯绑定导入）时名称不可靠，跳过（模块注释"已知边界"）。
        if d.original_first_thunk != 0 && d.name_rva != 0 && (d.name_rva as usize) < size_of_image {
            if dll_name_matches(base, d.name_rva as usize) {
                let int_base = (base + d.original_first_thunk as usize) as *const u64;
                let mut i = 0usize;
                loop {
                    let thunk = ptr::read_volatile(int_base.add(i));
                    if thunk == 0 {
                        break;
                    }
                    if thunk & IMAGE_ORDINAL_FLAG64 == 0 {
                        let name_rva = thunk as usize;
                        if name_rva < size_of_image
                            && import_name_matches(base, name_rva)
                        {
                            let slot = (base + d.first_thunk_rva as usize + i * 8) as *mut u64;
                            let current = ptr::read_volatile(slot) as usize;
                            if current != 0 && current != hook_addr {
                                if patch_slot(slot, hook_addr) {
                                    if first_original == 0 {
                                        first_original = current;
                                    }
                                    record_slot(slot as usize, current as u64);
                                    hits += 1;
                                }
                            }
                        }
                    }
                    i += 1;
                }
            }
        }
        desc = desc.add(1);
        // 防御：描述符数组越界（损坏 PE）
        if (desc as usize) > base + size_of_image {
            break;
        }
    }
    (hits, first_original)
}

/// 读 DLL 名（C 字符串，截断 32 字节），与 `SHELL32` 前缀不区分大小写匹配。
unsafe fn dll_name_matches(base: usize, name_rva: usize) -> bool {
    let p = (base + name_rva) as *const u8;
    for k in 0..32 {
        let c = *p.add(k);
        if c == 0 {
            // 名串结束：仅当目标前缀已全部匹配才命中（如 "SHELL32"）
            return k >= TARGET_DLL.len();
        }
        if k >= TARGET_DLL.len() {
            return true; // 前缀已全匹配（如 "SHELL32.dll"）
        }
        if c.to_ascii_lowercase() != TARGET_DLL[k] {
            return false;
        }
    }
    true
}

/// 读 IMAGE_IMPORT_BY_NAME 的名称（跳过 2 字节 Hint），与目标导出名
/// 不区分大小写匹配。
unsafe fn import_name_matches(base: usize, name_rva: usize) -> bool {
    let p = (base + name_rva + 2) as *const u8;
    for k in 0..64 {
        let c = *p.add(k);
        if c == 0 {
            return k == TARGET_FN.len();
        }
        if k >= TARGET_FN.len() {
            return false;
        }
        if c.to_ascii_lowercase() != TARGET_FN[k] {
            return false;
        }
    }
    false
}

/// 单槽位打补丁：临时 RW → 写入 → 恢复保护属性。
unsafe fn patch_slot(slot: *mut u64, hook_addr: usize) -> bool {
    let addr = slot as *const c_void;
    let newp = windows::Win32::System::Memory::PAGE_READWRITE;
    let mut old = newp;
    if VirtualProtect(addr, 8, newp, &mut old).is_err() {
        return false;
    }
    ptr::write_volatile(slot, hook_addr as u64);
    let restore = old;
    let mut dummy = restore;
    let _ = VirtualProtect(addr, 8, restore, &mut dummy);
    true
}

unsafe fn record_slot(slot: usize, original: u64) {
    let mut guard = match SLOTS.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.push((slot, original));
}

/// 命中模块记账进共享状态（前 12 个命中模块，名称 ASCII 截断 24）。
unsafe fn record_module(s: *mut SharedState, wide_name: &[u16; 256], hits: u32) {
    // 模块名（ASCII 化；非 ASCII 字节替换为 '_'）
    let mut name = [0u8; 24];
    let mut len = 0usize;
    for &u in wide_name.iter() {
        if u == 0 {
            break;
        }
        let b = if u < 128 { u as u8 } else { b'_' };
        if len == name.len() {
            len = name.len(); // 截断
            break;
        }
        name[len] = b;
        len += 1;
    }
    let _ = len;

    // 同名累加，否则首个空位写入
    for rec in (*s).patched.iter_mut() {
        if rec.name[0] != 0 && name_eq(&rec.name, &name) {
            rec.slots += hits;
            return;
        }
    }
    for rec in (*s).patched.iter_mut() {
        if rec.name[0] == 0 {
            rec.name = name;
            rec.slots = hits;
            return;
        }
    }
}

fn name_eq(a: &[u8; 24], b: &[u8; 24]) -> bool {
    a.iter().eq(b.iter())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::size_of;

    /// PE 布局守卫：与 winnt.h 逐一对照（错位 = 扫描读错字段，静默失效）。
    #[test]
    fn pe_layout_offsets() {
        assert_eq!(size_of::<ImageDosHeader>(), 64);
        assert_eq!(std::mem::offset_of!(ImageDosHeader, e_lfanew), 60);
        assert_eq!(size_of::<ImageFileHeader>(), 20);
        assert_eq!(size_of::<ImageOptionalHeader64>(), 240);
        assert_eq!(
            std::mem::offset_of!(ImageOptionalHeader64, size_of_image),
            56
        );
        assert_eq!(
            std::mem::offset_of!(ImageOptionalHeader64, data_directory),
            112
        );
        assert_eq!(size_of::<ImageDataDirectory>(), 8);
        assert_eq!(size_of::<ImageImportDescriptor>(), 20);
        assert_eq!(SIZE_OF_IMAGE_OFFSET, 56);
        assert_eq!(IMPORT_DIRECTORY_OFFSET, 120); // 数据目录[1] = 112 + 8
    }

    #[test]
    fn target_constants() {
        assert_eq!(TARGET_FN, b"SHGetPropertyStoreForWindow");
        assert_eq!(TARGET_DLL, b"shell32");
        assert_eq!(IMAGE_ORDINAL_FLAG64, 0x8000_0000_0000_0000);
    }
}
