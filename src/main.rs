//! tbg-lite — Windows 任务栏分组控制（零注入路线 B+）
//!
//! 当前任务：5–8（Phase 0b PoC）。CLI 提供 `inspect` / `set` / `watch` /
//! `restore`：任务 5 手工验证属性存储 API 读写语义；任务 6 用
//! SetWinEventHook 事件驱动地自动改写新窗口 AUMID；任务 7 剥离后缀
//! 还原原生分组；任务 8 双线路切换——`watch --strategy ungroup|group`，
//! 线路一每窗口后缀（取消分组），线路二共享 AUMID（自定义分组，
//! 原值落盘 tbg-restore.tsv 供还原）（docs/plan.md §7 Phase 0b）。

mod appid;
mod restoremap;
mod winevent;
mod winutil;

use std::process::ExitCode;
use std::time::Duration;

use windows::Win32::Foundation::HWND;

const HELP: &str = "\
tbg-lite — zero-injection Windows taskbar grouping controller

USAGE:
    tbg-lite [--version | --help]
    tbg-lite inspect [--hwnd <HEX>] [--all]
    tbg-lite set --hwnd <HEX> (--suffix | --value <APPID>)
    tbg-lite watch [--strategy <ungroup|group>] [--group <NAME>]
                   [--duration <SECS>] [--dry-run] [--verbose]
    tbg-lite restore [--hwnd <HEX>]

COMMANDS:
    inspect   list top-level windows and their AppUserModelID
              --hwnd <HEX>   show one window in detail
              --all          also include hidden / tool windows
    set       rewrite one window's AppUserModelID (docs/plan.md task 5)
              --suffix       append the per-window ungroup marker (~TBG~w<HWND>)
              --value <ID>   set an exact AppUserModelID
    watch     event-driven PoC (docs/plan.md task 6+8): listen for new
              top-level windows via SetWinEventHook (out-of-context,
              no injection) and rewrite their AUMID along one of two
              strategy lines (docs/plan.md §4 route B+):
                --strategy ungroup   per-window suffix ~TBG~w<HWND>,
                                     every window gets its own taskbar
                                     group (default; disables grouping)
                --strategy group     rewrite every new candidate window
                                     to the shared AUMID TBG.Group.<NAME>
                                     (custom grouping; requires --group;
                                     originals are persisted to
                                     tbg-restore.tsv next to the exe)
              --duration <SECS>  run length (default 60; 0 = until Ctrl+C)
              --dry-run          log only, never write AUMID
              --verbose          also log skipped windows with reasons
    restore   restore native AppUserModelIDs (docs/plan.md task 7+8):
              line 1 strips the per-window suffix; line 2 looks the
              original value up in tbg-restore.tsv. Windows whose
              original AUMID was empty get the property cleared;
              group-marked windows without a map entry are reported
              as orphans and left untouched
              --hwnd <HEX>   restore one window; without it, all windows

STATUS:
    tasks 5-8 (Phase 0b PoC) — see docs/plan.md §7 Phase 0b
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("-h") | Some("--help") => {
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
            ExitCode::FAILURE
        }
    }
}

fn next_arg<'a>(
    it: &mut impl Iterator<Item = &'a String>,
    flag: &str,
) -> Result<&'a String, String> {
    it.next()
        .ok_or_else(|| format!("missing value for {flag}"))
}

