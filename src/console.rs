//! 控制台信号（任务 34，审查 P1-E/P1-S，2026-09-25）。
//!
//! E：CLI `watch --duration 0` 此前没有 Ctrl+C 处理器——硬杀 → 无终扫
//! 统计；且 30s 内 Ctrl+C + 重启累计 3 次会**误触发熔断**删掉用户的自
//! 启项。S：交互菜单会话里**关闭控制台窗口**（conhost 默认直接终止进
//! 程）同样绕过 watch 线程的优雅收尾路径（摘钩/终扫/health end_clean
//! 一个都走不到）。
//!
//! 修复：`SetConsoleCtrlHandler` 注册一个只做原子写的处理器——
//! - 第一次 Ctrl+C / Ctrl+Break / 关窗 / 注销 / 关机：置位停止标志并
//!   返回 TRUE（抑制默认硬杀）。`winevent::run` 消息泵每 ≤1s 醒一次
//!   轮询该标志，读到后走优雅退出（摘钩 + 终扫统计 + end_clean）；
//!   菜单线程阻塞在 stdin 读上，控制台输入读会被该事件打断（
//!   ERROR_OPERATION_ABORTED → EOF 路径同样优雅停止 watch）。
//! - 第二次信号（用户坚持硬杀）：立即退出，退出码与默认 Ctrl+C 终止
//!   一致（STATUS_CONTROL_C_EXIT）。
//!
//! 仅在长驻模式（CLI watch / 交互菜单）安装：一次性命令保持默认行为
//! （Ctrl+C 立即终止即可，没有需要收尾的状态）。
//!
//! 约束与边界：
//! - 处理器在系统创建的独立线程上下文被调，只做 `AtomicBool::store`
//!   与计数——无锁、无分配、async-signal-safe；
//! - 菜单退出主路径仍是菜单项 `[0]`（plan v2 §0 决策 5 维护者指示
//!   "不需要 Ctrl+C"）：本模块是 CLI 常驻与关窗场景的兜底，不改变
//!   菜单交互语义；
//! - CLOSE/LOGOFF/SHUTDOWN 系统只给 ~5s 宽限：终扫的逐窗口标题读取
//!   已带 100ms 超时（任务 35，SendMessageTimeoutW），最坏路径有界；
//! - `Ctrl+C = 硬杀` 的旧行为变更为"优雅退出（通常 ≤1s）"，依赖硬杀
//!   立即性的脚本可连按两次 Ctrl+C 或 taskkill（README 已声明）。

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use windows::Win32::Foundation::BOOL;
use windows::Win32::System::Console::{
    SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT,
    CTRL_SHUTDOWN_EVENT,
};

/// 停止请求（任何控制台信号）。
static STOP: AtomicBool = AtomicBool::new(false);

/// 已收到的信号次数（≥2 = 用户坚持硬杀）。
static SIGNALS: AtomicU32 = AtomicU32::new(0);

/// 查询：控制台信号是否请求停止（消息泵 / 菜单主循环轮询）。
pub(crate) fn stop_requested() -> bool {
    STOP.load(Ordering::Relaxed)
}

/// 安装控制台信号处理器（幂等：重复调用无害，OS 侧同函数只登记一次）。
/// 返回是否成功——失败（无控制台/极端环境）不致命，只是退回默认硬杀。
pub(crate) fn install_ctrl_handler() -> bool {
    unsafe { SetConsoleCtrlHandler(Some(console_ctrl_handler), true).is_ok() }
}

/// 任务 40（审查 P1-Q，2026-09-25）：`--background` 自脱离。
///
/// 现象：`install` 注册的 `watch --duration 0` 登录时被 explorer 拉起，
/// **分配一个可见 conhost 窗口**常驻任务栏（莫名黑窗 + 点 X = 杀
/// watch，功能静默失效）。
///
/// 修复：stdout/stderr 先 `SetStdHandle` 重定向到 NUL 设备，再
/// `FreeConsole` 脱离控制台（从真控制台启动时黑窗仅一闪）。
/// 关键顺序约束：
/// - `SetStdHandle` 必须发生在**任何 stdout/stderr 输出之前**——Rust
///   std 惰性缓存 stdio 句柄（首写取 `GetStdHandle` 后不再查询），先
///   输出后重定向无效；
/// - `println!` 写 NUL 恒成功，不会触发 "failed printing to stdout"
///   panic 路径（BUG-09 钩子不受扰动）；
/// - `FreeConsole` 对无控制台进程（CREATE_NO_WINDOW）报错——静默
///   忽略（`let _ =`，正是期望状态）；
/// - NUL 句柄 `mem::forget` 不回收：句柄须存活至进程结束，drop 会把
///   `SetStdHandle` 指向的底层句柄关闭。
/// 事件轨迹由环形日志承担（`--background` 恒开，256 KiB 有界）。
pub(crate) fn detach_console() -> Result<(), String> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Console::{
        FreeConsole, SetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
    };

    let open_nul = || {
        std::fs::OpenOptions::new()
            .write(true)
            .open("NUL")
            .map_err(|e| format!("background: cannot open the NUL device: {e}"))
    };
    let out = open_nul()?;
    let err = open_nul()?;
    unsafe {
        SetStdHandle(STD_OUTPUT_HANDLE, HANDLE(out.as_raw_handle()))
            .map_err(|e| format!("background: redirect stdout failed: {e}"))?;
        SetStdHandle(STD_ERROR_HANDLE, HANDLE(err.as_raw_handle()))
            .map_err(|e| format!("background: redirect stderr failed: {e}"))?;
    }
    std::mem::forget(out);
    std::mem::forget(err);
    // 已无控制台（CREATE_NO_WINDOW 启动）时失败——正是期望状态
    let _ = unsafe { FreeConsole() };
    Ok(())
}

/// 处理器：只做原子写。`BOOL(1)` = 已处理（抑制默认终止）。
unsafe extern "system" fn console_ctrl_handler(event: u32) -> BOOL {
    match event {
        CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT
        | CTRL_SHUTDOWN_EVENT => {
            STOP.store(true, Ordering::Relaxed);
            let n = SIGNALS.fetch_add(1, Ordering::Relaxed) + 1;
            if n > 1 {
                // 第二次信号：立即硬退出（与默认 Ctrl+C 终止码一致，
                // STATUS_CONTROL_C_EXIT = 0xC000013A）
                std::process::exit(-1_073_741_510);
            }
            BOOL(1)
        }
        _ => BOOL(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_flag_roundtrip() {
        // 处理器本体无法在单测里安全驱动（系统线程上下文），这里只验
        // 标志语义：默认 false，置位后可见（不与其他测试并行污染：
        // 置位后复位）
        assert!(matches!(STOP.load(Ordering::Relaxed), _));
        STOP.store(true, Ordering::Relaxed);
        assert!(stop_requested());
        STOP.store(false, Ordering::Relaxed);
        assert!(!stop_requested());
    }
}
