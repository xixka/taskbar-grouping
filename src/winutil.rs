//! 窗口与字符串工具（任务 5 起使用）。

use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RPC_E_CHANGED_MODE};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowLongW, GetWindowTextW, GetWindowThreadProcessId,
    IsWindowVisible, GWL_EXSTYLE, WS_EX_TOOLWINDOW, WNDENUMPROC,
};

/// `str` → 以 NUL 结尾的 UTF-16 缓冲。
pub(crate) fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 把 API 写入 `buf` 的前 `len` 个 UTF-16 码元转成 `String`。
pub(crate) fn wide_buf_to_string(buf: &[u16], len: i32) -> String {
    if len <= 0 {
        return String::new();
    }
    let end = (len as usize).min(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// `PWSTR`（调用方负责以 `CoTaskMemFree` 释放）→ `String`。
pub(crate) unsafe fn pwstr_to_string(pw: windows::core::PWSTR) -> String {
    if pw.0.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    while *pw.0.add(len) != 0 {
        len += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(pw.0, len))
}

/// 展示用：空 AUMID 显示为 `<empty>`。
pub(crate) fn shown_aumid(s: &str) -> &str {
    if s.is_empty() {
        "<empty>"
    } else {
        s
    }
}

/// COM 初始化守卫：STA；若本线程已用其他模式初始化（RPC_E_CHANGED_MODE）
/// 则沿用现状、不负责反初始化。
pub(crate) struct ComGuard {
    owned: bool,
}

impl ComGuard {
    pub(crate) fn init() -> Result<Self, String> {
        unsafe {
            match CoInitializeEx(None, COINIT_APARTMENTTHREADED) {
                Ok(()) => Ok(ComGuard { owned: true }),
                Err(e) if e.code() == RPC_E_CHANGED_MODE => Ok(ComGuard { owned: false }),
                Err(e) => Err(format!("CoInitializeEx failed: {e}")),
            }
        }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.owned {
            unsafe { CoUninitialize() };
        }
    }
}

pub(crate) unsafe fn window_text(hwnd: HWND) -> String {
    let mut buf = [0u16; 512];
    let n = GetWindowTextW(hwnd, &mut buf[..]);
    wide_buf_to_string(&buf, n)
}

pub(crate) unsafe fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = GetClassNameW(hwnd, &mut buf[..]);
    wide_buf_to_string(&buf, n)
}

pub(crate) unsafe fn window_pid(hwnd: HWND) -> u32 {
    let mut pid: u32 = 0;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    pid
}

/// 是否像“任务栏上会出现按钮”的应用主窗口：可见 + 有标题 + 非工具窗口。
pub(crate) unsafe fn is_app_window(hwnd: HWND) -> bool {
    if !IsWindowVisible(hwnd).as_bool() {
        return false;
    }
    if window_text(hwnd).is_empty() {
        return false;
    }
    let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
    ex & WS_EX_TOOLWINDOW.0 == 0
}

unsafe extern "system" fn collect_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let out = &mut *(lparam.0 as *mut Vec<HWND>);
    out.push(hwnd);
    BOOL(1)
}

/// 枚举所有顶层窗口（不过滤）。
pub(crate) unsafe fn enum_top_level_windows() -> Vec<HWND> {
    let mut out: Vec<HWND> = Vec::new();
    let lparam = LPARAM(&mut out as *mut Vec<HWND> as isize);
    let _ = EnumWindows(Some(collect_cb), lparam);
    out
}

/// 解析 HWND 参数（十六进制，可带 0x 前缀）。
pub(crate) fn parse_hwnd(s: &str) -> Result<HWND, String> {
    let t = s.trim().trim_start_matches("0x").trim_start_matches("0X");
    isize::from_str_radix(t, 16)
        .map(HWND)
        .map_err(|_| format!("invalid HWND '{s}' (expected hex, e.g. 0x00000000010C12A8)"))
}

pub(crate) fn hwnd_hex(hwnd: HWND) -> String {
    format!("{:X}", hwnd.0)
}
