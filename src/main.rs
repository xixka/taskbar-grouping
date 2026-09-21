//! tbg-lite — Windows 任务栏分组控制（零注入路线 B+）
//!
//! 当前任务：5（API 语义 PoC）。CLI 提供 `inspect` / `set`，用于在真实
//! Windows 上手工验证 `SHGetPropertyStoreForWindow` + `PKEY_AppUserModel_ID`
//! 的读写语义（docs/plan.md §7 Phase 0b）。

mod appid;
mod winutil;

use std::process::ExitCode;

use windows::Win32::Foundation::HWND;

const HELP: &str = "\
tbg-lite — zero-injection Windows taskbar grouping controller

USAGE:
    tbg-lite [--version | --help]
    tbg-lite inspect [--hwnd <HEX>] [--all]
    tbg-lite set --hwnd <HEX> (--suffix | --value <APPID>)

COMMANDS:
    inspect   list top-level windows and their AppUserModelID
              --hwnd <HEX>   show one window in detail
              --all          also include hidden / tool windows
    set       rewrite one window's AppUserModelID (docs/plan.md task 5)
              --suffix       append the per-window ungroup marker (~TBG~w<HWND>)
              --value <ID>   set an exact AppUserModelID

STATUS:
    task 5 (API semantics PoC) — see docs/plan.md §7 Phase 0b
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
            format!("{before}{}{}", appid::SUFFIX_MARKER, winutil::hwnd_hex(hwnd))
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
