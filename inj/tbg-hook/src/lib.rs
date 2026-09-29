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

use windows::core::{E_POINTER, HRESULT};
use windows::Win32::Foundation::{BOOL, HINSTANCE, HMODULE, HWND};
use windows::Win32::System::LibraryLoader::{DisableThreadLibraryCalls, FreeLibraryAndExitThread};
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
/// `SHGetPropertyStoreForWindow` 的原始地址（IAT 槽里补丁前的值）。
static ORIGINAL_FN: AtomicUsize = AtomicUsize::new(0);

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

    // 2) 扫描 + 重定向（记录原始指针供桩函数调用）。
    let stub_ptr: GetStoreFn = stub_get_store;
    let ok = iat::install(stub_ptr as usize, ORIGINAL_FN.as_ptr(), s);
    if !ok {
        (*s).state.store(STATE_ERROR, Ordering::Release);
        return INIT_HOOK_FAIL;
    }
    if (*s).slots_patched == 0 {
        // 全进程无人静态导入目标函数：拦截面为 0（plan v2 §5 已知限制①，
        // 状态显式暴露而非静默"成功"）。
        (*s).err = tbg_proto::ERR_NO_SLOTS;
        (*s).state.store(STATE_ERROR, Ordering::Release);
        return INIT_HOOK_FAIL;
    }

    (*s).state.store(STATE_ACTIVE, Ordering::Release);
    INIT_OK
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
    FreeLibraryAndExitThread(hmod, STOP_OK);
    // FreeLibraryAndExitThread 不返回；显式收尾值仅为满足返回类型。
    STOP_OK
}
