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
//!
//! stdin 首行 UTF-8 BOM 容错：Windows 管道写端（如 PowerShell
//! `Process.StandardInput` 的 StreamWriter）与记事本保存的脚本文件
//! 默认在首行前置 U+FEFF；它**不是** Rust `trim()` 语义的空白，须显式
//! 剥离，否则首条菜单指令被判 unknown（CI run 35698563610 实锤）。
//!
//! 任务 29：菜单文案双语（en/zh）——系统 UI 语言自动检测
//! （GetUserDefaultUILanguage 主语言 ID 0x04 = 中文）+ 菜单内 `L` 键
//! 切换（不落盘，plan v2 §6-2 配置文件维持不需要）。仅菜单层字符串
//! 双语化；watch / inspect / restore 技术输出保持英文（CI 断言与文档
//! 口径）。en-US CI Runner 走 EN 分支，Phase M 菜单流与断言不变。

use std::io::{self, Write};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use windows::Win32::Globalization::GetUserDefaultUILanguage;

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
            // 任务 20：环形日志走 CLI --log 开关；菜单模式默认关
            ring_log: false,
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

/// 剥离行首 UTF-8 BOM（U+FEFF）——它不是 `trim()` 语义的空白，
/// 必须显式处理；否则管道/脚本驱动的首条菜单指令被判 unknown。
fn strip_bom(line: &str) -> &str {
    line.strip_prefix('\u{feff}').unwrap_or(line)
}

/// 读一行 stdin。`None` = EOF / 读失败（调用方应走优雅退出路径）。
/// 行首 U+FEFF（BOM）剥离后返回（见 `strip_bom`）。
fn read_line() -> Option<String> {
    let mut s = String::new();
    match io::stdin().read_line(&mut s) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(strip_bom(&s).to_string()),
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

/// 任务 29：菜单语言——系统 UI 语言自动检测 + 菜单内 `L` 键切换。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lang {
    En,
    Zh,
}

impl Lang {
    /// `GetUserDefaultUILanguage` 主语言 ID（低 10 位）0x04 = 中文
    /// （覆盖 zh-CN / zh-TW / zh-HK 等变体）；其余默认英文。CI Runner 为
    /// en-US → 英文 → Phase M 菜单流与既有英文断言（`interactive menu`
    /// 等）不受影响；`L` 键 CI 脚本不发送，stdin 序列对齐不被扰动。
    fn detect() -> Self {
        let lid = unsafe { GetUserDefaultUILanguage() };
        if lid & 0x3FF == 0x04 {
            Self::Zh
        } else {
            Self::En
        }
    }
}

/// 任务 29：交互菜单文案双语（en/zh）。只覆盖**菜单层**交互字符串；
/// watch / inspect / restore 的技术输出保持英文（CI 断言与文档口径，
/// 任务 28 统计行等不动）。新增菜单字符串须同步补两语言分支。
struct L10n {
    lang: Lang,
}

impl L10n {
    fn new() -> Self {
        Self {
            lang: Lang::detect(),
        }
    }

    fn toggle(&mut self) {
        self.lang = match self.lang {
            Lang::En => Lang::Zh,
            Lang::Zh => Lang::En,
        };
    }

