//! `tbg_hook.dll` —— 路线 A 进程内组件（任务 34，docs/plan.md v2 §5）。
//!
//! 由宿主 `tbg-inject.exe` 经 `CreateRemoteThread + LoadLibraryW` 装入
//! explorer，再远程调用导出 [`tbg_hook_init`] 完成安装；卸载走导出
//! [`tbg_hook_stop`]（摘钩 → 宽限 → `FreeLibraryAndExitThread`）。
//!
//! **DllMain 纪律**：加载器锁下只做 `DisableThreadLibraryCalls` 与
//! 记录自身基址，一切实际初始化（打开共享节、扫 IAT、打补丁）都在
//! 宿主远程线程驱动的 `tbg_hook_init` 里做——这是本设计相对
//! "DllMain 里 CreateThread" 的关键安全性决策。
//!
//! **拦截面**（清室设计，零私有符号）：遍历本进程模块导入表，把
//! `shell32.dll!SHGetPropertyStoreForWindow` 的 IAT 槽重定向到
//! [`stub_get_store`]；桩函数调原函数后，把返回的 `IPropertyStore`
//! 包进委托对象（`wrap` 模块），仅对 `PKEY_AppUserModel_ID` 的
//! GetValue 按共享内存里的策略改写（线路一后缀 / 线路二共享值），
//! 其余键与 SetValue/Commit 原样透传。

#![cfg(windows)]

mod filter;
mod iat;
mod wrap;

use std::ffi::c_void;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

// 0.58 实源核对：E_POINTER 在 Win32::Foundation（非 core）；IID 关联常量
// 需 Interface trait 在作用域内。
use windows::core::{HRESULT, Interface, PCSTR};
use windows::Win32::Foundation::{BOOL, E_POINTER, FARPROC, HINSTANCE, HMODULE, HWND};
use windows::Win32::System::LibraryLoader::{
    DisableThreadLibraryCalls, FreeLibraryAndExitThread, GetModuleFileNameW, LoadLibraryW,
};
use windows::Win32::System::Memory::{MapViewOfFile, OpenFileMappingW, FILE_MAP_READ, FILE_MAP_WRITE};
use windows::Win32::System::Threading::{GetCurrentProcessId, Sleep};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::core::GUID;

use tbg_proto::{
    SharedState, INIT_ALREADY, INIT_BAD_PROTO, INIT_HOOK_FAIL, INIT_NO_SECTION, INIT_OK, MAGIC,
    PROTO_VERSION, STATE_ACTIVE, STATE_ERROR, STATE_INITING, STATE_STOPPING, STATE_UNLOADED,
    STOP_NOT_ACTIVE, STOP_OK,
};

// DllMain reason 常量（winuser 加载通知；手写数值避免引入额外 feature）
const DLL_PROCESS_ATTACH: u32 = 1;
const DLL_PROCESS_DETACH: u32 = 0;

/// 本 DLL 在本进程的加载基址（DllMain 记录）。
static SELF_MODULE: AtomicUsize = AtomicUsize::new(0);
/// 共享内存映射视图（init 成功后有效）。
static SHARED: AtomicPtr<SharedState> = AtomicPtr::new(std::ptr::null_mut());
/// `SHGetPropertyStoreForWindow` 的原始地址（导出表解析，见 iat::install）。
static ORIGINAL_FN: AtomicUsize = AtomicUsize::new(0);
/// `GetProcAddress` 的原始地址（导出表解析，见 iat::install）。
static GPA_ORIGINAL: AtomicUsize = AtomicUsize::new(0);

// 自检锚点：本 DLL 自带目标函数的静态导入（仅取址引用，永不调用）。
// init 扫描自身模块计入 self_slots——非 0 即证明扫描器与 PE 导入表
// 解析工作正常（任务 36 修复轮 2 的扫描器自检）。`#[link(name=...)]`
// 显式挂 shell32 导入库——本 DLL 别处不经 windows crate 链接 shell32，
// 缺此属性则锚点符号链接期未解析。
#[link(name = "shell32")]
extern "system" {
    #[link_name = "SHGetPropertyStoreForWindow"]
    fn _tbg_self_import_anchor(hwnd: HWND, riid: *const GUID, ppv: *mut *mut c_void) -> HRESULT;
}
#[used]
static _SELF_IMPORT: unsafe extern "system" fn(HWND, *const GUID, *mut *mut c_void) -> HRESULT =
    _tbg_self_import_anchor;

