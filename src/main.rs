//! tbg-lite — Windows 任务栏分组控制（零注入路线 B+）
//!
//! 当前任务：5–13（Phase 0b PoC + 默认行为闭环）。CLI 提供 `inspect` / `set` /
//! `watch` / `restore`：任务 5 手工验证属性存储 API 读写语义；任务 6 用
//! SetWinEventHook 事件驱动地自动改写新窗口 AUMID；任务 7 剥离后缀
//! 还原原生分组；任务 8 双线路切换——`watch --strategy ungroup|group`，
//! 线路一每窗口后缀（取消分组），线路二共享 AUMID（自定义分组，
//! 原值落盘 tbg-restore.tsv 供还原）；任务 13 启动扫存量窗口——watch
//! 一启动即把已存在的应用窗口也按线路改写（对齐 mod 默认“开启即全量
//! 取消分组”）；任务 14（2026-09-22 维护者改版）：无参数启动 →
//! 交互菜单（`src/menu.rs`，含退出项，取代 Ctrl+C 方案），带参数
//! 启动 → CLI 行为不变；任务 19（Phase 3）：`install` / `uninstall` /
//! `status`——HKCU Run 开机自启（无需管理员）与状态速览（自启命令、
//! 标记窗口计数、映射表状态）（docs/plan.md v2 §3）。

mod appid;
mod autostart;
mod menu;
mod restoremap;
mod singleinstance;
mod winevent;
mod winutil;

use std::process::ExitCode;
use std::time::Duration;

use windows::Win32::Foundation::HWND;

const HELP: &str = "\
tbg-lite — zero-injection Windows taskbar grouping controller

USAGE:
    tbg-lite                              (no arguments: interactive menu)
    tbg-lite [--version | --help]
    tbg-lite inspect [--hwnd <HEX>] [--all] [--json]
    tbg-lite set --hwnd <HEX> (--suffix | --value <APPID>)
    tbg-lite watch [--strategy <ungroup|group>] [--group <NAME>]
                   [--duration <SECS>] [--dry-run] [--verbose]
    tbg-lite restore [--hwnd <HEX>] [--dry-run]
    tbg-lite install [--strategy <ungroup|group>] [--group <NAME>]
    tbg-lite uninstall
    tbg-lite status

COMMANDS:
    (menu)    launched with NO arguments (task 14): interactive menu —
              start/stop watch on either strategy line, restore all,
              inspect windows, and exit (no Ctrl+C needed; exiting via
              the menu stops the watch gracefully: hooks removed and
              stats printed, with an optional restore-before-exit)
    inspect   list top-level windows and their AppUserModelID
              --hwnd <HEX>   show one window in detail
              --all          also include hidden / tool windows
              --json         machine-readable JSON output (single object
                             with --hwnd, array otherwise; aumid null =
                             unreadable) — consumed by CI scripts
    set       rewrite one window's AppUserModelID (docs/plan.md task 5)
              --suffix       append the per-window ungroup marker (~TBG~w<HWND>)
              --value <ID>   set an exact AppUserModelID
    watch     event-driven watch (docs/plan.md task 6+8+13): on start,
              existing application windows are swept and rewritten along
              the chosen strategy line (task 13: enabling the watch =
              ungroup everything, matching the Windhawk mod default);
              afterwards new top-level windows are handled via
              SetWinEventHook (out-of-context, no injection):
                --strategy ungroup   per-window suffix ~TBG~w<HWND>,
                                     every window gets its own taskbar
                                     group (default; disables grouping)
                --strategy group     rewrite every candidate window
                                     (incl. pre-existing) to the shared
                                     AUMID TBG.Group.<NAME> (custom
                                     grouping; requires --group; originals
                                     are persisted to tbg-restore.tsv
                                     next to the exe)
              --duration <SECS>  run length (default 60; 0 = until stopped:
                                 menu mode exits gracefully via the stop
                                 flag; CLI mode Ctrl+C is a hard exit)
              --dry-run          log only, never write AUMID
              --verbose          also log skipped windows with reasons
    restore   restore native AppUserModelIDs (docs/plan.md task 7+8):
              line 1 strips the per-window suffix; line 2 looks the
              original value up in tbg-restore.tsv. Windows whose
              original AUMID was empty get the property cleared;
              group-marked windows without a map entry are reported
              as orphans and left untouched
              --hwnd <HEX>   restore one window; without it, all windows
              --dry-run      preview only: list what would be restored
                             (targets and original values); no property
                             writes, restore map untouched
    install   register per-user autostart (task 19): writes the HKCU Run
              value "tbg-lite" (no admin rights needed). The registered
              command is the current exe running `watch` along the chosen
              strategy line with --duration 0 (run until stopped);
              default line = ungroup. Re-running install replaces the
              previous command (no accumulation). Console visibility and
              the resident-host lifecycle are task 20 scope
    uninstall remove the autostart entry; idempotent — reports
              "not installed" and exits 0 when nothing is registered
    status    one-glance state (task 19): the autostart command (or
              "not installed"), marked-window counters per strategy line,
              and the restore map (entries / absent / corrupt). Read-only:
              never creates, migrates or rewrites the map