    fn lang_name(&self) -> &'static str {
        match self.lang {
            Lang::En => "English",
            Lang::Zh => "中文",
        }
    }

    fn banner(&self) -> String {
        let ver = env!("CARGO_PKG_VERSION");
        match self.lang {
            // ZH 横幅保留英文短语 interactive menu（CI 对该串有断言，
            // 双保险：CI 上默认走 EN 分支，ZH 分支也含该子串）
            Lang::En => format!("tbg-lite {ver} — interactive menu (no arguments given)"),
            Lang::Zh => format!("tbg-lite {ver} — 交互菜单 interactive menu（无参数启动）"),
        }
    }

    fn tip(&self) -> &'static str {
        match self.lang {
            Lang::En => {
                "tip: `tbg-lite --help` shows the CLI; no Ctrl+C needed — use [0] to exit"
            }
            Lang::Zh => "提示：`tbg-lite --help` 查看 CLI 用法；无需 Ctrl+C——用 [0] 退出",
        }
    }

    /// 语言切换提示行：各语言只展示"如何切到另一种"，避免一行双语冗长。
    fn lang_hint(&self) -> &'static str {
        match self.lang {
            Lang::En => "language: English — press L for 中文",
            Lang::Zh => "语言：中文——按 L 切换 English",
        }
    }

    fn status_running(&self, label: &str) -> String {
        match self.lang {
            Lang::En => format!("watch status : RUNNING — {label}"),
            Lang::Zh => format!("watch 状态：运行中——{label}"),
        }
    }

    fn status_not_running(&self) -> &'static str {
        match self.lang {
            Lang::En => "watch status : not running",
            Lang::Zh => "watch 状态：未运行",
        }
    }

    fn item1(&self) -> &'static str {
        match self.lang {
            Lang::En => "  [1] start watch — ungroup  (default: disable grouping on the taskbar)",
            Lang::Zh => "  [1] 启动 watch——取消分组（默认：任务栏不合并按钮）",
        }
    }

    fn item2(&self) -> &'static str {
        match self.lang {
            Lang::En => "  [2] start watch — group    (custom group name)",
            Lang::Zh => "  [2] 启动 watch——自定义分组（输入组名）",
        }
    }

    fn item3(&self) -> &'static str {
        match self.lang {
            Lang::En => "  [3] stop watch             (graceful: unhook + stats; rewrites stay)",
            Lang::Zh => "  [3] 停止 watch（优雅停止：摘钩 + 统计；改写保留）",
        }
    }

    fn item4(&self) -> &'static str {
        match self.lang {
            Lang::En => {
                "  [4] restore                (undo all rewrites: line-1 suffixes + line-2 map)"
            }
            Lang::Zh => "  [4] 还原（撤销全部改写：线路一后缀 + 线路二映射表）",
        }
    }

    fn item5(&self) -> &'static str {
        match self.lang {
            Lang::En => "  [5] inspect                (list windows and their AUMID state)",
            Lang::Zh => "  [5] 检查（列出窗口及其 AUMID 状态）",
        }
    }

    fn item0(&self) -> &'static str {
        match self.lang {
            Lang::En => {
                "  [0] exit                   (stop watch if running, optionally restore, exit)"
            }
            Lang::Zh => "  [0] 退出（若 watch 运行中则先停止，可选还原，然后退出）",
        }
    }

    fn watch_already_running(&self) -> &'static str {
        match self.lang {
            Lang::En => "menu: watch already running — stop it first with [3]",
            Lang::Zh => "menu：watch 已在运行——请先用 [3] 停止",
        }
    }

    fn watch_started_ungroup(&self) -> &'static str {
        match self.lang {
            Lang::En => "menu: watch started (line 1, ungroup) — taskbar grouping disabled",
            Lang::Zh => "menu：watch 已启动（线路一，取消分组）——任务栏分组已禁用",
        }
    }

    fn group_name_prompt(&self) -> &'static str {
        match self.lang {
            Lang::En => "group name (1-32 chars, [A-Za-z0-9._-], empty = cancel): ",
            Lang::Zh => "组名（1-32 字符，[A-Za-z0-9._-]，空 = 取消）：",
        }
    }

    fn cancelled_empty_group(&self) -> &'static str {
        match self.lang {
            Lang::En => "menu: cancelled (empty group name)",
            Lang::Zh => "menu：已取消（组名为空）",
        }
    }

    fn invalid_group_name(&self, e: &str) -> String {
        match self.lang {
            Lang::En => format!("menu: invalid group name: {e}"),
            Lang::Zh => format!("menu：组名无效：{e}"),
        }
    }

    fn watch_started_group(&self, name: &str) -> String {
        match self.lang {
            Lang::En => format!("menu: watch started (line 2, group {name:?})"),
            Lang::Zh => format!("menu：watch 已启动（线路二，分组 {name:?}）"),
        }
    }

    fn watch_stopped_kept(&self) -> &'static str {
        match self.lang {
            Lang::En => "menu: watch stopped gracefully (stats above; rewrites kept)",
            Lang::Zh => "menu：watch 已优雅停止（统计见上；改写保留）",
        }
    }

    fn watch_stop_failed(&self, e: &str) -> String {
        match self.lang {
            Lang::En => format!("menu: watch stop failed: {e}"),
            Lang::Zh => format!("menu：watch 停止失败：{e}"),
        }
    }

    fn watch_not_running(&self) -> &'static str {
        match self.lang {
            Lang::En => "menu: watch is not running",
            Lang::Zh => "menu：watch 未运行",
        }
    }

    fn stop_first_restore(&self) -> &'static str {
        match self.lang {
            Lang::En => {
                "menu: stop the watch first ([3]) — restoring while watching would fight the rewriter"
            }
            Lang::Zh => "menu：请先停止 watch（[3]）——边监听边还原会与改写逻辑互相打架",
        }
    }

    fn restore_confirm(&self) -> &'static str {
        match self.lang {
            Lang::En => "restore all rewrites? [y/N] ",
            Lang::Zh => "还原全部改写？[y/N] ",
        }
    }

    fn stdin_closed_watch_kept(&self) -> &'static str {
        match self.lang {
            Lang::En => "menu: stdin closed — exiting (watch stopped, rewrites kept)",
            Lang::Zh => "menu：stdin 已关闭——退出（watch 已停止，改写保留）",
        }
    }

    fn stdin_closed_kept(&self) -> &'static str {
        match self.lang {
            Lang::En => "menu: stdin closed — exiting (rewrites kept)",
            Lang::Zh => "menu：stdin 已关闭——退出（改写保留）",
        }
    }

    fn restore_cancelled(&self) -> &'static str {
        match self.lang {
            Lang::En => "menu: restore cancelled",
            Lang::Zh => "menu：已取消还原",
        }
    }

    fn restore_finished(&self) -> &'static str {
        match self.lang {
            Lang::En => "menu: restore finished (see summary above)",
            Lang::Zh => "menu：还原完成（汇总见上）",
        }
    }

    fn restore_failed(&self, e: &str) -> String {
        match self.lang {
            Lang::En => format!("menu: restore failed: {e}"),
            Lang::Zh => format!("menu：还原失败：{e}"),
        }
    }

    fn inspect_failed(&self, e: &str) -> String {
        match self.lang {
            Lang::En => format!("menu: inspect failed: {e}"),
            Lang::Zh => format!("menu：inspect 失败：{e}"),
        }
    }

    fn exit_confirm(&self) -> &'static str {
        match self.lang {
            Lang::En => "watch is running — restore rewrites before exit? [y/N] ",
            Lang::Zh => "watch 运行中——退出前先还原改写？[y/N] ",
        }
    }

    fn watch_stopped_stats(&self) -> &'static str {
        match self.lang {
            Lang::En => "menu: watch stopped gracefully (stats above)",
            Lang::Zh => "menu：watch 已优雅停止（统计见上）",
        }
    }

    fn exiting_bye(&self) -> &'static str {
        match self.lang {
            Lang::En => "menu: exiting — bye.",
            Lang::Zh => "menu：退出——再见。",
        }
    }

    fn unknown_option(&self, other: &str) -> String {
        match self.lang {
            Lang::En => format!("menu: unknown option {other:?} (valid: 0-5, L)"),
            Lang::Zh => format!("menu：未知选项 {other:?}（可用：0-5、L）"),
        }
    }

    fn watch_ended_own(&self) -> &'static str {
        match self.lang {
            Lang::En => "menu: watch ended on its own (see stats above)",
            Lang::Zh => "menu：watch 已自行结束（统计见上）",
        }
    }

    fn watch_failed(&self, e: &str) -> String {
        match self.lang {
            Lang::En => format!("menu: watch failed: {e}"),
            Lang::Zh => format!("menu：watch 失败：{e}"),
        }
    }

    fn watch_panicked(&self) -> &'static str {
        match self.lang {
            Lang::En => "menu: watch thread panicked (see stderr)",
            Lang::Zh => "menu：watch 线程 panic（见 stderr）",
        }
    }

    fn lang_switched(&self) -> String {
        match self.lang {
            Lang::En => format!("menu: language switched to {}", self.lang_name()),
            Lang::Zh => format!("menu：语言已切换为{}", self.lang_name()),
        }
    }
}