fn cmd_inspect(args: &[String]) -> Result<(), String> {
    let mut hwnd: Option<HWND> = None;
    let mut all = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--hwnd" => hwnd = Some(winutil::parse_hwnd(next_arg(&mut it, "--hwnd")?)?),
            "--all" => all = true,
            other => return Err(format!("inspect: unknown argument '{other}'")),
        }
    }
    let _com = winutil::ComGuard::init()?;

    if let Some(hwnd) = hwnd {
        unsafe {
            let aumid = appid::get_aumid(hwnd)
                .map_err(|e| format!("inspect 0x{}: read AUMID failed: {e}", winutil::hwnd_hex(hwnd)))?;
            println!("HWND     : 0x{}", winutil::hwnd_hex(hwnd));
            println!("PID      : {}", winutil::window_pid(hwnd));
            println!("Class    : {}", winutil::class_name(hwnd));
            println!("Title    : {}", winutil::window_text(hwnd));
            println!("AUMID    : {}", winutil::shown_aumid(&aumid));
            println!("Suffixed : {}", aumid.contains(appid::SUFFIX_MARKER));
            println!("Grouped  : {}", appid::is_group_aumid(&aumid));
        }
        return Ok(());
    }

    println!(
        "{:<18} {:<7} {:<26} {:<30} {}",
        "HWND", "PID", "CLASS", "AUMID", "TITLE"
    );
    for hwnd in unsafe { winutil::enum_top_level_windows() } {
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
                    .map_err(|_| format!("watch: invalid duration '{v}' (expected seconds)"))?;
            }
            "--strategy" => {
                let v = next_arg(&mut it, "--strategy")?;
                strategy = match v.as_str() {
                    "ungroup" => winevent::WatchStrategy::Ungroup,
                    "group" => winevent::WatchStrategy::Group,
                    other => {
                        return Err(format!(
                            "watch: unknown strategy '{other}' (expected ungroup|group)"
                        ))
                    }
                };
            }
            "--group" => group = Some(next_arg(&mut it, "--group")?.clone()),
            "--dry-run" => dry_run = true,
            "--verbose" => verbose = true,
            other => return Err(format!("watch: unknown argument '{other}'")),
        }
    }
    // 线路二必须显式给组名；线路一不允许带 --group（防止歧义）
    let group_name = match (strategy, group) {
        (winevent::WatchStrategy::Group, Some(n)) => Some(n),
        (winevent::WatchStrategy::Group, None) => {
            return Err("watch: --strategy group requires --group <NAME>".into())
        }
        (winevent::WatchStrategy::Ungroup, None) => None,
        (winevent::WatchStrategy::Ungroup, Some(_)) => {
            return Err("watch: --group is only valid together with --strategy group".into())
        }
    };
    winevent::run(winevent::WatchOptions {
        duration: Duration::from_secs(duration_secs),
        dry_run,
        verbose,
        strategy,
        group_name,
    })
}

fn cmd_restore(args: &[String]) -> Result<(), String> {
    let mut hwnd: Option<HWND> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--hwnd" => hwnd = Some(winutil::parse_hwnd(next_arg(&mut it, "--hwnd")?)?),
            other => return Err(format!("restore: unknown argument '{other}'")),
        }
    }
    let _com = winutil::ComGuard::init()?;
    unsafe {
        // 无 --hwnd 时全量扫描顶层窗口，逐个还原带标记的窗口
        let targets: Vec<HWND> = match hwnd {
            Some(h) => vec![h],
            None => winutil::enum_top_level_windows(),
        };
        let single = targets.len() == 1;
        let mut restored = 0u32;
        let mut cleared = 0u32;
        let mut skipped = 0u32;
        let mut orphans = 0u32;
        let mut failed = 0u32;
        // 线路二的还原映射（懒加载：首次遇到共享 AUMID 才读盘）
        let mut map: Option<restoremap::RestoreMap> = None;
        let mut map_loaded = false;
        for hwnd in &targets {
            let aumid = match appid::get_aumid(*hwnd) {
                Ok(a) => a,
                Err(e) => {
                    failed += 1;
                    println!("0x{} read FAILED: {e}", winutil::hwnd_hex(*hwnd));
                    continue;
                }
            };
            if let Some(original) = appid::strip_suffix(&aumid) {
                // 线路一：后缀内联还原（任务 7）
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
                if !map_loaded {
                    let dir = std::env::current_exe()
                        .ok()
                        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                        .ok_or_else(|| {
                            "restore: cannot locate exe directory for restore map".to_string()
                        })?;
                    match restoremap::RestoreMap::load(&dir) {
                        Ok(m) => map = Some(m),
                        Err(e) => {
                            failed += 1;
                            println!("0x{} restore map load FAILED: {e}", winutil::hwnd_hex(*hwnd));
                            continue;
                        }
                    }
                    map_loaded = true;
                }
                if let Some(m) = map.as_mut() {
                    match m.take(key, &aumid) {
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
        // 映射表有变动（take/remove）则回写；还原完毕且表空则文件删除
        if map_loaded {
            if let Some(m) = &map {
                m.save().map_err(|e| format!("restore: {e}"))?;
            }
        }
        println!();
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
            other => return Err(format!("set: unknown argument '{other}'")),
        }
    }
    let hwnd = hwnd.ok_or("set: --hwnd <HEX> is required")?;
    if suffix == value.is_some() {
        return Err("set: exactly one of --suffix / --value is required".into());
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
                eprintln!(
                    "set: warning: original AUMID too long, truncated to {} chars",
                    s.value.len() - appid::SUFFIX_MARKER.len() - winutil::hwnd_hex(hwnd).len()
                );
            }
            s.value
        } else {
            value.unwrap()
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