#[inline]
fn shared_ref() -> Option<&'static SharedState> {
    let p = SHARED.load(Ordering::Acquire);
    if p.is_null() {
        None
    } else {
        // SAFETY: p 在 init 成功后指向有效映射，本 DLL 卸载前不变。
        Some(unsafe { &*p })
    }
}

/// 标准入口：仅记录基址 + 禁用线程通知（见模块注释的 DllMain 纪律）。
#[no_mangle]
unsafe extern "system" fn DllMain(hinst: HINSTANCE, reason: u32, _reserved: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        let _ = DisableThreadLibraryCalls(HMODULE(hinst.0));
        SELF_MODULE.store(hinst.0 as usize, Ordering::Relaxed);
    } else if reason == DLL_PROCESS_DETACH {
        // 兜底：未经 stop 的外部卸载（正常进程退出路径）。此时补丁若
        // 仍在，槽位将指向即将解除映射的代码——尽力恢复。
        if let Some(s) = shared_ref() {
            if s.state.load(Ordering::Acquire) == STATE_ACTIVE {
                iat::remove();
            }
        }
    }
    BOOL(1)
}

/// `SHGetPropertyStoreForWindow` 的原始签名（文档化原型）。
type GetStoreFn = unsafe extern "system" fn(HWND, *const GUID, *mut *mut c_void) -> HRESULT;
/// `GetProcAddress` 的原始签名（文档化原型）。
type GpaFn = unsafe extern "system" fn(HMODULE, PCSTR) -> FARPROC;

/// `GetProcAddress` 的 IAT 桩（任务 36 修复轮 2 第三层拦截面）：一切
/// 查询透传原函数；仅当查询名恰为目标函数且原调用可解析时，返回
/// [`stub_get_store`]——覆盖任务栏运行时动态解析（含 delay-load
/// helper 经被补丁模块 IAT 的内部解析：helper 把我们的桩写入 delay
/// 槽，后续调用直达桩）。ordinal 伪指针（<64K）与空指针不做字符串
/// 比较，直接透传。
unsafe extern "system" fn stub_get_proc_address(hmod: HMODULE, name: PCSTR) -> FARPROC {
    let orig = GPA_ORIGINAL.load(Ordering::Acquire);
    if orig == 0 {
        return None; // 补丁未就绪（理论不可达：install 先解析再落补丁）
    }
    let f: GpaFn = std::mem::transmute(orig);
    let r = f(hmod, name);
    let p = name.0 as usize;
    if p >= 0x1_0000 {
        // ANSI 名（非 ordinal 伪指针）：与目标名两侧统一小写比较
        let b = p as *const u8;
        let mut eq = true;
        for k in 0..iat::TARGET_FN.len() {
            if (*b.add(k)).to_ascii_lowercase() != iat::TARGET_FN[k] {
                eq = false;
                break;
            }
        }
        if eq && *b.add(iat::TARGET_FN.len()) == 0 && r.is_some() {
            let stub: GetStoreFn = stub_get_store;
            return Some(std::mem::transmute::<
                GetStoreFn,
                unsafe extern "system" fn() -> isize,
            >(stub));
        }
    }
    r
}