STATUS:
    tasks 5-14 + 19 done; task 15 template shipped (real-machine matrix
    pending maintainer fill) — see docs/plan.md v2 §3
";

fn main() -> ExitCode {
    // 审计 BUG-09（任务 25）：Windows 无 SIGPIPE 概念，stdout 管道读端
    // 关闭后 println! 会 panic（"failed printing to stdout: ..."），release
    // panic=abort 下表现为丑陋中止。装 panic hook：管道断裂 → 静默退出 0
    // （`tbg-lite inspect | head -1` 等 CLI 管道惯例）；其他 panic → 单行
    // 报告 + 101（保留可诊断性）。
    std::panic::set_hook(Box::new(|info| {
        let msg = info.to_string();
        if msg.contains("failed printing to stdout") {
            std::process::exit(0);
        }
        eprintln!("tbg-lite: internal error: {msg}");
        std::process::exit(101);
    }));
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        // 任务 14（2026-09-22 维护者指示）：无参数启动 → 交互菜单
        // （含退出项，不需要 Ctrl+C）；--help 仍打印本帮助文本
        None => menu::run(),
        Some("-h") | Some("--help") => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        Some("-V") | Some("--version") => {
            println!("tbg-lite {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("inspect") => report(cmd_inspect(&args[1..])),
        Some("set") => report(cmd_set(&args[1..])),
        Some("watch") => report(cmd_watch(&args[1..])),
        Some("restore") => report(cmd_restore(&args[1..])),
        Some("install") => report(cmd_install(&args[1..])),
        Some("uninstall") => report(cmd_uninstall(&args[1..])),
        Some("status") => report(cmd_status(&args[1..])),
        Some(other) => {
            eprintln!("tbg-lite: unknown command '{other}' (see --help)");
            ExitCode::from(2)
        }
    }
}

fn report(r: Result<(), String>) -> ExitCode {
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("tbg-lite: error: {e}");
            // 审计 P2-16 简版（任务 25）：用法类错误（参数缺失/非法/组合
            // 不当）退出码 2，运行时错误 1；用法类错误信息统一 "usage: "
            // 前缀供此处判定
            if e.starts_with("usage: ") {
                ExitCode::from(2)
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

fn next_arg<'a>(
    it: &mut impl Iterator<Item = &'a String>,
    flag: &str,
) -> Result<&'a String, String> {
    it.next()
        .ok_or_else(|| format!("usage: missing value for {flag}"))
}

/// JSON 字符串转义（任务 25，审计 BUG-14：inspect --json 机器可读输出
/// 供 CI 消费，根治定宽列解析与标题行抢匹配问题）。
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// 任务 14：cmd_inspect / cmd_restore 供交互菜单复用（菜单项 4/5），
/// 故 pub(crate)。两者内部各自初始化 COM（ComGuard），可在任意线程
/// 逐次调用。
pub(crate) fn cmd_inspect(args: &[String]) -> Result<(), String> {
    let mut hwnd: Option<HWND> = None;
    let mut all = false;
    let mut json = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--hwnd" => hwnd = Some(winutil::parse_hwnd(next_arg(&mut it, "--hwnd")?)?),
            "--all" => all = true,
            "--json" => json = true,
            other => return Err(format!("usage: inspect: unknown argument '{other}'")),
        }
    }
    let _com = winutil::ComGuard::init()?;

    if let Some(hwnd) = hwnd {
        unsafe {
            let aumid = appid::get_aumid(hwnd)
                .map_err(|e| format!("inspect 0x{}: read AUMID failed: {e}", winutil::hwnd_hex(hwnd)))?;
            if json {
                // 单窗 JSON 对象（aumid 恒可读，否则上面已 Err）
                println!(
                    "{{\"hwnd\":\"0x{}\",\"pid\":{},\"class\":\"{}\",\"title\":\"{}\",\"aumid\":\"{}\",\"suffixed\":{},\"grouped\":{}}}",
                    winutil::hwnd_hex(hwnd),
                    winutil::window_pid(hwnd),
                    json_escape(&winutil::class_name(hwnd)),
                    json_escape(&winutil::window_text(hwnd)),
                    json_escape(&aumid),
                    appid::strip_suffix(&aumid, hwnd).is_some(),
                    appid::is_group_aumid(&aumid)
                );
                return Ok(());
            }
            println!("HWND     : 0x{}", winutil::hwnd_hex(hwnd));
            println!("PID      : {}", winutil::window_pid(hwnd));
            println!("Class    : {}", winutil::class_name(hwnd));
            println!("Title    : {}", winutil::window_text(hwnd));
            println!("AUMID    : {}", winutil::shown_aumid(&aumid));
            // 与 JSON 模式一致：严格判定（标记+合法hex+与本窗 HWND 一致）
            println!("Suffixed : {}", appid::strip_suffix(&aumid, hwnd).is_some());
            println!("Grouped  : {}", appid::is_group_aumid(&aumid));
        }
        return Ok(());
    }

    if json {
        // 列表 JSON 数组（机器可读：CI 用 ConvertFrom-Json 消费，
        // 根治定宽列切片错位与标题行抢匹配；读失败窗口 aumid 为 null）
        let mut rows: Vec<String> = Vec::new();
        for hwnd in unsafe { winutil::enum_top_level_windows()? } {
            if !all && !unsafe { winutil::is_app_window(hwnd) } {
                continue;
            }
            unsafe {
                let aumid = appid::get_aumid(hwnd).ok();
                let aumid_json = match &aumid {
                    Some(v) => format!("\"{}\"", json_escape(v)),
                    None => "null".to_string(),
                };
                rows.push(format!(
                    "{{\"hwnd\":\"0x{}\",\"pid\":{},\"class\":\"{}\",\"title\":\"{}\",\"aumid\":{},\"suffixed\":{},\"grouped\":{}}}",
                    winutil::hwnd_hex(hwnd),
                    winutil::window_pid(hwnd),
                    json_escape(&winutil::class_name(hwnd)),
                    json_escape(&winutil::window_text(hwnd)),
                    aumid_json,
                    aumid.as_deref().map(|v| v.contains(appid::SUFFIX_MARKER)).unwrap_or(false),
                    aumid.as_deref().map(appid::is_group_aumid).unwrap_or(false)
                ));
            }
        }
        if rows.is_empty() {
            println!("[]");
        } else {
            println!("[{}]", rows.join(","));
        }
        return Ok(());
    }

    println!(
        "{:<18} {:<7} {:<26} {:<30} {}",
        "HWND", "PID", "CLASS", "AUMID", "TITLE"
    );
    for hwnd in unsafe { winutil::enum_top_level_windows()? } {
        if !all && !unsafe { winutil::is_app_window(hwnd) } {
            continue;
        }
        unsafe {
            let aumid = appid::get_aumid(hwnd).unwrap_or_else(|_| "<unreadable>".into());
            println!(
                "{:<18} {:<7} {:<26} {:<30} {}",
                format!("0x{}", winutil::hwnd_hex(hwnd)),
                winutil::window_pid(hwnd),
                truncate(&winutil::class_name(hwnd), 26),
                truncate(&aumid, 30),
                winutil::window_text(hwnd)
            );
        }
    }
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('~');
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short_passthrough() {
        assert_eq!(truncate("abc", 5), "abc");
        assert_eq!(truncate("", 5), "");
        assert_eq!(truncate("abc", 3), "abc"); // 恰好等长不截
    }

    #[test]
    fn truncate_marks_with_tilde() {
        assert_eq!(truncate("abcdef", 5), "abcd~");
        // 多字节字符不切半：按字符取，非字节
        assert_eq!(truncate("中文测试", 3), "中文~");
    }

    #[test]
    fn json_escape_specials() {
        assert_eq!(json_escape("plain"), "plain");
        assert_eq!(json_escape("a\"b"), "a\\\"b");
        assert_eq!(json_escape("a\\b"), "a\\\\b");
        assert_eq!(json_escape("a\nb"), "a\\nb");
        assert_eq!(json_escape("a\rb"), "a\\rb");
        assert_eq!(json_escape("a\tb"), "a\\tb");
        assert_eq!(json_escape("a\u{1}b"), "a\\u0001b");
        // 非 ASCII 原样（JSON 字符串允许裸 UTF-8）
        assert_eq!(json_escape("中文"), "中文");
    }
}

