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

// ============ 任务 41（审查 P1-J，2026-09-25）：控制台 I/O 代码页 ============
//
// 现象：GBK 代码页（936，zh-CN 默认）控制台上，`println!` 的 UTF-8 字节
// 被按 GBK 解码——中文菜单/提示/窗口标题乱码；英文代码页（437/850）下
// 同样。任务 29 的双语交付在最需要它的 zh-CN 环境恰好不工作。输入侧
// `io::stdin().read_line` 要求 UTF-8，GBK 控制台输入中文 →
// `Err(InvalidData)` → 被当作 EOF → 菜单静默退出。
//
// 双管齐下：
// 1. **交互层（菜单）输出**走 `WriteConsoleW`——宽字符直写控制台，
//    **与代码页无关**（GBK/任何 CP 下中文都正确渲染）；管道/重定向
//    时保持 `println!` 语义（UTF-8 字节，CI/脚本消费者不变）。
//    经 `outln!`/`outp!` 宏使用（见文件底部）。
// 2. **入口代码页 UTF-8 化**（尽力而为）：stdout/stdin 是控制台时把
//    输入/输出 CP 切 65001、进程退出前恢复——覆盖其余 `println!` 路径
//    （inspect 表格中文标题、watch 事件行）与控制台输入（切 65001 后
//    `read_until` 拿到真 UTF-8 字节；即使切换失败，菜单输入也已改
//    lossy 读，非法序列不再误判 EOF——见 menu.rs `read_line`）。

use std::io::IsTerminal;

use windows::Win32::Globalization::CP_UTF8;
use windows::Win32::System::Console::{
    GetConsoleCP, GetConsoleOutputCP, SetConsoleCP, SetConsoleOutputCP, WriteConsoleW,
    STD_OUTPUT_HANDLE,
};

/// stdout 是否为交互控制台（管道/重定向/NUL → false）。
pub(crate) fn stdout_is_console() -> bool {
    std::io::stdout().is_terminal()
}

/// stderr 是否为交互控制台。
#[allow(dead_code)]
pub(crate) fn stderr_is_console() -> bool {
    std::io::stderr().is_terminal()
}

/// 宽字符直写控制台 stdout（分块：控制台单次写入码元数有上限，保守
/// 8192）。失败静默——菜单渲染尽力而为。
fn write_console_stdout(s: &str) {
    unsafe {
        let Ok(h) = GetStdHandle(STD_OUTPUT_HANDLE) else { return };
        let wide: Vec<u16> = s.encode_utf16().collect();
        let mut written: u32 = 0;
        for chunk in wide.chunks(8192) {
            let _ = WriteConsoleW(h, chunk, Some(&mut written), None);
        }
    }
}

/// 输出一行（带换行）：控制台 → `WriteConsoleW`；管道 → `println!`。
pub(crate) fn out_line(args: std::fmt::Arguments) {
    if stdout_is_console() {
        write_console_stdout(&format!("{args}\n"));
    } else {
        println!("{args}");
    }
}

/// 输出不带换行（菜单提示符 `> ` 用）：控制台 → 直写；管道 → `print!`
/// + flush（沿用行缓冲语义）。
pub(crate) fn out_print(args: std::fmt::Arguments) {
    if stdout_is_console() {
        write_console_stdout(&format!("{args}"));
    } else {
        print!("{args}");
        use std::io::Write;
        let _ = std::io::stdout().flush();
    }
}

/// 控制台代码页 UTF-8 守卫：进入时把输入/输出 CP 切 65001（仅当
/// stdout/stdin 是控制台），Drop 恢复原值。panic 早退路径（process::exit）
/// 不经 Drop——控制台 CP 遗留 65001，无害（多数现代工具常驻该值）。
pub(crate) struct ConsoleCpGuard {
    restore_out_cp: u32,
    restore_in_cp: u32,
}

/// 入口调用（main 最早期）：尽力而为的 UTF-8 代码页化。
pub(crate) fn utf8_console() -> ConsoleCpGuard {
    let mut guard = ConsoleCpGuard {
        restore_out_cp: 0,
        restore_in_cp: 0,
    };
    if !(std::io::stdout().is_terminal() || std::io::stdin().is_terminal()) {
        // 管道/重定向/无控制台：不动（CI 消费者按 UTF-8 字节不变）
        return guard;
    }
    unsafe {
        let out_cp = GetConsoleOutputCP();
        let in_cp = GetConsoleCP();
        // 0 = 无控制台句柄（GetConsole* 失败）；已是 65001 则无需记录
        if out_cp != 0 && out_cp != CP_UTF8 {
            if SetConsoleOutputCP(CP_UTF8).is_ok() {
                guard.restore_out_cp = out_cp;
            }
        }
        if in_cp != 0 && in_cp != CP_UTF8 {
            if SetConsoleCP(CP_UTF8).is_ok() {
                guard.restore_in_cp = in_cp;
            }
        }
    }
    guard
}

impl Drop for ConsoleCpGuard {
    fn drop(&mut self) {
        unsafe {
            if self.restore_out_cp != 0 {
                let _ = SetConsoleOutputCP(self.restore_out_cp);
            }
            if self.restore_in_cp != 0 {
                let _ = SetConsoleCP(self.restore_in_cp);
            }
        }
    }
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

// ============ 菜单层输出宏（任务 41，审查 J） ============
//
// 用法与 println!/print! 同构；控制台 → WriteConsoleW（代码页无关），
// 管道 → 原语义（UTF-8 字节，CI/脚本断言不变）。仅交互层（menu.rs）
// 使用；技术输出（watch/inspect 日志行）保持 println!（入口 CP 守卫
// 已覆盖其控制台渲染）。

/// println! 的控制台安全版（带换行）。
#[macro_export]
macro_rules! outln {
    ($($arg:tt)*) => {
        $crate::console::out_line(format_args!($($arg)*))
    };
}

/// print! 的控制台安全版（不带换行；管道路径自带 flush）。
#[macro_export]
macro_rules! outp {
    ($($arg:tt)*) => {
        $crate::console::out_print(format_args!($($arg)*))
    };
}
