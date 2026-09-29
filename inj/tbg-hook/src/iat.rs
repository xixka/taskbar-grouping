//! IAT 重定向（任务 34，docs/plan.md v2 §5 清室架构核心）。
//!
//! 枚举本进程（explorer）已加载模块，解析每个模块的 PE 导入表，把
//! **静态导入** `SHGetPropertyStoreForWindow` 的 IAT 槽位替换为我们的
//! 桩函数地址；原值保存供透传调用与摘钩恢复。
//!
//! 三层拦截面（任务 36 修复轮 2 扩充；首轮实测 246 模块 0 命中后按
//! plan v2 §5 已知限制①的决策树扩充）：
//! 1. 普通 IAT 槽——按函数名匹配、不按 DLL 名（API Set 别名
//!    `api-ms-win-shell-shell32-*.dll` 导入同名可达）；
//! 2. delay-load 目录（DataDirectory[13]）的目标槽——首次调用前的槽值
//!    是 delay helper thunk 而非真实函数，故原函数地址统一改从
//!    shell32 导出表解析（GetModuleHandleW + GetProcAddress），不取槽内值；
//! 3. `GetProcAddress` 的 IAT 槽——覆盖运行时动态解析（含 delay helper
//!    经本模块 IAT 的内部解析）：桩透传一切查询，仅当查询名恰为目标
//!    函数且原调用可解析时替换为 [`super::stub_get_store`]。
//!
//! **大小写教训（修复轮 2 根因）**：目标常量以全小写存储、两侧统一
//! `to_ascii_lowercase` 比较——此前单侧小写对混合大小写目标
//! `SHGetPropertyStoreForWindow` 恒不匹配，是首轮 scanned=246 /
//! patched=0 的直接根因（单元测试 `import_name_matching_*` 钉死）。
//!
//! 为什么是 IAT 而不是 inline hook：不改任何代码字节，只改数据指针
//! （x64 对齐 8 字节写原子），无指令长度反汇编需求，无私有符号依赖；
//! 导入名 `SHGetPropertyStoreForWindow` 是稳定 ABI。
//!
//! 已知边界（plan v2 §5 已知限制①）：运行时经 `GetProcAddressForCaller`
//! 等变体自解析、或任务栏根本不经本 API 读 AUMID 的情形不在拦截面内
//! ——统计字段（patched/gpa/calls）直接暴露命中情况。

use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::Ordering;
use std::sync::Mutex;

use windows::core::{s, w};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Module32FirstW, Module32NextW, MODULEENTRY32W, TH32CS_SNAPMODULE,
    TH32CS_SNAPMODULE32,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Memory::VirtualProtect;

use tbg_proto::{PatchedModule, SharedState, ERR_SNAPSHOT};

// ---------------- PE 结构（x64，手写最小集；单元测试钉布局） ----------------

#[repr(C)]
struct ImageDosHeader {
    e_magic: u16,
    _r0: [u8; 58],
    e_lfanew: i32,
}

#[repr(C)]
struct ImageFileHeader {
    _machine: u16,
    _number_of_sections: u16,
    _time_date_stamp: u32,
    _ptr_to_symbol_table: u32,
    _number_of_symbols: u32,
    size_of_optional_header: u16,
    characteristics: u16,
}

#[repr(C)]
struct ImageDataDirectory {
    _virtual_address: u32,
    _size: u32,
}

/// 导入描述符（20 字节）。
#[repr(C)]
struct ImageImportDescriptor {
    original_first_thunk: u32, // INT（名称表）RVA
    _time_date_stamp: u32,
    _forwarder_chain: u32,
    name_rva: u32,
    first_thunk_rva: u32, // IAT（地址表）RVA
}

/// delay-load 描述符（x64 64 字节；attrs 位 1 = 字段为 RVA，否则 VA）。
#[repr(C)]
struct ImageDelayDescriptor {
    attrs: u32,
    _pad0: u32,
    name: u64,
    _phmod: u64,
    iat: u64,
    int_: u64,
    _bound_iat: u64,
    _unload_iat: u64,
    _timestamp: u32,
    _pad1: u32,
}

// ---------------- 常量 ----------------