fn cmd_watch(args: &[String]) -> Result<(), String> {
    let mut duration_secs: u64 = 60;
    let mut dry_run = false;
    let mut verbose = false;
    let mut strategy = winevent::WatchStrategy::Ungroup;
    let mut group: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--duration" => {
                let v = next_arg(&mut it, "--duration")?;
                duration_secs = v
                    .parse()
                    .map_err(|_| format!("usage: watch: invalid duration '{v}' (expected seconds)"))?;
            }
            "--strategy" => {
                let v = next_arg(&mut it, "--strategy")?;
                strategy = match v.as_str() {
                    "ungroup" => winevent::WatchStrategy::Ungroup,
                    "group" => winevent::WatchStrategy::Group,
                    other => {
                        return Err(format!(
                            "usage: watch: unknown strategy '{other}' (expected ungroup|group)"
                        ))
                    }
                };
            }
            "--group" => group = Some(next_arg(&mut it, "--group")?.clone()),
            "--dry-run" => dry_run = true,
            "--verbose" => verbose = true,
            other => return Err(format!("usage: watch: unknown argument '{other}'")),
        }
    }
    // 线路二必须显式给组名；线路一不允许带 --group（防止歧义）
    let group_name = match (strategy, group) {
        (winevent::WatchStrategy::Group, Some(n)) => Some(n),
        (winevent::WatchStrategy::Group, None) => {
            return Err("usage: watch: --strategy group requires --group <NAME>".into())
        }
        (winevent::WatchStrategy::Ungroup, None) => None,
        (winevent::WatchStrategy::Ungroup, Some(_)) => {
            return Err("usage: watch: --group is only valid together with --strategy group".into())
        }
    };
    // 组名属参数校验：提前判（usage 退出码 2；winevent::run 内还会再算一次）
    if let Some(name) = group_name.as_deref() {
        appid::group_aumid(name).map_err(|e| format!("usage: watch: {e}"))?;
    }
    winevent::run(winevent::WatchOptions {
        duration: Duration::from_secs(duration_secs),
        dry_run,
        verbose,
        strategy,
        group_name,
        // CLI 参数模式：无外部停止标志（--duration 0 = Ctrl+C 强杀，
        // 原行为不变；优雅退出属菜单模式，任务 14）
        stop: None,
    })
}

