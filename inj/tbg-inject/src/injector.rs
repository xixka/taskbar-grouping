//! 注入器（任务 35，docs/plan.md v2 §5）。
//!
//! 三段式远程调用，全部走文档化 API，无内存代码写入：
//! 1. `CreateRemoteThread(kernel32!LoadLibraryW, 远端路径串)` 装载
//!    `tbg_hook.dll`（DllMain 空载：仅 DisableThreadLibraryCalls + 记基址）；
//! 2. 宿主本地装载同一 DLL 取导出 RVA，Toolhelp 快照取 explorer 内
//!    装载基址，两者相加得远程函数地址（规避线程退出码 32 位截断）；
//! 3. `CreateRemoteThread(tbg_hook_init / tbg_hook_stop)` 驱动安装/卸载。

use std::ffi::c_void;
use std::path::Path;

use windows::core::{s, w, PCSTR, PCWSTR};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Module32FirstW, Module32NextW, MODULEENTRY32W, TH32CS_SNAPMODULE,
};
use windows::Win32::System::LibraryLoader::{
    FreeLibrary, GetModuleHandleW, GetProcAddress, LoadLibraryW,
};
use windows::Win32::System::Memory::{
    VirtualAllocEx, VirtualFreeEx, WriteProcessMemory, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE,
    PAGE_READWRITE,
};
use windows::Win32::System::Threading::{
    GetExitCodeThread, OpenProcess, WaitForSingleObject, CreateRemoteThread, LPTHREAD_START_ROUTINE,
    PROCESS_CREATE_THREAD, PROCESS_QUERY_INFORMATION, PROCESS_VM_OPERATION, PROCESS_VM_READ,
    PROCESS_VM_WRITE,
};
use windows::Win32::UI::WindowsAndMessaging::{GetShellWindow, GetWindowThreadProcessId};

/// explorer 进程号（shell 窗口反查；CI 会话与真机同路径）。
pub(crate) unsafe fn explorer_pid() -> Option<u32> {
    let shell = GetShellWindow();
    if shell.0.is_null() {
        return None;
    }
    let mut pid: u32 = 0;
    GetWindowThreadProcessId(shell, Some(&mut pid));
    if pid == 0 {
        None
    } else {
        Some(pid)
    }
}

type RemoteFn = unsafe extern "system" fn(*mut c_void) -> u32;

/// 在 explorer 里远程调用 `tbg_hook.dll` 的导出函数，返回其退出码。
///
/// `export` 为带 NUL 的导出名字节串（如 `b"tbg_hook_init\0"`）。
/// 本函数假定 DLL 已在（或即将经本函数第一步）装载于目标进程。
pub(crate) unsafe fn call_remote_export(
    pid: u32,
    dll: &Path,
    export: &[u8],
) -> Result<u32, String> {
    let h = OpenProcess(
        PROCESS_CREATE_THREAD | PROCESS_QUERY_INFORMATION | PROCESS_VM_OPERATION
            | PROCESS_VM_READ | PROCESS_VM_WRITE,
        false,
        pid,
    )
    .map_err(|e| format!("inject: OpenProcess(explorer {pid}) failed: {e}"))?;

    // 1) 远程 LoadLibraryW(dll 绝对路径)
    let path_w: Vec<u16> = dll
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let bytes = path_w.len() * 2;
    // 0.58 签名：VirtualAllocEx 返回裸指针（失败为 null，非 Result）
    let remote = VirtualAllocEx(h, None, bytes, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
    if remote.is_null() {
        return Err("inject: VirtualAllocEx failed (explorer memory)".into());
    }
    let mut written: usize = 0;
    WriteProcessMemory(
        h,
        remote,
        path_w.as_ptr() as *const c_void,
        bytes,
        Some(&mut written),
    )
    .map_err(|e| format!("inject: WriteProcessMemory failed: {e}"))?;

    let k32 = GetModuleHandleW(w!("kernel32.dll"))
        .map_err(|e| format!("inject: GetModuleHandleW(kernel32) failed: {e}"))?;
    let load_lib = GetProcAddress(k32, s!("LoadLibraryW"))
        .ok_or_else(|| "inject: resolve kernel32!LoadLibraryW failed".to_string())?;
    let start: LPTHREAD_START_ROUTINE = Some(std::mem::transmute::<
        unsafe extern "system" fn() -> isize,
        unsafe extern "system" fn(*mut c_void) -> u32,
    >(load_lib));

    let t = CreateRemoteThread(h, None, 0, start, Some(remote as *const c_void), 0, None)
        .map_err(|e| format!("inject: CreateRemoteThread(LoadLibraryW) failed: {e}"))?;
    let _ = WaitForSingleObject(t, 15_000);
    let _ = CloseHandle(t);
    let _ = VirtualFreeEx(h, remote, 0, MEM_RELEASE);

    // 2) 远程基址 + 本地 RVA → 远程函数地址
    let base = wait_remote_module(pid)?;
    let local = LoadLibraryW(PCWSTR::from_raw(path_w.as_ptr()))
        .map_err(|e| format!("inject: local LoadLibraryW({}) failed: {e}", dll.display()))?;
    let mut name_buf = export.to_vec();
    if name_buf.last() != Some(&0) {
        name_buf.push(0);
    }
    let proc = GetProcAddress(local, PCSTR::from_raw(name_buf.as_ptr()))
        .ok_or_else(|| format!("inject: export {} not found in tbg_hook.dll", String::from_utf8_lossy(&name_buf)))?;
    let rva = proc as usize - local.0 as usize;
    let _ = FreeLibrary(local);
    let remote_addr = base + rva;

    // 3) 远程调用导出
    let start2: LPTHREAD_START_ROUTINE = Some(std::mem::transmute::<
        usize,
        unsafe extern "system" fn(*mut c_void) -> u32,
    >(remote_addr));
    let t2 = CreateRemoteThread(h, None, 0, start2, None, 0, None)
        .map_err(|e| format!("inject: CreateRemoteThread(export) failed: {e}"))?;
    // stop 内含 1.5 s 宽限 + 卸载，等待窗必须覆盖
    let _ = WaitForSingleObject(t2, 15_000);
    let mut code: u32 = 0;
    GetExitCodeThread(t2, &mut code)
        .map_err(|e| format!("inject: GetExitCodeThread failed: {e}"))?;
    let _ = CloseHandle(t2);
    let _ = CloseHandle(h);
    Ok(code)
}

/// 轮询 explorer 模块表定位 `tbg_hook.dll` 基址（≤2 s）。
unsafe fn wait_remote_module(pid: u32) -> Result<usize, String> {
    for _ in 0..20 {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE, pid)
            .map_err(|e| format!("inject: snapshot(explorer modules) failed: {e}"))?;
        let mut me = MODULEENTRY32W {
            dwSize: std::mem::size_of::<MODULEENTRY32W>() as u32,
            ..Default::default()
        };
        let mut found = 0usize;
        if Module32FirstW(snap, &mut me).is_ok() {
            loop {
                let name = String::from_utf16_lossy(
                    &me.szModule[..me
                        .szModule
                        .iter()
                        .position(|&u| u == 0)
                        .unwrap_or(me.szModule.len())],
                );
                if name.eq_ignore_ascii_case("tbg_hook.dll") {
                    found = me.modBaseAddr as usize;
                    break;
                }
                if Module32NextW(snap, &mut me).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
        if found != 0 {
            return Ok(found);
        }
        windows::Win32::System::Threading::Sleep(100);
    }
    Err("inject: tbg_hook.dll not observed in explorer module list after remote LoadLibraryW".into())
}