const IMAGE_DOS_SIGNATURE: u16 = 0x5A4D;
const IMAGE_NT_SIGNATURE: u32 = 0x0000_4550;
const IMAGE_OPTIONAL_MAGIC_PE32PLUS: u16 = 0x020B;
/// PE32+ 可选头内 DataDirectory[1]（导入表）的偏移。
const IMPORT_DIRECTORY_OFFSET: usize = 112 + 8;
/// PE32+ 可选头内 DataDirectory[13]（delay-load 导入表）的偏移。
const DELAY_DIRECTORY_OFFSET: usize = 112 + 13 * 8;
/// PE32+ 可选头内 SizeOfImage 字段偏移。
const SIZE_OF_IMAGE_OFFSET: usize = 56;
/// PE32+ 可选头内 ImageBase 字段偏移（非 RVA delay 字段换算用）。
const IMAGE_BASE_OFFSET: usize = 24;
/// x64 序号导入标志位（Thunk 高位）。
const IMAGE_ORDINAL_FLAG64: u64 = 0x8000_0000_0000_0000;
/// delay 描述符 attrs 的 RVA 标志位（dlattrRva）。
const DLATTR_RVA: u32 = 1;

// ---------------- 补丁登记（init 写入 / stop / DETACH 恢复读取） ----------------

/// (槽位地址, 原始值)。LIFO 恢复。普通 / delay / GetProcAddress 槽统一登记。
static SLOTS: Mutex<Vec<(usize, u64)>> = Mutex::new(Vec::new());

/// 目标导入名（**全小写存储**，匹配两侧统一小写——见模块注释的大小写
/// 教训）。winuser 文档化导出，全 ABI 稳定；全进程仅 shell32 导出。
pub(crate) const TARGET_FN: &[u8] = b"shgetpropertystoreforwindow";
/// GetProcAddress 桩的目标名（同为小写）。
const GPA_FN: &[u8] = b"getprocaddress";