pub(crate) fn cmd_restore(args: &[String]) -> Result<(), String> {
    let mut hwnd: Option<HWND> = None;
    let mut dry_run = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--hwnd" => hwnd = Some(winutil::parse_hwnd(next_arg(&mut it, "--hwnd")?)?),
            "--dry-run" => dry_run = true,
            other => return Err(format!("usage: restore: unknown argument '{other}'")),
        }
    }
    let _com = winutil::ComGuard::init()?;
    unsafe {
        // 无 --hwnd 时全量扫描顶层窗口，逐个还原带标记的窗口
        // （审计 BUG-11：枚举失败上抛而非空表静默空跑）
        let targets: Vec<HWND> = match hwnd {
            Some(h) => vec![h],
            None => unsafe { winutil::enum_top_level_windows()? },
        };
        // 审计 BUG-12（任务 23）：单窗详情模式只应由 --hwnd 显式指定触发；
        // 原先仅按 targets.len()==1 判定，全系统恰有一个顶层窗口时会误入
        let single = hwnd.is_some() && targets.len() == 1;
        let mut restored = 0u32;
        let mut cleared = 0u32;
        let mut skipped = 0u32;
        let mut orphans = 0u32;
        let mut failed = 0u32;
        // 线路二的还原映射（懒加载：首次遇到共享 AUMID 才读盘）
        let mut map: Option<restoremap::RestoreMap> = None;
        let mut map_loaded = false;
        // 审计 BUG-02（任务 22）：restore 会改写映射表（take/save），与
        // `watch --strategy group` 互斥；遇到第一个共享 AUMID 窗口时获取
        // （纯线路一 restore 不碰表、不参与互斥）。守卫存活至函数返回。
        let mut map_mutex: Option<singleinstance::MapMutex> = None;
        // 任务 25（审计 P1-12）：--dry-run 预览计数（不动属性、不动表）
        let mut would = 0u32;
        for hwnd in &targets {
            let aumid = match appid::get_aumid(*hwnd) {
                Ok(a) => a,
                Err(e) => {
                    failed += 1;
                    println!("0x{} read FAILED: {e}", winutil::hwnd_hex(*hwnd));
                    continue;
                }
            };
            if let Some(original) = appid::strip_suffix(&aumid, *hwnd) {
                // 线路一：后缀内联还原（任务 7）
                if dry_run {
                    would += 1;
                    if original.is_empty() {
                        println!(
                            "0x{} {} -> <cleared> [dry-run]",
                            winutil::hwnd_hex(*hwnd),
                            aumid
                        );
                    } else {
                        println!(
                            "0x{} \"{}\" -> \"{}\" [dry-run]",
                            winutil::hwnd_hex(*hwnd),
                            aumid,
                            original
                        );
                    }
                    continue;
                }
                if original.is_empty() {
                    // 原本无 AUMID：清除属性（VT_EMPTY）
                    match appid::clear_aumid(*hwnd) {
                        Ok(()) => {
                            cleared += 1;
                            println!("0x{} {} -> <cleared>", winutil::hwnd_hex(*hwnd), aumid);
                        }
                        Err(e) => {
                            failed += 1;
                            println!("0x{} clear FAILED: {e}", winutil::hwnd_hex(*hwnd));
                        }
                    }
                } else {
                    match appid::set_aumid(*hwnd, original) {
                        Ok(()) => {
                            restored += 1;
                            println!(
                                "0x{} \"{}\" -> \"{}\"",
                                winutil::hwnd_hex(*hwnd),
                                aumid,
                                original
                            );
                        }
                        Err(e) => {
                            failed += 1;
                            println!("0x{} restore FAILED: {e}", winutil::hwnd_hex(*hwnd));
                        }
                    }
                }
            } else if appid::is_group_aumid(&aumid) {
                // 线路二：从还原映射表取原值（任务 8）
                let key = hwnd.0 as usize;
                if map_mutex.is_none() {
                    map_mutex = Some(
                        singleinstance::MapMutex::acquire()
                            .map_err(|e| format!("restore: {e}"))?,
                    );
                }
                if !map_loaded {
                    // 审计 SEC-01（任务 22）：映射表位于 %LOCALAPPDATA%\tbg-lite
                    let dir = match restoremap::data_dir() {
                        Ok(d) => d,
                        Err(e) => {
                            failed += 1;
                            println!("restore map dir unavailable: {e}");
                            map_loaded = true; // 审计 BUG-01：一次性标记，不逐窗口重试
                            continue;
                        }
                    };
                    match restoremap::RestoreMap::load(&dir) {
                        Ok(m) => map = Some(m),
                        Err(e) => {
                            failed += 1;
                            println!("0x{} restore map load FAILED: {e}", winutil::hwnd_hex(*hwnd));
                            // 审计 BUG-01（任务 22）：失败也置位——否则每个共享
                            // AUMID 窗口都会重复读盘并把 failed 虚增 N 次
                            map_loaded = true;
                            continue;
                        }
                    }
                    map_loaded = true;
                }
                if let Some(m) = map.as_mut() {
                    // 任务 25：--dry-run 用 peek（只读预览，条目不动）
                    let outcome = if dry_run {
                        m.peek(key, &aumid)
                    } else {
                        m.take(key, &aumid)
                    };
                    match outcome {
                        Some(original) if dry_run => {
                            would += 1;
                            if original.is_empty() {
                                println!(
                                    "0x{} \"{}\" -> <cleared> [dry-run]",
                                    winutil::hwnd_hex(*hwnd),
                                    aumid
                                );
                            } else {
                                println!(
                                    "0x{} \"{}\" -> \"{}\" [dry-run]",
                                    winutil::hwnd_hex(*hwnd),
                                    aumid,
                                    original
                                );
                            }
                        }
                        Some(original) if original.is_empty() => {
                            match appid::clear_aumid(*hwnd) {
                                Ok(()) => {
                                    cleared += 1;
                                    println!(
                                        "0x{} \"{}\" -> <cleared>",
                                        winutil::hwnd_hex(*hwnd),
                                        aumid
                                    );
                                }
                                Err(e) => {
                                    failed += 1;
                                    println!(
                                        "0x{} clear FAILED: {e}",
                                        winutil::hwnd_hex(*hwnd)
                                    );
                                }
                            }
                        }
                        Some(original) => match appid::set_aumid(*hwnd, &original) {
                            Ok(()) => {
                                restored += 1;
                                println!(
                                    "0x{} \"{}\" -> \"{}\"",
                                    winutil::hwnd_hex(*hwnd),
                                    aumid,
                                    original
                                );
                            }
                            Err(e) => {
                                failed += 1;
                                println!("0x{} restore FAILED: {e}", winutil::hwnd_hex(*hwnd));
                            }
                        },
                        None => {
                            // 无匹配条目（HWND 复用 / 映射丢失）：只报告，不动
                            orphans += 1;
                            println!(
                                "0x{} group AUMID \"{}\" has no matching map entry; left untouched",
                                winutil::hwnd_hex(*hwnd),
                                aumid
                            );
                        }
                    }
                } else {
                    orphans += 1;
                    println!(
                        "0x{} restore map unavailable; left untouched",
                        winutil::hwnd_hex(*hwnd)
                    );
                }
            } else {
                skipped += 1;
                if single {
                    println!(
                        "0x{} no marker, nothing to restore (aumid: {})",
                        winutil::hwnd_hex(*hwnd),
                        winutil::shown_aumid(&aumid)
                    );
                }
            }
        }
        // 映射表有变动（take/remove）则回写；还原完毕且表空则文件删除。
        // 任务 25：--dry-run 只预览，不动表不回写
        if map_loaded && !dry_run {
            if let Some(m) = &map {
                m.save().map_err(|e| format!("restore: {e}"))?;
            }
        }
        println!();
        if dry_run {
            println!(
                "restore summary (dry-run): would-restore={would} skipped(no marker)={skipped} orphans(no map entry)={orphans} failed={failed} — nothing written, restore map untouched"
            );
            return Ok(());
        }
        println!(
            "restore summary: restored={restored} cleared={cleared} skipped(no marker)={skipped} orphans(no map entry)={orphans} failed={failed}"
        );
        println!(
            "note: whether taskbar buttons fully return to native grouping must be observed on real Windows"
        );
        if failed > 0 {
            return Err(format!("restore: {failed} window(s) failed"));
        }
        Ok(())
    }
}

