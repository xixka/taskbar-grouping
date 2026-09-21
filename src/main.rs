//! tbg-lite — Windows 任务栏分组控制（零注入路线 B+）
//!
//! 当前任务：5–7（Phase 0b PoC）。CLI 提供 `inspect` / `set` / `watch` /
//! `restore`：任务 5 手工验证属性存储 API 读写语义；任务 6 用
//! SetWinEventHook 事件驱动地自动改写新窗口 AUMID；任务 7 剥离后缀
//! 还原原生分组（docs/plan.md §7 Phase 0b）。

mod appid;
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
    tbg-lite watch [--duration <SECS>] [--dry-run] [--verbose]
    tbg-lite restore [--hwnd <HEX>]

COMMANDS:
    inspect   list top-level windows and their AppUserModelID
              --hwnd <HEX>   show one window in detail
              --all          also include hidden / tool windows
    set       rewrite one window's AppUserModelID (docs/plan.md task 5)
              --suffix       append the per-window ungroup marker (~TBG~w<HWND>)
              --value <ID>   set an exact AppUserModelID
    watch     event-driven PoC (docs/plan.md task 6): listen for new
              top-level windows via SetWinEventHook (out-of-context,
              no injection) and rewrite their AUMID with the per-window
              suffix; prints stats plus a missed/reverted scan at exit
              --duration <SECS>  run length (default 60; 0 = until Ctrl+C)
              --dry-run          log only, never write AUMID
              --verbose          also log skipped windows with reasons
    restore   strip the per-window suffix and restore the original
              AppUserModelID (docs/plan.md task 7); windows whose
              original AUMID was empty get the property cleared
              --hwnd <HEX>   restore one window; without it, all windows

STATUS:
    tasks 5-7 (Phase 0b PoC) — see docs/plan.md §7 Phase 0b
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
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--duration" => {
                let v = next_arg(&mut it, "--duration")?;
                duration_secs = v
                    .parse()
                    .map_err(|_| format!("watch: invalid duration '{v}' (expected seconds)"))?;
            }
            "--dry-run" => dry_run = true,
            "--verbose" => verbose = true,
            other => return Err(format!("watch: unknown argument '{other}'")),
        }
    }
    winevent::run(winevent::WatchOptions {
        duration: Duration::from_secs(duration_secs),
        dry_run,
        verbose,
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
        let mut failed = 0u32;
        for hwnd in &targets {
            let aumid = match appid::get_aumid(*hwnd) {
                Ok(a) => a,
                Err(e) => {
                    failed += 1;
                    println!("0x{} read FAILED: {e}", winutil::hwnd_hex(*hwnd));
                    continue;
                }
            };
            let Some(original) = appid::strip_suffix(&aumid) else {
                skipped += 1;
                if single {
                    println!(
                        "0x{} no suffix marker, nothing to restore (aumid: {})",
                        winutil::hwnd_hex(*hwnd),
                        winutil::shown_aumid(&aumid)
                    );
                }
                continue;
            };
            if original.is_empty() {
                // 原本无 AUMID：清除属性（VT_EMPTY）
                match appid::clear_aumid(*hwnd) {
                    Ok(()) => {
                        cleared += 1;
                        println!(
                            "0x{} {} -> <cleared>",
                            winutil::hwnd_hex(*hwnd),
                            aumid
                        );
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
        }
        println!();
        println!(
            "restore summary: restored={restored} cleared={cleared} skipped(no marker)={skipped} failed={failed}"
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
