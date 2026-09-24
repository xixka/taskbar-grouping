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
    /// 任务 31：退出保活——分离重启子进程所需的 CLI 同构参数。
    strategy: WatchStrategy,
    group_name: Option<String>,
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
    let group_for_thread = group_name.clone();
    let handle = std::thread::spawn(move || {
        winevent::run(WatchOptions {
            duration: Duration::ZERO,
            dry_run: false,
            verbose: false,
            strategy,
            group_name: group_for_thread,
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
        strategy,
        group_name,
    }
}

/// 任务 31：`[k]` 退出保活确认：k/keep（大小写不敏感、容忍首尾空白）
/// 为真——退出菜单但把 watch 分离式重启到后台（菜单进程退出后新窗口
/// 继续被标记、Explorer 回写继续被任务 28 补写）。
fn is_keep(line: &str) -> bool {
    matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "k" | "keep"
    )
}

/// 任务 31：分离式后台重启 watch 子进程（`[0]` 退出选 `k`）。
///
/// **必须在 `stop_and_join` 之后调用**：`Local\tbg-lite.map` 互斥体随
/// watch 线程结束 Drop 释放（group 线路），先停后启保证子进程拿得到
/// 互斥体（审计 BUG-02 单实例红线）。
///
/// 子进程形态 = 纯 CLI `watch --duration 0`（常驻直至被杀；参数与菜单
/// 线路同构）。无控制台窗口（CREATE_NO_WINDOW + 独立进程组，不受父
/// 进程退出/Ctrl+C 影响）；stdout/stderr 追加到
/// `%LOCALAPPDATA%\tbg-lite\tbg-background.log`——CREATE_NO_WINDOW 的
/// 隐式 stdout 是无效句柄，`println!` 写失败会 panic 杀死后台进程，必须
/// 显式给出口（打不开则回退 NUL 设备，丢弃日志但进程存活）。
fn spawn_detached_watch(
    strategy: WatchStrategy,
    group_name: Option<&str>,
) -> Result<u32, String> {
    use std::os::windows::process::CommandExt;

    let exe =
        std::env::current_exe().map_err(|e| format!("cannot resolve current exe path: {e}"))?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("watch")
        .arg("--strategy")
        .arg(match strategy {
            WatchStrategy::Ungroup => "ungroup",
            WatchStrategy::Group => "group",
        })
        // CLI 默认 duration=60s；后台保活必须显式常驻
        .arg("--duration")
        .arg("0");
    if let Some(name) = group_name {
        cmd.arg("--group").arg(name);
    }
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    cmd.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
    // 显式 stdio：日志文件优先，回退 NUL（见函数注释）
    if let Some(log) = background_log() {
        let log_err = log
            .try_clone()
            .map_err(|e| format!("cannot duplicate background log handle: {e}"))?;
        cmd.stdout(std::process::Stdio::from(log));
        cmd.stderr(std::process::Stdio::from(log_err));
    } else {
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
    }
    let child = cmd
        .spawn()
        .map_err(|e| format!("background watch spawn failed: {e}"))?;
    // 分离：不持有句柄、不等待（Windows 下 drop Child 不杀进程）；
    // 返回 pid 供提示。之后用 `tbg-lite` 菜单 [3]/任务管理器结束。
    Ok(child.id())
}

/// 后台保活日志文件（append）。目录沿用映射表数据目录
/// `%LOCALAPPDATA%\tbg-lite`；任何失败返回 None（调用方回退 NUL）。
fn background_log() -> Option<std::fs::File> {
    let dir = crate::restoremap::data_dir().ok()?;
    std::fs::create_dir_all(&dir).ok()?;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("tbg-background.log"))
        .ok()
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

    fn item6(&self) -> &'static str {
        match self.lang {
            Lang::En => "  [6] injection route info   (route A status + Windhawk coexistence guide)",
            Lang::Zh => "  [6] 注入路线说明（路线 A 现状 + Windhawk 协同指引）",
        }
    }

    /// 任务 30：注入路线菜单入口（信息 + 协同引导，**不含任何注入代码**——
    /// plan v2 §5 红线：路线 A 备用不实现）。注入路线的实操载体是
    /// Windhawk（成熟注入平台）+ 其 taskbar-grouping mod：mod 在 explorer
    /// 内部挂钩任务栏自身分组逻辑，天然覆盖 shell 自管回写的 Explorer
    /// 文件夹窗口。给出本机 Windhawk 安装检测 + 冲突规则（README
    /// Coexistence 同口径）+ 操作步骤。
    fn injection_route_info(&self, watch_running: bool) -> String {
        // Windhawk 安装检测（常见两处安装位置；存在 windhawk.exe 即视为
        // 已安装——纯文件系统探测，无注入、无新依赖）
        let mut wh_path = None;
        for base in ["ProgramFiles", "LOCALAPPDATA"] {
            if let Some(dir) = std::env::var_os(base) {
                let cand = std::path::Path::new(&dir)
                    .join("Windhawk")
                    .join("windhawk.exe");
                if cand.is_file() {
                    wh_path = Some(cand);
                    break;
                }
                // per-user 安装布局：LOCALAPPDATA\Programs\Windhawk
                let cand_user = std::path::Path::new(&dir)
                    .join("Programs")
                    .join("Windhawk")
                    .join("windhawk.exe");
                if cand_user.is_file() {
                    wh_path = Some(cand_user);
                    break;
                }
            }
        }
        let wh_line = match &wh_path {
            Some(p) => format!("{} ({})", self.wh_found(), p.display()),
            None => self.wh_not_found().to_string(),
        };
        let mut out = match self.lang {
            Lang::En => format!(
                concat!(
                    "menu: injection route (route A) — info\n",
                    "  tbg-lite itself never injects (plan v2 §5: route A stays the archived\n",
                    "  backup; injection = symbol hooks inside explorer with per-build\n",
                    "  maintenance, AV-false-positive and GPL risks). The practical injection\n",
                    "  route today is Windhawk (https://windhawk.net) + its taskbar-grouping\n",
                    "  mod, which hooks the taskbar itself — this also covers Explorer\n",
                    "  folder windows natively (the non-injection route can only re-assert,\n",
                    "  see task 28).\n",
                    "  Windhawk: {wh_line}\n"
                ),
                wh_line = wh_line
            ),
            Lang::Zh => format!(
                concat!(
                    "menu：注入路线（路线 A）——说明\n",
                    "  tbg-lite 本体不做注入（plan v2 §5：路线 A 为存档备用——注入需在\n",
                    "  explorer 内挂符号钩子，逐版本维护、杀软误报与 GPL 风险）。当前\n",
                    "  可实操的注入路线是 Windhawk（https://windhawk.net）+ 其\n",
                    "  taskbar-grouping mod：mod 直接在任务栏内部挂钩分组逻辑，天然\n",
                    "  覆盖 shell 自管回写的 Explorer 文件夹窗口（非注入路线只能\n",
                    "  检测+补写，见任务 28）。\n",
                    "  Windhawk：{wh_line}\n"
                ),
                wh_line = wh_line
            ),
        };
        if watch_running {
            out.push_str(match self.lang {
                Lang::En => "  ! the tbg-lite watch is RUNNING — stop it first ([3]) and run [4]\n     restore, or both tools will fight over the same windows' AUMIDs.\n",
                Lang::Zh => "  ！tbg-lite watch 正在运行——请先 [3] 停止并 [4] 还原，否则两个\n     工具会争抢同一批窗口的 AUMID。\n",
            });
        }
        out.push_str(match self.lang {
            Lang::En => "  steps: 1) install Windhawk  2) Explore mods -> search \"taskbar group\"\n  3) install & enable the mod  4) keep tbg-lite stopped (or uninstall its\n  autostart). Note: Win11 23H2+ also has a native \"never combine\" taskbar\n  setting (Settings > Personalization > Taskbar) for plain ungrouping.",
            Lang::Zh => "  步骤：1) 安装 Windhawk  2) Explore mods 搜索 \"taskbar group\"\n  3) 安装并启用 mod  4) 保持 tbg-lite 停止（或注销其自启）。注：Win11\n  23H2+ 对纯取消分组还有原生设置（设置 > 个性化 > 任务栏 > 永不合并）。",
        });
        out
    }

    fn wh_found(&self) -> &'static str {
        match self.lang {
            Lang::En => "installed",
            Lang::Zh => "已安装",
        }
    }

    fn wh_not_found(&self) -> &'static str {
        match self.lang {
            Lang::En => "not found (https://windhawk.net)",
            Lang::Zh => "未找到（https://windhawk.net）",
        }
    }

    fn item0(&self) -> &'static str {
        match self.lang {
            Lang::En => {
                "  [0] exit                   (stop watch; [y] restore / [n] keep marks / [k] keep watch alive)"
            }
            Lang::Zh => {
                "  [0] 退出（停止 watch；[y] 还原 / [n] 保留改写 / [k] 后台保活）"
            }
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
            // 任务 31：新增 k = 退出保活（分离式后台重启 watch）；y/N
            // 原语义不变（y=先还原，n/空=保留改写停止 watch）
            Lang::En => {
                "watch is running — restore before exit? [y/N] (k = exit but keep the watch running in the background): "
            }
            Lang::Zh => {
                "watch 运行中——退出前先还原改写？[y/N]（k = 退出但 watch 后台保活继续运行）："
            }
        }
    }

    /// 任务 31：退出保活成功提示。
    fn background_started(&self, pid: u32) -> String {
        match self.lang {
            Lang::En => format!(
                "menu: background watch started (pid {pid}) — rewrites keep being applied to new windows (log: %LOCALAPPDATA%\\tbg-lite\\tbg-background.log); run tbg-lite and use [3] to stop it, or `install` for boot persistence"
            ),
            Lang::Zh => format!(
                "menu：后台 watch 已启动（pid {pid}）——新窗口将继续被标记（日志：%LOCALAPPDATA%\\tbg-lite\\tbg-background.log）；再运行 tbg-lite 用 [3] 停止，开机延续用 `install`"
            ),
        }
    }

    /// 任务 31：退出保活失败提示（已有改写不受影响，仅新窗口不再标记）。
    fn background_failed(&self, e: &str) -> String {
        match self.lang {
            Lang::En => format!(
                "menu: background watch failed to start: {e} (rewrites on existing windows stay; new windows are no longer marked)"
            ),
            Lang::Zh => format!(
                "menu：后台 watch 启动失败：{e}（已有窗口的改写保留；新窗口不再标记）"
            ),
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
            Lang::En => format!("menu: unknown option {other:?} (valid: 0-6, L)"),
            Lang::Zh => format!("menu：未知选项 {other:?}（可用：0-6、L）"),
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
    println!("{}", loc.item6());
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
            "6" => {
                // 任务 30：注入路线入口——信息 + 协同引导（无注入代码，
                // plan v2 §5 红线不破；实操载体 = Windhawk）
                println!("{}", loc.injection_route_info(session.is_some()));
            }
            "0" => {
                if let Some(s) = session.take() {
                    let answer = match prompt(loc.exit_confirm()) {
                        Some(confirm) => confirm,
                        None => {
                            println!();
                            println!("{}", loc.stdin_closed_watch_kept());
                            let _ = s.stop_and_join();
                            return exit_code(exit_restore_failed);
                        }
                    };
                    // 任务 31：k = 退出保活（分离式后台重启 watch）。
                    // 解析先于 stop（答案与 watch 状态无关，先读 stdin）；
                    // 重启必须在 stop_and_join 之后（互斥体释放，见
                    // spawn_detached_watch 注释）
                    let keep_background = is_keep(&answer);
                    let restore_first = !keep_background && is_yes(&answer);
                    // 先停（互斥体随线程 Drop 释放），后启
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
                    } else if keep_background {
                        match spawn_detached_watch(s.strategy, s.group_name.as_deref()) {
                            Ok(pid) => println!("{}", loc.background_started(pid)),
                            Err(e) => println!("{}", loc.background_failed(&e)),
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