fn cmd_set(args: &[String]) -> Result<(), String> {
    let mut hwnd: Option<HWND> = None;
    let mut value: Option<String> = None;
    let mut suffix = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--hwnd" => hwnd = Some(winutil::parse_hwnd(next_arg(&mut it, "--hwnd")?)?),
            "--suffix" => suffix = true,
            "--value" => value = Some(next_arg(&mut it, "--value")?.clone()),
            other => return Err(format!("usage: set: unknown argument '{other}'")),
        }
    }
    let hwnd = hwnd.ok_or("usage: set: --hwnd <HEX> is required")?;
    if suffix == value.is_some() {
        return Err("usage: set: exactly one of --suffix / --value is required".into());
    }
    let _com = winutil::ComGuard::init()?;
    unsafe {
        let before = appid::get_aumid(hwnd)
            .map_err(|e| format!("set: read AUMID failed: {e}"))?;
        let new_id = if suffix {
            if before.contains(appid::SUFFIX_MARKER) {
                return Err(format!(
                    "set: 0x{} already carries the suffix marker '{}'",
                    winutil::hwnd_hex(hwnd),
                    appid::SUFFIX_MARKER
                ));
            }
            let s = appid::suffixed_aumid(&before, hwnd);
            if s.truncated {
                // 审计 BUG-06（任务 23）：警告按字符数计，与 UTF-16 码元
                // 预算口径分开陈述
                let kept_chars = s.value.chars().count()
                    - appid::SUFFIX_MARKER.chars().count()
                    - winutil::hwnd_hex(hwnd).chars().count();
                eprintln!(
                    "set: warning: original AUMID too long, kept {kept_chars} chars (UTF-16 budget {}) ",
                    appid::AUMID_MAX_LEN - appid::SUFFIX_MARKER.len() - winutil::hwnd_hex(hwnd).len()
                );
            }
            s.value
        } else {
            let v = value.unwrap();
            // 审计 BUG-07/SEC-05（任务 23）：拦超长与控制字符，防破坏
            // 属性存储语义与线路二 TSV 还原表
            appid::validate_aumid_value(&v)
                .map_err(|e| format!("usage: set: invalid --value: {e}"))?;
            v
        };
        let t0 = std::time::Instant::now();
        appid::set_aumid(hwnd, &new_id)
            .map_err(|e| format!("set: write AUMID failed: {e}"))?;
        let write_time = t0.elapsed();
        let after = appid::get_aumid(hwnd)
            .map_err(|e| format!("set: re-read AUMID failed: {e}"))?;
        println!("AUMID: \"{}\" -> \"{}\"", winutil::shown_aumid(&before), winutil::shown_aumid(&after));
        println!("property write+commit took {write_time:?}");
        println!(
            "note: taskbar re-layout latency must be observed manually (docs/plan.md task 5-(2))"
        );
    }
    Ok(())
}

