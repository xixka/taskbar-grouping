//! 交互菜单模式（任务 14，docs/plan.md v2 §3；2026-09-22 维护者改版）。
//!
//! `tbg-lite` 无参数启动 → 本菜单；带参数启动 → CLI 行为不变。
//! 维护者指示：**不需要 Ctrl+C**——退出走菜单项 `[0]`：
//! - watch 运行中 → 询问是否先还原（默认否），再优雅停止 watch
//!   （`Arc<AtomicBool>` 停止标志 → 摘钩 + 终扫 + 统计输出，见
//!   `winevent::run`），最后进程退出码 0；
//! - watch 未运行 → 直接退出。
//!
//! 架构：watch 跑在**后台线程**（winevent 钩子与消息泵都是线程亲和的，
//! 独立线程自含 COM 初始化与消息泵）；菜单线程只读 stdin 派发动作，
//! 自身不初始化 COM——`inspect` / `restore` 动作直接复用 main.rs 的
//! `cmd_inspect` / `cmd_restore`（内部各自 ComGuard，逐次调用即可）。
//! stdin 关闭（EOF / 重定向管道写端关闭）→ 视同选择退出（默认不还原），
//! 保证脚本化/CI 驱动下不会忙转。

use std::io::{self, Write};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use crate::winevent::{self, WatchOptions, WatchStrategy};

/// 一个正在后台运行的 watch 会话。
struct WatchSession {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<Result<(), String>>,
    /// 菜单展示用的线路标签。
    label: &'static str,
}

impl WatchSession {
    /// 置位停止标志并 join 后台线程。watch 线程在返回前已自行完成
    /// 摘钩、终扫与统计输出；这里只回收结果。
    fn stop_and_join(mut self) -> Result<(), String> {
        self.stop.store(true, Ordering::Relaxed);
        match self.handle.join() {
            Ok(r) => r,
            Err(_) => Err("watch thread panicked (see stderr)".into()),
        }
    }
}

/// 后台线程里跑 winevent::run（duration=0 常驻，由停止标志退出）。
fn spawn_watch(strategy: WatchStrategy, group_name: Option<String>) -> WatchSession {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = stop.clone();
    let handle = std::thread::spawn(move || {
        winevent::run(WatchOptions {
            duration: Duration::ZERO,
            dry_run: false,
            verbose: false,
            strategy,
            group_name,
            // 任务 14：菜单模式——外部停止标志（优雅退出）
            stop: Some(stop_flag),
        })
    });
    WatchSession {
        stop,
        handle,
        label: match strategy {
            WatchStrategy::Ungroup => "ungroup (line 1: disable grouping on the taskbar)",
            WatchStrategy::Group => "group (line 2: shared AUMID)",
        },
    }
}

/// 读一行 stdin。`None` = EOF / 读失败（调用方应走优雅退出路径）。
fn read_line() -> Option<String> {
    let mut s = String::new();
    match io::stdin().read_line(&mut s) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(s),
    }
}

/// 打印提示符并读一行（提示符需要显式 flush：stdout 是行缓冲）。
fn prompt(text: &str) -> Option<String> {
    print!("{text}");
    let _ = io::stdout().flush();
    read_line()
}