/// IAT 重定向目标：调原函数，成功时按策略包装返回的属性存储。
unsafe extern "system" fn stub_get_store(
    hwnd: HWND,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    let orig = ORIGINAL_FN.load(Ordering::Acquire);
    if orig == 0 {
        // 补丁已摘 / 未初始化完成——理论不可达，保守返回 E_POINTER。
        return E_POINTER;
    }
    let f: GetStoreFn = std::mem::transmute(orig);
    let hr = f(hwnd, riid, ppv);

    if let Some(s) = shared_ref() {
        s.calls_seen.fetch_add(1, Ordering::Relaxed);
    }
    // 只包装 IPropertyStore 请求（其余 riid 原样放行，杜绝接口错配）。
    if hr.is_ok()
        && !ppv.is_null()
        && !(*ppv).is_null()
        && !riid.is_null()
        && *riid == IPropertyStore::IID
    {
        if let Some(s) = shared_ref() {
            let serve = s.state.load(Ordering::Acquire) == STATE_ACTIVE
                && s.enabled.load(Ordering::Acquire) == 1
                && filter::should_serve(hwnd);
            if serve {
                *ppv = wrap::proxy_new(*ppv, hwnd, s as *const SharedState);
                s.stores_wrapped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    hr
}

/// 宿主远程线程入口：安装钩子。见模块注释（DllMain 纪律）。
#[no_mangle]
pub unsafe extern "system" fn tbg_hook_init(_param: *mut c_void) -> u32 {
    let cur_pid = GetCurrentProcessId();
    // 幂等：同一 explorer 内重复 init（宿主侧已挡，此处双保险）。
    if let Some(s) = shared_ref() {
        if s.state.load(Ordering::Acquire) == STATE_ACTIVE
            && s.explorer_pid.load(Ordering::Relaxed) == cur_pid
        {
            return INIT_ALREADY;
        }
    }

    // 1) 打开宿主创建的共享节（宿主在注入前已写入配置）。
    let name_w: Vec<u16> = tbg_proto::SECTION_NAME
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let name = windows::core::PCWSTR::from_raw(name_w.as_ptr());
    // OpenFileMappingW 0.58 签名：第一参数是裸 u32（非 FILE_MAP 旗标）
    let h = match OpenFileMappingW(FILE_MAP_READ.0 | FILE_MAP_WRITE.0, false, name) {
        Ok(h) => h,
        Err(_) => return INIT_NO_SECTION,
    };
    // 0.58 签名：MapViewOfFile 返回 MEMORY_MAPPED_VIEW_ADDRESS（失败 .Value 为 null）
    let view = MapViewOfFile(h, FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, 0);
    if view.Value.is_null() {
        let _ = windows::Win32::Foundation::CloseHandle(h);
        return INIT_NO_SECTION;
    }
    let base = view.Value as *mut SharedState;
    // MapView 已持节存活；句柄可关（宿主持有的句柄与本视图都保节）。
    let _ = windows::Win32::Foundation::CloseHandle(h);

    let s = base;
    if (*s).magic != MAGIC || (*s).proto != PROTO_VERSION {
        return INIT_BAD_PROTO;
    }

    SHARED.store(s, Ordering::Release);
    (*s).explorer_pid.store(cur_pid, Ordering::Relaxed);
    (*s).state.store(STATE_INITING, Ordering::Release);
    (*s).err = tbg_proto::ERR_NONE;
    (*s).generation.fetch_add(1, Ordering::Relaxed);
    // 64 位 HMODULE 无法经 32 位线程退出码回传——自报基址。
    (*s).self_module = SELF_MODULE.load(Ordering::Relaxed);
    // 新一代注入：统计清零（explorer 重启后的全新进程）。
    (*s).calls_seen.store(0, Ordering::Relaxed);
    (*s).stores_wrapped.store(0, Ordering::Relaxed);
    (*s).aumid_served.store(0, Ordering::Relaxed);

    // 2) 扫描 + 重定向（三层拦截面：普通 IAT / delay-load / GetProcAddress；
    //    原始函数地址由 install 从导出表确定性解析）。
    let stub_ptr: GetStoreFn = stub_get_store;
    let gpa_ptr: GpaFn = stub_get_proc_address;
    let ok = iat::install(
        stub_ptr as usize,
        gpa_ptr as usize,
        ORIGINAL_FN.as_ptr(),
        GPA_ORIGINAL.as_ptr(),
        s,
    );
    if !ok {
        (*s).state.store(STATE_ERROR, Ordering::Release);
        return unload_self(INIT_HOOK_FAIL);
    }
    if (*s).slots_patched == 0 && (*s).gpa_slots == 0 {
        // 三层拦截面全部为 0：无任何可重定向入口（plan v2 §5 已知限制①，
        // 状态显式暴露而非静默"成功"；self_slots 此时亦为 0 = 扫描器
        // 自身异常，一并暴露于诊断面板）。
        (*s).err = tbg_proto::ERR_NO_SLOTS;
        (*s).state.store(STATE_ERROR, Ordering::Release);
        return unload_self(INIT_HOOK_FAIL);
    }

    (*s).state.store(STATE_ACTIVE, Ordering::Release);

    // 自钉扎（任务 36 修复轮 4）：实测宿主进程退出后本 DLL 会从 explorer
    // 中消失（机制待诊断：远程 LoadLibraryW 的进程级引用计数理论上应
    // 保持常驻——run 36525398321：ACTIVE + patched=5 + explorer pid 未变，
    // 下一次宿主却看到全新节）。对自身再 LoadLibraryW 一次（+1 引用）：
    // 未知路径的单次递减不再致命；stop 走两段释放仍可干净卸载。
    // 诊断证据（inject 后模块表×2 + 事件日志）见 Phase INJ 插桩。
    let hmod = HMODULE(SELF_MODULE.load(Ordering::Relaxed) as *mut c_void);
    let mut path_buf = [0u16; 512];
    let n = GetModuleFileNameW(hmod, &mut path_buf);
    if n > 0 && (n as usize) < path_buf.len() {
        let _ = LoadLibraryW(windows::core::PCWSTR::from_raw(path_buf.as_ptr()));
    }

    INIT_OK
}

/// init 失败路径的自清理：补丁未生效（无在途调用），直接摘除自身，
/// 以 `code` 作为远程线程退出码结束——宿主 GetExitCodeThread 取回同值，
/// explorer 内不残留失效 DLL。状态先写 ERROR 再卸载：宿主自己的视图
/// 保节存活，诊断面板仍可读到失败细节。
unsafe fn unload_self(code: u32) -> u32 {
    let hmod = HMODULE(SELF_MODULE.load(Ordering::Relaxed) as *mut c_void);
    FreeLibraryAndExitThread(hmod, code)
}

/// 宿主远程线程入口：摘钩 + 卸载（本函数不返回）。
#[no_mangle]
pub unsafe extern "system" fn tbg_hook_stop(_param: *mut c_void) -> u32 {
    let s = match shared_ref() {
        Some(s) => s,
        None => return STOP_NOT_ACTIVE,
    };
    if s.state.load(Ordering::Acquire) != STATE_ACTIVE {
        return STOP_NOT_ACTIVE;
    }
    s.state.store(STATE_STOPPING, Ordering::Release);
    // 先停改写再摘钩：在途 GetValue 快速回落原值。
    s.enabled.store(0, Ordering::Release);
    iat::remove();
    s.state.store(STATE_UNLOADED, Ordering::Release);
    // 宽限窗口：让可能位于本 DLL 代码内的在途调用返回（理论竞态，
    // plan v2 §5 已知限制②；业界同类工具多以常驻规避，本版选择完整卸载）。
    Sleep(1500);
    let hmod = HMODULE(SELF_MODULE.load(Ordering::Relaxed) as *mut c_void);
    // 卸载走单段 FreeLibraryAndExitThread（对任意计数安全：原子化
    // "递减 + 线程退出"，计数归零时卸载发生在本线程退出之后）。**不做**
    // 先行 FreeLibrary 再 FAET 的两段式——若外部发生过引用递减，先行
    // FreeLibrary 可能当场归零并解除映射，后续指令在死代码上执行。
    // 自钉扎的代价：正常路径（计数 2）下 FAET 后余 1，模块以"补丁已摘、
    // 状态 UNLOADED"的惰性形态驻留至 explorer 重启——功能正确（分组
    // 已回原生），残留为文档化的已知取舍。
    FreeLibraryAndExitThread(hmod, STOP_OK);
    // FreeLibraryAndExitThread 不返回；显式收尾值仅为满足返回类型。
    STOP_OK
}