/// 任务 19：注册开机自启。参数与 `watch` 同构（--strategy / --group），
/// 注册的命令行 = 当前 exe + `watch --strategy <s> [--group <NAME>]
/// --duration 0`（0 = 常驻直至停止；常驻宿主生命周期属任务 20）。
fn cmd_install(args: &[String]) -> Result<(), String> {
    let mut strategy = winevent::WatchStrategy::Ungroup;
    let mut group: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--strategy" => {
                let v = next_arg(&mut it, "--strategy")?;
                strategy = match v.as_str() {
                    "ungroup" => winevent::WatchStrategy::Ungroup,
                    "group" => winevent::WatchStrategy::Group,
                    other => {
                        return Err(format!(
                            "usage: install: unknown strategy '{other}' (expected ungroup|group)"
                        ))
                    }
                };
            }
            "--group" => group = Some(next_arg(&mut it, "--group")?.clone()),
            other => return Err(format!("usage: install: unknown argument '{other}'")),
        }
    }
    // 与 watch 相同的组合规则：线路二必须显式组名；线路一不允许带 --group
    let group_name = match (strategy, group) {
        (winevent::WatchStrategy::Group, Some(n)) => Some(n),
        (winevent::WatchStrategy::Group, None) => {
            return Err("usage: install: --strategy group requires --group <NAME>".into())
        }
        (winevent::WatchStrategy::Ungroup, None) => None,
        (winevent::WatchStrategy::Ungroup, Some(_)) => {
            return Err("usage: install: --group is only valid together with --strategy group".into())
        }
    };
    if let Some(name) = group_name.as_deref() {
        appid::group_aumid(name).map_err(|e| format!("usage: install: {e}"))?;
    }
    let exe = std::env::current_exe()
        .map_err(|e| format!("install: cannot locate the current exe: {e}"))?
        .to_string_lossy()
        .into_owned();
    // 注册表里存干净路径：去 `\\?\` 前缀、折叠 `..` 段（GetModuleFileNameW
    // 可能保留启动路径形态，任务 19 注记）
    let exe = autostart::normalize_win_path(&exe);
    let mut tail: Vec<String> = vec![
        "watch".into(),
        "--strategy".into(),
        match strategy {
            winevent::WatchStrategy::Ungroup => "ungroup".into(),
            winevent::WatchStrategy::Group => "group".into(),
        },
    ];
    if let Some(name) = &group_name {
        tail.push("--group".into());
        tail.push(name.clone());
    }
    tail.push("--duration".into());
    tail.push("0".into());
    let refs: Vec<&str> = tail.iter().map(|s| s.as_str()).collect();
    let command = autostart::build_command(&exe, &refs);
    match autostart::install(&command)? {
        autostart::InstallOutcome::New => {
            println!(
                "autostart registered (HKCU Run value '{}')",
                autostart::VALUE_NAME
            );
        }
        autostart::InstallOutcome::Replaced(prev) => {
            println!("autostart updated (previous command replaced)");
            println!("previous: {prev}");
        }
    }
    println!("command : {command}");
    println!(
        "note    : the registered watch runs until stopped (--duration 0); resident-host lifecycle is task 20"
    );
    Ok(())
}