/// `[y/N]` 确认：仅 y/yes（大小写不敏感、容忍首尾空白）为真，默认否。
fn is_yes(line: &str) -> bool {
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn print_menu(running: Option<&WatchSession>) {
    println!();
    if let Some(s) = running {
        println!("watch status : RUNNING — {}", s.label);
    } else {
        println!("watch status : not running");
    }
    println!("  [1] start watch — ungroup  (default: disable grouping on the taskbar)");
    println!("  [2] start watch — group    (custom group name)");
    println!("  [3] stop watch             (graceful: unhook + stats; rewrites stay)");
    println!("  [4] restore                (undo all rewrites: line-1 suffixes + line-2 map)");
    println!("  [5] inspect                (list windows and their AUMID state)");
    println!("  [0] exit                   (stop watch if running, optionally restore, exit)");
}

/// 菜单主循环。返回进程退出码。
pub(crate) fn run() -> ExitCode {
    println!(
        "tbg-lite {} — interactive menu (no arguments given)",
        env!("CARGO_PKG_VERSION")
    );
    println!("tip: `tbg-lite --help` shows the CLI; no Ctrl+C needed — use [0] to exit");
    let mut session: Option<WatchSession> = None;
    let mut exit_restore_failed = false;
    loop {
        // 后台 watch 意外早退（如互斥体被占、钩子安装失败）→ 收割结果
        // 并回落到未运行状态，避免菜单显示一个已死的 RUNNING
        if let Some(s) = session.take() {
            if s.handle.is_finished() {
                match s.handle.join() {
                    Ok(Ok(())) => println!("menu: watch ended on its own (see stats above)"),
                    Ok(Err(e)) => println!("menu: watch failed: {e}"),
                    Err(_) => println!("menu: watch thread panicked (see stderr)"),
                }
                println!("watch status : not running");
            } else {
                session = Some(s);
            }
        }
        print_menu(session.as_ref());
        let Some(line) = prompt("> ") else {
            // stdin 关闭：视同 [0]，默认不还原（无法交互确认）
            println!();
            println!("menu: stdin closed — exiting (watch stopped, rewrites kept)");
            if let Some(s) = session.take() {
                let _ = s.stop_and_join();
            }
            return exit_code(exit_restore_failed);
        };
        match line.trim() {
            "1" => {
                if session.is_some() {
                    println!("menu: watch already running — stop it first with [3]");
                } else {
                    session = Some(spawn_watch(WatchStrategy::Ungroup, None));
                    println!("menu: watch started (line 1, ungroup) — taskbar grouping disabled");
                }
            }
            "2" => {
                if session.is_some() {
                    println!("menu: watch already running — stop it first with [3]");
                    continue;
                }
                let Some(name_line) = prompt("group name (1-32 chars, [A-Za-z0-9._-], empty = cancel): ")
                else {
                    println!();
                    println!("menu: stdin closed — exiting (watch stopped, rewrites kept)");
                    return exit_code(exit_restore_failed);
                };
                let name = name_line.trim();
                if name.is_empty() {
                    println!("menu: cancelled (empty group name)");
                    continue;
                }
                // 复用 CLI 同一套组名校验（长度/字符集 → 共享 AUMID 构造）
                if let Err(e) = crate::appid::group_aumid(name) {
                    println!("menu: invalid group name: {e}");
                    continue;
                }
                session = Some(spawn_watch(
                    WatchStrategy::Group,
                    Some(name.to_string()),
                ));
                println!("menu: watch started (line 2, group {name:?})");
            }
            "3" => match session.take() {
                Some(s) => match s.stop_and_join() {
                    Ok(()) => println!("menu: watch stopped gracefully (stats above; rewrites kept)"),
                    Err(e) => println!("menu: watch stop failed: {e}"),
                },
                None => println!("menu: watch is not running"),
            },
            "4" => {
                if session.is_some() {
                    println!("menu: stop the watch first ([3]) — restoring while watching would fight the rewriter");
                    continue;
                }
                let Some(confirm) = prompt("restore all rewrites? [y/N] ") else {
                    println!();
                    println!("menu: stdin closed — exiting (rewrites kept)");
                    return exit_code(exit_restore_failed);
                };
                if !is_yes(&confirm) {
                    println!("menu: restore cancelled");
                    continue;
                }
                match crate::cmd_restore(&[]) {
                    Ok(()) => println!("menu: restore finished (see summary above)"),
                    Err(e) => println!("menu: restore failed: {e}"),
                }
            }
            "5" => {
                if let Err(e) = crate::cmd_inspect(&[]) {
                    println!("menu: inspect failed: {e}");
                }
            }
            "0" => {
                if let Some(s) = session.take() {
                    let restore_first = match prompt("watch is running — restore rewrites before exit? [y/N] ") {
                        Some(confirm) => is_yes(&confirm),
                        None => {
                            println!();
                            println!("menu: stdin closed — exiting (watch stopped, rewrites kept)");
                            let _ = s.stop_and_join();
                            return exit_code(exit_restore_failed);
                        }
                    };
                    match s.stop_and_join() {
                        Ok(()) => println!("menu: watch stopped gracefully (stats above)"),
                        Err(e) => println!("menu: watch stop failed: {e}"),
                    }
                    if restore_first {
                        match crate::cmd_restore(&[]) {
                            Ok(()) => println!("menu: restore finished (see summary above)"),
                            Err(e) => {
                                println!("menu: restore failed: {e}");
                                exit_restore_failed = true;
                            }
                        }
                    }
                }
                println!("menu: exiting — bye.");
                return exit_code(exit_restore_failed);
            }
            other => println!("menu: unknown option {other:?} (valid: 0-5)"),
        }
    }
}

fn exit_code(restore_failed: bool) -> ExitCode {
    if restore_failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::is_yes;

    #[test]
    fn yes_variants() {
        assert!(is_yes("y"));
        assert!(is_yes("Y"));
        assert!(is_yes("yes"));
        assert!(is_yes("YES"));
        assert!(is_yes("  y  ")); // 容忍首尾空白（读行含换行符场景）
    }

    #[test]
    fn no_is_default() {
        assert!(!is_yes(""));
        assert!(!is_yes("\n"));
        assert!(!is_yes("n"));
        assert!(!is_yes("N"));
        assert!(!is_yes("no"));
        assert!(!is_yes("ye"));
        assert!(!is_yes("yeah"));
    }
}