/// 安装：扫描全部模块并重定向（三层拦截面，见模块注释）。
/// 返回 false 仅当模块快照失败。
///
/// * `hook_addr` —— 目标函数桩地址（lib.rs 的 stub_get_store）
/// * `gpa_hook_addr` —— GetProcAddress 桩地址（lib.rs 的 stub_get_proc_address）
/// * `original_out` / `gpa_original_out` —— 原始函数地址输出
/// * `s` —— 共享状态（统计写入）
pub(crate) unsafe fn install(
    hook_addr: usize,
    gpa_hook_addr: usize,
    original_out: *mut usize,
    gpa_original_out: *mut usize,
    s: *mut SharedState,
) -> bool {
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
    let mut delay_total: u32 = 0;
    let mut gpa_total: u32 = 0;
    let mut names_total: u32 = 0;

    // 原函数地址先于任何补丁、从导出表确定性解析（普通 IAT 槽加载后的
    // 值与之一致；delay 槽解析后的值亦同；delay 槽解析前的 helper thunk
    // 不是函数本体——首轮设计从槽内取原值的做法对 delay 场景是错的）。
    if let Ok(h) = GetModuleHandleW(w!("shell32.dll")) {
        if let Some(f) = GetProcAddress(h, s!("SHGetPropertyStoreForWindow")) {
            ptr::write_volatile(original_out, f as usize);
        }
    }
    if let Ok(h) = GetModuleHandleW(w!("kernel32.dll")) {
        if let Some(f) = GetProcAddress(h, s!("GetProcAddress")) {
            ptr::write_volatile(gpa_original_out, f as usize);
        }
    }

    let mut me = MODULEENTRY32W {
        dwSize: std::mem::size_of::<MODULEENTRY32W>() as u32,
        ..Default::default()
    };
    if Module32FirstW(snap, &mut me).is_ok() {
        loop {
            let base = me.modBaseAddr as usize;
            if base != 0 {
                if base == self_base {
                    // 自检：本 DLL 自带目标函数静态导入锚点（lib.rs），
                    // 只数不打——非 0 即证明扫描器与导入表解析正常。
                    if let Some((size_of_image, imp_rva, _)) = pe_headers(base) {
                        let (th, _, names) =
                            scan_imports(base, size_of_image, imp_rva, 0, 0, false);
                        (*s).self_slots = th;
                        names_total = names_total.saturating_add(names);
                    }
                } else if let Some((size_of_image, imp_rva, delay_rva)) = pe_headers(base) {
                    scanned += 1;
                    let (th, gh, names) = scan_imports(
                        base,
                        size_of_image,
                        imp_rva,
                        hook_addr,
                        gpa_hook_addr,
                        true,
                    );
                    let (dh, dnames) = scan_delay(base, size_of_image, delay_rva, hook_addr);
                    names_total = names_total.saturating_add(names).saturating_add(dnames);
                    patched_total += th + dh;
                    delay_total += dh;
                    gpa_total += gh;
                    if th + gh + dh > 0 {
                        record_module(s, &me.szModule, th + gh + dh);
                    }
                } else {
                    // PE 头解析失败的模块仍计入扫描面（健康度指标）。
                    scanned += 1;
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
    (*s).delay_slots = delay_total;
    (*s).gpa_slots = gpa_total;
    (*s).names_seen = names_total;
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

// ---------------- PE 头与目录 ----------------

/// 解析 PE 头：返回 (SizeOfImage, 导入表 RVA, delay-load 目录 RVA)。
unsafe fn pe_headers(base: usize) -> Option<(usize, usize, usize)> {
    let dos = base as *const ImageDosHeader;
    if ptr::read_unaligned(dos).e_magic != IMAGE_DOS_SIGNATURE {
        return None;
    }
    let e_lfanew = ptr::read_unaligned(dos).e_lfanew as usize;
    if e_lfanew == 0 || e_lfanew > 0x1000 {
        return None;
    }
    let nt = base + e_lfanew;
    if ptr::read_unaligned(nt as *const u32) != IMAGE_NT_SIGNATURE {
        return None;
    }
    let opt = nt + 4 + std::mem::size_of::<ImageFileHeader>();
    if ptr::read_unaligned(opt as *const u16) != IMAGE_OPTIONAL_MAGIC_PE32PLUS {
        return None; // 仅支持 x64 PE32+（与目标平台一致）
    }
    let size_of_image = ptr::read_unaligned((opt + SIZE_OF_IMAGE_OFFSET) as *const u32) as usize;
    if size_of_image == 0 {
        return None;
    }
    let imp_rva = ptr::read_unaligned((opt + IMPORT_DIRECTORY_OFFSET) as *const u32) as usize;
    let delay_rva = ptr::read_unaligned((opt + DELAY_DIRECTORY_OFFSET) as *const u32) as usize;
    Some((size_of_image, imp_rva, delay_rva))
}

/// 普通导入表扫描：返回 (目标命中数, GetProcAddress 命中数, 检视名称数)。
///
/// `allow_patch = false` 时只统计不修改（自检路径）。
unsafe fn scan_imports(
    base: usize,
    size_of_image: usize,
    imp_rva: usize,
    hook_addr: usize,
    gpa_hook_addr: usize,
    allow_patch: bool,
) -> (u32, u32, u32) {
    let mut hits: u32 = 0;
    let mut gpa_hits: u32 = 0;
    let mut names: u32 = 0;
    if imp_rva == 0 || imp_rva >= size_of_image {
        return (0, 0, 0);
    }
    let mut desc = (base + imp_rva) as *const ImageImportDescriptor;
    loop {
        let d = ptr::read_unaligned(desc);
        if d.original_first_thunk == 0 && d.first_thunk_rva == 0 && d.name_rva == 0 {
            break; // 目录表结束哨兵
        }
        // INT 缺失（纯绑定导入）时名称不可靠，跳过（模块注释"已知边界"）。
        // DLL 名不过滤（API Set 别名导入同名可达，任务 36 修复轮 1）。
        if d.original_first_thunk != 0 && d.first_thunk_rva != 0 {
            let int_base = (base + d.original_first_thunk as usize) as *const u64;
            let mut i = 0usize;
            loop {
                let thunk = ptr::read_volatile(int_base.add(i));
                if thunk == 0 {
                    break;
                }
                if thunk & IMAGE_ORDINAL_FLAG64 == 0 {
                    let name_rva = thunk as usize;
                    if name_rva < size_of_image {
                        names += 1;
                        if import_name_matches(base, name_rva, TARGET_FN) {
                            if allow_patch {
                                if patch_one(base, d.first_thunk_rva as usize, i, hook_addr) {
                                    hits += 1;
                                }
                            } else {
                                hits += 1; // 自检路径：只数命中不打补丁
                            }
                        } else if allow_patch && import_name_matches(base, name_rva, GPA_FN) {
                            if patch_one(base, d.first_thunk_rva as usize, i, gpa_hook_addr) {
                                gpa_hits += 1;
                            }
                        }
                    }
                }
                i += 1;
                if i > 1_000_000 {
                    break; // 防御：异常 INT 数组
                }
            }
        }
        desc = desc.add(1);
        // 防御：描述符数组越界（损坏 PE）
        if (desc as usize) > base + size_of_image {
            break;
        }
    }
    (hits, gpa_hits, names)
}

/// delay-load 目录扫描（DataDirectory[13]）：返回 (命中数, 检视名称数)。
unsafe fn scan_delay(base: usize, size_of_image: usize, delay_rva: usize, hook_addr: usize) -> (u32, u32) {
    if delay_rva == 0 || delay_rva >= size_of_image {
        return (0, 0);
    }
    let dos = base as *const ImageDosHeader;
    let nt = base + ptr::read_unaligned(dos).e_lfanew as usize;
    let opt = nt + 4 + std::mem::size_of::<ImageFileHeader>();
    let preferred = ptr::read_unaligned((opt + IMAGE_BASE_OFFSET) as *const u64) as usize;

    let mut hits = 0u32;
    let mut names = 0u32;
    let mut desc = (base + delay_rva) as *const ImageDelayDescriptor;
    loop {
        let d = ptr::read_unaligned(desc);
        if d.attrs == 0 && d.name == 0 && d.iat == 0 && d.int_ == 0 {
            break; // 结束哨兵
        }
        let rva_mode = d.attrs & DLATTR_RVA != 0;
        let resolve = |f: u64| -> usize {
            if rva_mode {
                base + f as usize
            } else {
                // 旧格式 VA 字段：按首选基址换算到实际加载基址（ASLR 重定位）
                (f as usize).saturating_sub(preferred).saturating_add(base)
            }
        };
        let int_addr = resolve(d.int_);
        let iat_addr = resolve(d.iat);
        if d.int_ != 0
            && d.iat != 0
            && int_addr > base
            && int_addr < base + size_of_image
            && iat_addr > base
            && iat_addr < base + size_of_image
        {
            let mut i = 0usize;
            loop {
                let thunk = ptr::read_volatile((int_addr + i * 8) as *const u64);
                if thunk == 0 {
                    break;
                }
                if thunk & IMAGE_ORDINAL_FLAG64 == 0 {
                    let name_rva = thunk as usize;
                    if name_rva < size_of_image {
                        names += 1;
                        if import_name_matches(base, name_rva, TARGET_FN) {
                            let slot = (iat_addr + i * 8) as *mut u64;
                            let current = ptr::read_volatile(slot) as usize;
                            // 槽值可能是 delay helper thunk（未解析）或真实
                            // 函数（已解析）——一律覆写为桩；原函数地址统一
                            // 走导出表解析（见 install），不依赖槽内值。
                            if current != 0 && current != hook_addr && patch_slot(slot, hook_addr)
                            {
                                record_slot(slot as usize, current as u64);
                                hits += 1;
                            }
                        }
                    }
                }
                i += 1;
                if i > 1_000_000 {
                    break; // 防御：异常描述符
                }
            }
        }
        desc = desc.add(1);
        if (desc as usize) > base + size_of_image {
            break;
        }
    }
    (hits, names)
}

/// 按描述符与索引打一个普通导入槽位。返回是否成功落补丁。
unsafe fn patch_one(
    base: usize,
    first_thunk_rva: usize,
    index: usize,
    hook_addr: usize,
) -> bool {
    if hook_addr == 0 {
        return false;
    }
    let slot = (base + first_thunk_rva + index * 8) as *mut u64;
    let current = ptr::read_volatile(slot) as usize;
    if current == 0 || current == hook_addr {
        return false;
    }
    if patch_slot(slot, hook_addr) {
        record_slot(slot as usize, current as u64);
        true
    } else {
        false
    }
}

/// 读 IMAGE_IMPORT_BY_NAME 的名称（跳过 2 字节 Hint），与目标导出名
/// 不区分大小写匹配（**两侧统一小写**；目标常量以小写存储——见模块
/// 注释的大小写教训）。
unsafe fn import_name_matches(base: usize, name_rva: usize, target: &[u8]) -> bool {
    let p = (base + name_rva + 2) as *const u8;
    for k in 0..64 {
        let c = *p.add(k);
        if c == 0 {
            return k == target.len();
        }
        if k >= target.len() {
            return false;
        }
        if c.to_ascii_lowercase() != target[k] {
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
    for (len, &u) in wide_name.iter().enumerate() {
        if u == 0 || len == name.len() {
            break;
        }
        name[len] = if u < 128 { u as u8 } else { b'_' };
    }

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
        assert_eq!(size_of::<ImageDataDirectory>(), 8);
        assert_eq!(size_of::<ImageImportDescriptor>(), 20);
        assert_eq!(size_of::<ImageDelayDescriptor>(), 64);
        assert_eq!(SIZE_OF_IMAGE_OFFSET, 56);
        assert_eq!(IMAGE_BASE_OFFSET, 24);
        assert_eq!(IMPORT_DIRECTORY_OFFSET, 120); // 数据目录[1] = 112 + 8
        assert_eq!(DELAY_DIRECTORY_OFFSET, 216); // 数据目录[13] = 112 + 104
    }

    #[test]
    fn target_constants() {
        // 目标名全小写存储（大小写教训见模块注释）
        assert_eq!(TARGET_FN, b"shgetpropertystoreforwindow");
        assert_eq!(GPA_FN, b"getprocaddress");
        assert_eq!(IMAGE_ORDINAL_FLAG64, 0x8000_0000_0000_0000);
        assert_eq!(DLATTR_RVA, 1);
    }

    /// 修复轮 2 根因回归测试：混合大小写的导入名必须命中（首轮 bug：
    /// 单侧 to_ascii_lowercase 对混合大小写目标恒不中 → 246 模块 0 命中）。
    #[test]
    fn import_name_matching_canonical_case() {
        // 构造迷你 IMAGE_IMPORT_BY_NAME：2 字节 Hint + 规范大小写名称
        let name = b"SHGetPropertyStoreForWindow";
        let mut buf = [0u8; 64];
        buf[2..2 + name.len()].copy_from_slice(name);
        let base = buf.as_ptr() as usize;
        unsafe {
            assert!(import_name_matches(base, 0, TARGET_FN));
            assert!(!import_name_matches(base, 0, GPA_FN)); // 不同名不中
        }
    }

    #[test]
    fn import_name_matching_case_insensitive() {
        let mut buf = [0u8; 64];
        let lower = b"shgetpropertystoreforwindow";
        buf[2..2 + lower.len()].copy_from_slice(lower);
        let base = buf.as_ptr() as usize;
        unsafe {
            // 全小写形态同样命中（两侧统一小写比较）
            assert!(import_name_matches(base, 0, TARGET_FN));
            // 尾部多一个字符：不中（必须恰好在 NUL 结束）
            buf[2 + lower.len()] = b'X';
            assert!(!import_name_matches(base, 0, TARGET_FN));
            buf[2 + lower.len()] = 0;
            // 单字符差异：不中
            buf[2] = b'X';
            assert!(!import_name_matches(base, 0, TARGET_FN));
        }
    }

    /// 自检锚点存在性：本 DLL 导入表经 install() 计入 self_slots
    /// （CI 上以状态输出验证 >= 1；此处钉常量与外部 API 名长度一致：
    /// "SHGetPropertyStoreForWindow" = 27 字符）。
    #[test]
    fn self_import_anchor_declared() {
        assert_eq!(TARGET_FN.len(), 27);
    }
}