/// 任务 19：删除开机自启（幂等：未安装时明确报告、退出 0）。
fn cmd_uninstall(args: &[String]) -> Result<(), String> {
    if let Some(a) = args.first() {
        return Err(format!("usage: uninstall: unknown argument '{}'", a));
    }
    match autostart::uninstall()? {
        autostart::UninstallOutcome::Removed(prev) => {
            println!(
                "autostart removed (HKCU Run value '{}')",
                autostart::VALUE_NAME
            );
            println!("was     : {prev}");
        }
        autostart::UninstallOutcome::NotInstalled => {
            println!("autostart not installed; nothing to remove");
        }
    }
    Ok(())
}

/// 任务 19：状态速览——自启命令（HKCU Run）、双线路标记窗口计数、映射表
/// 状态。全程只读：注册表只读；窗口 AUMID 只读；映射表走 `restoremap::
/// status`（不建目录/不迁移/不写文件，审计 BUG-02 红线）。
fn cmd_status(args: &[String]) -> Result<(), String> {
    if let Some(a) = args.first() {
        return Err(format!("usage: status: unknown argument '{}'", a));
    }
    // 1) 自启状态（注册表读取，无需 COM）
    match autostart::read_command()? {
        Some(cmd) => println!("autostart  : installed — {cmd}"),
        None => println!("autostart  : not installed"),
    }
    // 2) 标记窗口计数（属性存储读取需 COM；只统计任务栏语义的应用窗口）
    {
        let _com = winutil::ComGuard::init()?;
        let mut line1 = 0u32;
        let mut line2 = 0u32;
        let mut unreadable = 0u32;
        for hwnd in unsafe { winutil::enum_top_level_windows()? } {
            if !unsafe { winutil::is_app_window(hwnd) } {
                continue;
            }
            match unsafe { appid::get_aumid(hwnd) } {
                Ok(a) => {
                    // 与 restore 同口径的严格判定（标记+合法hex+HWND 一致）
                    if appid::strip_suffix(&a, hwnd).is_some() {
                        line1 += 1;
                    } else if appid::is_group_aumid(&a) {
                        line2 += 1;
                    }
                }
                Err(_) => unreadable += 1,
            }
        }
        let total = line1 + line2;
        println!("marked     : line1(ungroup)={line1} line2(group)={line2} (total {total})");
        if unreadable > 0 {
            println!("             ({unreadable} app window(s) had an unreadable AUMID)");
        }
    }
    // 3) 映射表状态（纯只读探测）
    match restoremap::data_dir() {
        Ok(dir) => {
            let path = dir.join(restoremap::MAP_FILE_NAME);
            match restoremap::status(&dir) {
                restoremap::MapStatus::Absent => {
                    println!("restore map: absent ({})", path.display())
                }
                restoremap::MapStatus::Intact { entries } => {
                    println!("restore map: {entries} entries ({})", path.display())
                }
                restoremap::MapStatus::Corrupt(e) => {
                    println!("restore map: CORRUPT — {e} ({})", path.display())
                }
            }
        }
        Err(e) => println!("restore map: unavailable ({e})"),
    }
    Ok(())
}
