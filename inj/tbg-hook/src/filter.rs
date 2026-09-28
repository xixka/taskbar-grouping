//! 窗口过滤与 AUMID 改写（任务 34）。
//!
//! **隔离复刻说明**：标记语义（`~TBG~w<HWND>` 后缀、129 码元上限、
//! `TBG.Group.<NAME>` 共享值）与主包 `src/appid.rs` 一致——但按
//! AGENTS.md 红线，`inj/` 成员不引用主包代码（防耦合渗透），此处为
//! 经 CI 断言对齐的独立实现。
//!
//! 过滤与主包 `src/winutil.rs::is_app_window` 的差异（有意为之）：
//! 省略窗口标题非空检查——hook 读路径运行在任务栏线程内，
//! `GetWindowTextW` 对跨进程窗口可能同步发 `WM_GETTEXT`，
//! 会拖慢任务栏分组查询；root / visible / toolwindow / shell 类名
//! 四项过滤保留，语义足够（任务栏本身只对候选窗口查 AUMID）。

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetClassNameW, GetWindowLongW, IsWindowVisible, GA_ROOT, GWL_EXSTYLE,
    WS_EX_TOOLWINDOW,
};

use tbg_proto::{SharedState, MODE_GROUP, MODE_UNGROUP};

/// 与主包 appid.rs 同值的标记与上限（对齐决策见模块注释）。
pub(crate) const SUFFIX_MARKER: &str = "~TBG~w";
pub(crate) const AUMID_MAX_LEN: usize = 129;
const GROUP_PREFIX: &str = "TBG.Group.";

/// shell 自身 UI 窗口的类名（src/winutil.rs 同表复刻）。
const SHELL_WINDOW_CLASSES: [&str; 5] = [
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "XamlExplorerHostIslandWindow",
];

/// 读路径窗口过滤（模块注释说明与主包的差异）。
pub(crate) unsafe fn should_serve(hwnd: HWND) -> bool {
    if GetAncestor(hwnd, GA_ROOT).0 != hwnd.0 {
        return false;
    }
    if !IsWindowVisible(hwnd).as_bool() {
        return false;
    }
    let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
    if ex & WS_EX_TOOLWINDOW.0 != 0 {
        return false;
    }
    let mut buf = [0u16; 64];
    let n = GetClassNameW(hwnd, &mut buf[..]);
    if n <= 0 {
        return false;
    }
    let cls = String::from_utf16_lossy(&buf[..n as usize]);
    !SHELL_WINDOW_CLASSES
        .iter()
        .any(|c| cls.eq_ignore_ascii_case(c))
}

/// 按共享内存策略改写 AUMID 值（纯函数，可单测）。
///
/// - `MODE_UNGROUP`：原值（可能为空）+ `~TBG~w<HWND 大写十六进制>`，
///   总长超 129 码元时截原值保后缀（与 appid.rs `suffixed_aumid` 同
///   语义：空原值 → 仅后缀，空窗口天然独立成组）；
/// - `MODE_GROUP`：固定返回共享 AUMID（`TBG.Group.<NAME>`，宿主写入
///   `group_aumid`，缺省回退 `TBG.Group.default`）。
pub(crate) fn rewrite_aumid(orig: &str, hwnd: isize, s: &SharedState) -> String {
    if s.mode == MODE_GROUP {
        let shared = s.group_aumid_string();
        if shared.is_empty() {
            format!("{GROUP_PREFIX}default")
        } else {
            shared
        }
    } else {
        suffixed(orig, hwnd)
    }
}

/// 线路一标记（appid.rs `suffixed_aumid` 隔离复刻）。
///
/// 文档口径 AUMID 字符集为 ASCII（`[A-Za-z0-9.\-]`），此处按字节计数
/// 即码元计数；仍以 `char_indices` 逐字符截断防御非法输入——explorer
/// 进程内任何 panic 都是进程级故障，无 unwind 可言。
fn suffixed(orig: &str, hwnd: isize) -> String {
    let hex = format!("{:X}", hwnd as usize);
    let keep = AUMID_MAX_LEN
        .saturating_sub(SUFFIX_MARKER.len() + hex.len())
        .min(orig.len());
    let mut out = String::with_capacity(keep + SUFFIX_MARKER.len() + hex.len());
    for (i, ch) in orig.char_indices() {
        if i >= keep {
            break;
        }
        out.push(ch);
    }
    out.push_str(SUFFIX_MARKER);
    out.push_str(&hex);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;
    use tbg_proto::{MODE_UNGROUP, SharedState};

    fn proto(mode: u32, group: &str) -> SharedState {
        let mut s = SharedState::new_default();
        s.mode = mode;
        if !group.is_empty() {
            let wide: Vec<u16> = group.encode_utf16().collect();
            s.group_aumid[..wide.len()].copy_from_slice(&wide);
        }
        s
    }

    #[test]
    fn ungroup_suffix_basic() {
        let s = proto(MODE_UNGROUP, "");
        assert_eq!(rewrite_aumid("Microsoft.Notepad", 0xFF, &s), "Microsoft.Notepad~TBG~wFF");
        // 空原值：仅后缀（空 AUMID 窗口也独立成组）
        assert_eq!(rewrite_aumid("", 0x1A, &s), "~TBG~w1A");
    }

    #[test]
    fn ungroup_truncation() {
        let s = proto(MODE_UNGROUP, "");
        let long = "a".repeat(200);
        let out = rewrite_aumid(&long, 0xDEADBEEF, &s);
        assert!(out.len() <= 129, "129 码元上限被突破");
        assert!(out.ends_with("~TBG~wDEADBEEF"));
        let keep = 129 - "~TBG~w".len() - 8;
        assert_eq!(&out[..keep], "a".repeat(keep));
    }

    #[test]
    fn group_mode_shared_aumid() {
        let s = proto(MODE_GROUP, "TBG.Group.smoke");
        assert_eq!(rewrite_aumid("Microsoft.Notepad", 7, &s), "TBG.Group.smoke");
        assert_eq!(rewrite_aumid("", 8, &s), "TBG.Group.smoke");
        // 缺省回退
        let s2 = proto(MODE_GROUP, "");
        assert_eq!(rewrite_aumid("x", 9, &s2), "TBG.Group.default");
    }

    #[test]
    fn marker_constants_match_main_edition() {
        // 与主包 src/appid.rs 对齐（隔离复刻的锚点）
        assert_eq!(SUFFIX_MARKER, "~TBG~w");
        assert_eq!(AUMID_MAX_LEN, 129);
    }

    #[test]
    fn proto_state_field_alive() {
        // 防 SharedState 字段被意外移除导致编译漂移
        let s = proto(MODE_UNGROUP, "");
        assert_eq!(s.state.load(std::sync::atomic::Ordering::Relaxed), 0);
        let _x: &AtomicU32 = &s.enabled;
    }
}
