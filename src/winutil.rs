//! 窗口与字符串工具（任务 5 起使用）。

use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RPC_E_CHANGED_MODE};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetAncestor, GetClassNameW, GetWindowLongW, GetWindowTextW,
    GetWindowThreadProcessId, IsWindowVisible, GA_ROOT, GWL_EXSTYLE, WS_EX_TOOLWINDOW,
};

/// 把 API 写入 `buf` 的前 `len` 个 UTF-16 码元转成 `String`。
pub(crate) fn wide_buf_to_string(buf: &[u16], len: i32) -> String {
    if len <= 0 {
        return String::new();
    }
    let end = (len as usize).min(buf.len());
    String::from_utf16_lossy(&buf[..end])
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
            // windows 0.58：CoInitializeEx 返回裸 HRESULT（S_OK / S_FALSE 均视为本方持有）
            let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            if hr.is_ok() {
                Ok(ComGuard { owned: true })
            } else if hr == RPC_E_CHANGED_MODE {
                Ok(ComGuard { owned: false })
            } else {
                Err(format!("CoInitializeEx failed: {hr}"))
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

/// 是否像“任务栏上会出现按钮”的应用主窗口：顶层 + 可见 + 有标题 + 非工具窗口。
/// 顶层校验用 GetAncestor(GA_ROOT)：winevent 会为子控件也派发事件，
/// 必须排除（EnumWindows 枚举路径下该校验是空开销的无损操作）。
pub(crate) unsafe fn is_app_window(hwnd: HWND) -> bool {
    if GetAncestor(hwnd, GA_ROOT).0 != hwnd.0 {
        return false;
    }
    if !IsWindowVisible(hwnd).as_bool() {
        return false;
    }
    if window_text(hwnd).is_empty() {
        return false;
    }
    let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
    ex & WS_EX_TOOLWINDOW.0 == 0
}

/// shell 自身 UI 窗口的类名（桌面 / 任务栏）。这些窗口永不参与 AUMID 改写
/// （任务 6：winevent 事件里必须排除，否则会误改桌面/任务栏自身）。
const SHELL_WINDOW_CLASSES: [&str; 5] = [
    "Progman",                      // 桌面
    "WorkerW",                      // 桌面后备工作窗口
    "Shell_TrayWnd",                // 主任务栏
    "Shell_SecondaryTrayWnd",       // 副显示器任务栏
    "XamlExplorerHostIslandWindow", // Win11 任务栏宿主
];

/// 是否 shell 自身 UI 窗口（桌面/任务栏等）。类名比较不区分大小写
/// （Win32 窗口类注册本身即不区分大小写）。
pub(crate) unsafe fn is_shell_window(hwnd: HWND) -> bool {
    let cls = class_name(hwnd);
    SHELL_WINDOW_CLASSES
        .iter()
        .any(|c| cls.eq_ignore_ascii_case(c))
}

/// DWM 遮蔽（cloak）检测：被 cloak 的窗口（如挂起的 UWP）当前没有任务栏按钮，
/// 任务 6 中跳过不处理；查询失败按“未 cloak”处理。
/// 参考：DWMWA_CLOAKED 返回 DWM_CLOAKED_APP / DWM_CLOAKED_SHELL 标志位。
pub(crate) unsafe fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked: u32 = 0;
    let ok = DwmGetWindowAttribute(
        hwnd,
        DWMWA_CLOAKED,
        &mut cloaked as *mut u32 as *mut core::ffi::c_void,
        std::mem::size_of::<u32>() as u32,
    )
    .is_ok();
    ok && cloaked != 0
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
/// windows 0.58 的 HWND 是指针包装，需经 usize 中转构造。
pub(crate) fn parse_hwnd(s: &str) -> Result<HWND, String> {
    let t = s.trim().trim_start_matches("0x").trim_start_matches("0X");
    isize::from_str_radix(t, 16)
        .map(|v| HWND(v as usize as *mut core::ffi::c_void))
        .map_err(|_| format!("invalid HWND '{s}' (expected hex, e.g. 0x00000000010C12A8)"))
}

pub(crate) fn hwnd_hex(hwnd: HWND) -> String {
    format!("{:X}", hwnd.0 as usize)
}