fn print_menu(loc: &L10n, running: Option<&WatchSession>) {
    println!();
    if let Some(s) = running {
        println!("{}", loc.status_running(s.label));
    } else {
        println!("{}", loc.status_not_running());
    }
    println!("{}", loc.item1());
    println!("{}", loc.item2());
    println!("{}", loc.item3());
    println!("{}", loc.item4());
    println!("{}", loc.item5());
    println!("{}", loc.item0());
    println!("{}", loc.lang_hint());
}

/// 菜单主循环。返回进程退出码。
pub(crate) fn run() -> ExitCode {
    // 任务 29：语言 = 系统 UI 语言自动检测（en-US CI → 英文，中文系统 →
    // 中文），菜单内 L 键随时切换（仅影响菜单层文案）。
    let mut loc = L10n::new();
    println!("{}", loc.banner());
    println!("{}", loc.tip());
    let mut session: Option<WatchSession> = None;
    let mut exit_restore_failed = false;
    loop {
        // 后台 watch 意外早退（如互斥体被占、钩子安装失败）→ 收割结果
        // 并回落到未运行状态，避免菜单显示一个已死的 RUNNING
        if let Some(s) = session.take() {
            if s.handle.is_finished() {
                match s.handle.join() {
                    Ok(Ok(())) => println!("{}", loc.watch_ended_own()),
                    Ok(Err(e)) => println!("{}", loc.watch_failed(&e)),
                    Err(_) => println!("{}", loc.watch_panicked()),
                }
                println!("{}", loc.status_not_running());
            } else {
                session = Some(s);
            }
        }
        print_menu(&loc, session.as_ref());
        let Some(line) = prompt("> ") else {
            // stdin 关闭：视同 [0]，默认不还原（无法交互确认）
            println!();
            println!("{}", loc.stdin_closed_watch_kept());
            if let Some(s) = session.take() {
                let _ = s.stop_and_join();
            }
            return exit_code(exit_restore_failed);
        };
        match line.trim() {
            "l" | "L" => {
                // 任务 29：语言切换（会话内即时生效，不落盘——plan v2 §6-2
                // 配置文件维持不需要）
                loc.toggle();
                println!("{}", loc.lang_switched());
            }
            "1" => {
                if session.is_some() {
                    println!("{}", loc.watch_already_running());
                } else {
                    session = Some(spawn_watch(WatchStrategy::Ungroup, None));
                    println!("{}", loc.watch_started_ungroup());
                }
            }
            "2" => {
                if session.is_some() {
                    println!("{}", loc.watch_already_running());
                    continue;
                }
                let Some(name_line) = prompt(loc.group_name_prompt()) else {
                    println!();
                    println!("{}", loc.stdin_closed_watch_kept());
                    return exit_code(exit_restore_failed);
                };
                let name = name_line.trim();
                if name.is_empty() {
                    println!("{}", loc.cancelled_empty_group());
                    continue;
                }
                // 复用 CLI 同一套组名校验（长度/字符集 → 共享 AUMID 构造）
                if let Err(e) = crate::appid::group_aumid(name) {
                    println!("{}", loc.invalid_group_name(&e));
                    continue;
                }
                session = Some(spawn_watch(
                    WatchStrategy::Group,
                    Some(name.to_string()),
                ));
                println!("{}", loc.watch_started_group(name));
            }
            "3" => match session.take() {
                Some(s) => match s.stop_and_join() {
                    Ok(()) => println!("{}", loc.watch_stopped_kept()),
                    Err(e) => println!("{}", loc.watch_stop_failed(&e)),
                },
                None => println!("{}", loc.watch_not_running()),
            },
            "4" => {
                if session.is_some() {
                    println!("{}", loc.stop_first_restore());
                    continue;
                }
                let Some(confirm) = prompt(loc.restore_confirm()) else {
                    println!();
                    println!("{}", loc.stdin_closed_kept());
                    return exit_code(exit_restore_failed);
                };
                if !is_yes(&confirm) {
                    println!("{}", loc.restore_cancelled());
                    continue;
                }
                match crate::cmd_restore(&[]) {
                    Ok(()) => println!("{}", loc.restore_finished()),
                    Err(e) => println!("{}", loc.restore_failed(&e)),
                }
            }
            "5" => {
                if let Err(e) = crate::cmd_inspect(&[]) {
                    println!("{}", loc.inspect_failed(&e));
                }
            }
            "0" => {
                if let Some(s) = session.take() {
                    let restore_first = match prompt(loc.exit_confirm()) {
                        Some(confirm) => is_yes(&confirm),
                        None => {
                            println!();
                            println!("{}", loc.stdin_closed_watch_kept());
                            let _ = s.stop_and_join();
                            return exit_code(exit_restore_failed);
                        }
                    };
                    match s.stop_and_join() {
                        Ok(()) => println!("{}", loc.watch_stopped_stats()),
                        Err(e) => println!("{}", loc.watch_stop_failed(&e)),
                    }
                    if restore_first {
                        match crate::cmd_restore(&[]) {
                            Ok(()) => println!("{}", loc.restore_finished()),
                            Err(e) => {
                                println!("{}", loc.restore_failed(&e));
                                exit_restore_failed = true;
                            }
                        }
                    }
                }
                println!("{}", loc.exiting_bye());
                return exit_code(exit_restore_failed);
            }
            other => println!("{}", loc.unknown_option(other)),
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
    use super::{is_yes, strip_bom, Lang, L10n};

    #[test]
    fn lang_toggle_roundtrip() {
        // 任务 29：语言切换纯逻辑（detect 涉及 Win32 API，不在单测覆盖）
        let mut loc = L10n { lang: Lang::En };
        assert_eq!(loc.lang_name(), "English");
        loc.toggle();
        assert_eq!(loc.lang_name(), "中文");
        loc.toggle();
        assert_eq!(loc.lang_name(), "English");
    }

    #[test]
    fn zh_banner_keeps_ci_asserted_substring() {
        // CI 断言 `-cmatch 'interactive menu'`：ZH 横幅也须含该子串
        let loc = L10n { lang: Lang::Zh };
        assert!(loc.banner().contains("interactive menu"));
    }

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

    #[test]
    fn bom_prefix_stripped() {
        // CI run 35698563610 实锤：PS StandardInput 首次写入前置 BOM，
        // "\u{feff}1" 若不剥离则首条菜单指令判 unknown
        assert_eq!(strip_bom("\u{feff}1"), "1");
        assert_eq!(strip_bom("\u{feff}smoke\n"), "smoke\n");
    }

    #[test]
    fn bom_absent_passthrough() {
        assert_eq!(strip_bom("1"), "1");
        assert_eq!(strip_bom(""), "");
        // BOM 只在行首剥离，行中出现则保留（不误伤内容）
        assert_eq!(strip_bom("a\u{feff}b"), "a\u{feff}b");
    }
}
