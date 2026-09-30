//! `tbg-inject` —— 注入版宿主（任务 35，docs/plan.md v2 §5）。
//!
//! CLI：`inject [--strategy ungroup|group] [--group <NAME>]` / `stop` /
//! `status`；无参数启动进入双语交互菜单（`src/menu.rs`，模式对齐主版
//! 任务 14/29：菜单驱动 + `L` 切换语言 + stdin BOM 容错 + `[0]` 退出，
//! 全程无 Ctrl+C）。
//!
//! 与主版 tbg-lite 的边界（plan v2 §0-6 / AGENTS.md 红线）：
//! - **无自启注册**：注入属高敏行为，仅用户显式启动；
//! - **无还原表**：不写窗口真实属性，摘钩即回原状；
//! - 与 tbg-lite watch **互斥**：同一任务栏二选一。

mod injector;
mod menu;
mod sharedmem;

use std::path::PathBuf;

use tbg_proto::{
    SharedState, INIT_ALREADY, INIT_OK, MODE_GROUP, MODE_UNGROUP, STATE_ACTIVE, STATE_UNLOADED,
    STOP_OK,
};

fn main() {
    std::process::exit(real_main());
}

fn real_main() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        return menu::run();
    }
    match args[0].as_str() {
        "--version" => {
            println!("tbg-inject {}", env!("CARGO_PKG_VERSION"));
            0
        }
        "--help" | "-h" => {
            print_help();
            0
        }
        "inject" => cmd_inject(&args[1..]),
        "stop" => cmd_stop(),
        "status" => cmd_status(),
        other => {
            eprintln!("tbg-inject: unknown command '{other}'");
            eprintln!("usage: tbg-inject [inject [--strategy ungroup|group] [--group <NAME>] | stop | status]");
            2
        }
    }
}

fn print_help() {
    println!(
        "tbg-inject {} — taskbar grouping controller (injection edition, route A)

USAGE:
    tbg-inject                          interactive menu
    tbg-inject inject                   inject tbg_hook.dll into explorer (ungroup, default)
    tbg-inject inject --strategy group --group <NAME>
                                        inject with shared AUMID TBG.Group.<NAME>
    tbg-inject stop                     unhook + unload (stats printed)
    tbg-inject status                   hook state + interception counters
    tbg-inject --version | --help

NOTES:
    tbg_hook.dll must sit next to this exe.
    Mutually exclusive with tbg-lite watch (one taskbar, one edition).
    No autostart, no restore table: unhooking restores native grouping.
    Expect antivirus heuristics to flag this edition (real injection
    behavior, not a false positive in the usual sense).",
        env!("CARGO_PKG_VERSION")
    );
}

/// 注入命令。返回进程退出码（0 成功 / 1 运行时错误 / 2 用法错误）。
fn cmd_inject(rest: &[String]) -> i32 {
    let mut group: Option<String> = None;
    let mut want_group = false;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--strategy" => {
                i += 1;
                match rest.get(i).map(|s| s.as_str()) {
                    Some("ungroup") => want_group = false,
                    Some("group") => want_group = true,
                    _ => {
                        eprintln!("usage: inject: --strategy expects 'ungroup' or 'group'");
                        return 2;
                    }
                }
            }
            "--group" => {
                i += 1;
                match rest.get(i) {
                    Some(v) => group = Some(v.clone()),
                    None => {
                        eprintln!("usage: inject: --group expects a NAME");
                        return 2;
                    }
                }
            }
            other => {
                eprintln!("usage: inject: unknown argument '{other}'");
                return 2;
            }
        }
        i += 1;
    }
    if want_group && group.is_none() {
        eprintln!("usage: inject: --strategy group requires --group <NAME>");
        return 2;
    }
    if !want_group && group.is_some() {
        eprintln!("usage: inject: --group requires --strategy group");
        return 2;
    }
    if let Some(name) = &group {
        if let Err(e) = validate_group_name(name) {
            eprintln!("usage: inject: {e}");
            return 2;
        }
    }
    match do_inject(group.as_deref()) {
        Ok(msg) => {
            println!("{msg}");
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

/// 组名校验（与主版 watch/pin 同规则：字符集 [A-Za-z0-9._-]，1..=32）。
fn validate_group_name(name: &str) -> Result<(), String> {
    let n = name.chars().count();
    if n == 0 || n > 32 {
        return Err("group name must be 1..=32 characters".into());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
    {
        return Err("group name charset is [A-Za-z0-9._-]".into());
    }
    Ok(())
}

/// 注入实现（CLI 与菜单共用）。
pub(crate) fn do_inject(group: Option<&str>) -> Result<String, String> {
    unsafe {
        let pid = injector::explorer_pid()
            .ok_or("inject: explorer shell window not found (is explorer running?)")?;
        let dll = dll_path()?;
        if !dll.exists() {
            return Err(format!(
                "inject: {} not found — keep tbg_hook.dll next to tbg-inject.exe",
                dll.display()
            ));
        }
        let view = sharedmem::SharedView::ensure()?;
        let section_existed = view.existed;
        let s = view.as_mut();

        let state = s.state.load(std::sync::atomic::Ordering::Acquire);
        if state == STATE_ACTIVE && s.explorer_pid.load(std::sync::atomic::Ordering::Relaxed) == pid
        {
            return Err("inject: already injected into this explorer — run `tbg-inject stop` first".into());
        }

        // 写配置（enabled 先 0，init 成功后再启用）
        s.enabled.store(0, std::sync::atomic::Ordering::Release);
        s.mode = if group.is_some() {
            MODE_GROUP
        } else {
            MODE_UNGROUP
        };
        let full = format!("TBG.Group.{}", group.unwrap_or_default());
        let wide: Vec<u16> = full.encode_utf16().chain(std::iter::once(0)).collect();
        if wide.len() > s.group_aumid.len() {
            return Err("inject: group AUMID too long".into());
        }
        s.group_aumid = [0; 48];
        s.group_aumid[..wide.len()].copy_from_slice(&wide);
        s.explorer_pid.store(pid, std::sync::atomic::Ordering::Relaxed);

        let code = injector::call_remote_export(pid, &dll, b"tbg_hook_init\0")?;
        if code != INIT_OK && code != INIT_ALREADY {
            // 诊断面板（任务 36 修复轮 2：一次运行拿到全部判据）
            return Err(format!(
                "inject: hook init failed (code {code}) — state={} err={} gen={} scanned={} patched={} delay={} gpa={} self={} names={} section={} dll={}",
                s.state_name(),
                s.err,
                s.generation.load(std::sync::atomic::Ordering::Relaxed),
                s.modules_scanned,
                s.slots_patched,
                s.delay_slots,
                s.gpa_slots,
                s.self_slots,
                s.names_seen,
                if section_existed { "pre-existing" } else { "fresh" },
                dll.display(),
            ));
        }
        let st = s.state.load(std::sync::atomic::Ordering::Acquire);
        if st != STATE_ACTIVE {
            return Err(format!(
                "inject: hook state is {} after init (see `tbg-inject status`)",
                s.state_name()
            ));
        }
        s.enabled.store(1, std::sync::atomic::Ordering::Release);

        let mut msg = format!(
            "inject: ok — explorer pid {pid}, generation {}\n{}",
            s.generation.load(std::sync::atomic::Ordering::Relaxed),
            fmt_hook_line(s)
        );
        msg.push_str("note   : grouping now served in-process; `tbg-inject stop` unhooks + unloads");
        Ok(msg)
    }
}

/// 停止命令（摘钩 + 卸载 + 统计）。
fn cmd_stop() -> i32 {
    match do_stop() {
        Ok(msg) => {
            println!("{msg}");
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

pub(crate) fn do_stop() -> Result<String, String> {
    unsafe {
        let view = sharedmem::SharedView::ensure()?;
        let s = view.as_mut();
        let state = s.state.load(std::sync::atomic::Ordering::Acquire);
        if state != STATE_ACTIVE {
            return Err(format!(
                "stop: not injected (state: {})",
                s.state_name()
            ));
        }
        let pid = s.explorer_pid.load(std::sync::atomic::Ordering::Relaxed);
        let pid_now = injector::explorer_pid();
        if pid_now != Some(pid) {
            // explorer 已重启：DLL 随旧进程消亡，只需复位状态
            s.state.store(STATE_UNLOADED, std::sync::atomic::Ordering::Release);
            s.enabled.store(0, std::sync::atomic::Ordering::Release);
            return Ok(format!(
                "stop: stale state cleared (explorer {pid} is gone — hook died with it)"
            ));
        }
        let dll = dll_path()?;
        s.enabled.store(0, std::sync::atomic::Ordering::Release);
        let code = injector::call_remote_export(pid, &dll, b"tbg_hook_stop\0")?;
        if code != STOP_OK {
            return Err(format!("stop: hook stop returned {code}"));
        }
        let mut msg = format!(
            "stop: ok — hooks removed, dll unloaded (generation {})\n",
            s.generation.load(std::sync::atomic::Ordering::Relaxed)
        );
        msg.push_str(&fmt_traffic_line(s));
        Ok(msg)
    }
}

/// 状态命令（非侵入：可重复执行）。
fn cmd_status() -> i32 {
    unsafe {
        let pid = injector::explorer_pid();
        let view = sharedmem::SharedView::ensure().map_err(|e| e).ok();
        let out = match view {
            Some(v) => fmt_status(v.as_ref(), pid, v.existed),
            None => format!(
                "edition : tbg-inject (route A, in-process hook)\nexplorer: {}\nstate   : detached (no shared section)\n",
                fmt_pid(pid)
            ),
        };
        print!("{out}");
        0
    }
}

pub(crate) fn do_status() -> String {
    unsafe {
        let pid = injector::explorer_pid();
        match sharedmem::SharedView::ensure() {
            Ok(v) => fmt_status(v.as_ref(), pid, v.existed),
            Err(e) => format!("status: {e}"),
        }
    }
}

fn dll_path() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("inject: current_exe failed: {e}"))?;
    Ok(exe
        .parent()
        .map(|d| d.join("tbg_hook.dll"))
        .unwrap_or_else(|| PathBuf::from("tbg_hook.dll")))
}

fn fmt_pid(pid: Option<u32>) -> String {
    match pid {
        Some(p) => format!("pid {p}"),
        None => "not running (no shell window)".into(),
    }
}

/// `hook :` 行（注入/状态共用；任务 36 修复轮 2 起含三层拦截面明细）。
pub(crate) fn fmt_hook_line(s: &SharedState) -> String {
    let mut line = format!(
        "hook   : modules scanned={} patched={} delay={} gpa={} self={} names={}\n",
        s.modules_scanned,
        s.slots_patched,
        s.delay_slots,
        s.gpa_slots,
        s.self_slots,
        s.names_seen
    );
    let names = patched_names(s);
    if !names.is_empty() {
        line.push_str(&format!("          {}\n", names));
    }
    line
}

pub(crate) fn fmt_traffic_line(s: &SharedState) -> String {
    format!(
        "traffic: calls={} wrapped={} aumid-served={}\n",
        s.calls_seen.load(std::sync::atomic::Ordering::Relaxed),
        s.stores_wrapped.load(std::sync::atomic::Ordering::Relaxed),
        s.aumid_served.load(std::sync::atomic::Ordering::Relaxed)
    )
}

fn patched_names(s: &SharedState) -> String {
    let mut parts: Vec<String> = Vec::new();
    for m in s.patched.iter() {
        let len = m.name.iter().position(|&b| b == 0).unwrap_or(m.name.len());
        if len == 0 {
            continue;
        }
        parts.push(format!(
            "[{} x{}]",
            String::from_utf8_lossy(&m.name[..len]),
            m.slots
        ));
    }
    parts.join(" ")
}

/// 完整状态输出（CI 断言锚点：`state   :`/`hook`/`patched=`/`calls=` 等）。
pub(crate) fn fmt_status(s: &SharedState, pid: Option<u32>, existed: bool) -> String {
    let mut out = String::new();
    out.push_str("edition : tbg-inject (route A, in-process hook)\n");
    out.push_str(&format!(
        "section: state={} existed={} self_module={:#x} size={}\n",
        if s.magic == tbg_proto::MAGIC { "valid" } else { "invalid" },
        if existed { "yes" } else { "no (created fresh)" },
        s.self_module,
        s.size,
    ));
    out.push_str(&format!("explorer: {}\n", fmt_pid(pid)));
    let state = s.state.load(std::sync::atomic::Ordering::Acquire);
    let gen = s.generation.load(std::sync::atomic::Ordering::Relaxed);
    out.push_str(&format!(
        "state   : {} (generation {})\n",
        s.state_name(),
        gen
    ));
    if state == STATE_ACTIVE && pid.is_some() && s.explorer_pid.load(std::sync::atomic::Ordering::Relaxed) != pid.unwrap() {
        out.push_str("          (stale: explorer pid changed since inject)\n");
    }
    out.push_str(&fmt_hook_line(s));
    out.push_str(&fmt_traffic_line(s));
    out.push_str(&fmt_proxy_line(s));
    out.push_str(&format!(
        "config  : strategy={} enabled={}\n",
        if s.mode == MODE_GROUP { "group" } else { "ungroup" },
        s.enabled.load(std::sync::atomic::Ordering::Relaxed)
    ));
    out
}

/// 代理行为插桩行（任务 36 修复轮 7：任务栏对委托对象的真实调用序列）。
pub(crate) fn fmt_proxy_line(s: &SharedState) -> String {
    let rd = |a: &std::sync::atomic::AtomicU32| a.load(std::sync::atomic::Ordering::Relaxed);
    format!(
        "proxy  : qi(store={} cache={} other={} iid={}) riid(other={} last={})\n\
          m(count={} at={} value={} andstate={} set={} commit={} rel={}) key={}\n",
        rd(&s.qi_store),
        rd(&s.qi_cache),
        rd(&s.qi_other),
        guid_hex(&s.qi_last_iid),
        rd(&s.riid_other),
        guid_hex(&s.riid_last),
        rd(&s.m_getcount),
        rd(&s.m_getat),
        rd(&s.m_getvalue),
        rd(&s.m_getandstate),
        rd(&s.m_setvalue),
        rd(&s.m_commit),
        rd(&s.m_release),
        key_hex(&s.last_key),
    )
}

/// 16 字节内存布局 GUID → 标准字符串（全 0 → "-"）。
fn guid_hex(b: &[u8]) -> String {
    if b.len() < 16 || b[..16].iter().all(|&x| x == 0) {
        return "-".into();
    }
    let d1 = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    let d2 = u16::from_le_bytes([b[4], b[5]]);
    let d3 = u16::from_le_bytes([b[6], b[7]]);
    let tail: String = b[8..16].iter().map(|x| format!("{:02X}", x)).collect();
    format!("{{{:08X}-{:04X}-{:04X}-{}-{}}}", d1, d2, d3, &tail[..4], &tail[4..])
}

/// 20 字节 PROPERTYKEY（fmtid + pid）→ 字符串。
fn key_hex(b: &[u8; 20]) -> String {
    if b.iter().all(|&x| x == 0) {
        return "-".into();
    }
    format!(
        "{} pid={}",
        guid_hex(&b[..16]),
        u32::from_le_bytes([b[16], b[17], b[18], b[19]])
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;
    use tbg_proto::{MODE_GROUP, STATE_ACTIVE};

    #[test]
    fn group_name_rules() {
        assert!(validate_group_name("smoke").is_ok());
        assert!(validate_group_name("a.b-c_d").is_ok());
        assert!(validate_group_name("").is_err());
        assert!(validate_group_name(&"x".repeat(33)).is_err());
        assert!(validate_group_name("a b").is_err());
        assert!(validate_group_name("组").is_err());
    }

    #[test]
    fn status_format_anchors() {
        let mut s = SharedState::new_default();
        s.state = AtomicU32::new(STATE_ACTIVE);
        s.modules_scanned = 142;
        s.slots_patched = 4;
        s.delay_slots = 2;
        s.gpa_slots = 12;
        s.self_slots = 1;
        s.names_seen = 5150;
        s.patched[0].name[..6].copy_from_slice(b"shell3"); // 演示名（截断展示）
        s.patched[0].name[6] = b'2';
        s.patched[0].name[7] = 0;
        s.patched[0].slots = 1;
        s.calls_seen = AtomicU32::new(214);
        s.aumid_served = AtomicU32::new(63);
        s.mode = MODE_GROUP;
        let out = fmt_status(&s, Some(4816), false);
        assert!(out.contains("state   : active (generation 0)"), "{out}");
        assert!(out.contains("section: state=valid existed=no (created fresh)"), "{out}");
        assert!(out.contains("hook   : modules scanned=142 patched=4 delay=2 gpa=12 self=1 names=5150"), "{out}");
        assert!(out.contains("calls=214"), "{out}");
        assert!(out.contains("aumid-served=63"), "{out}");
        assert!(out.contains("proxy  : qi(store="), "{out}");
        assert!(out.contains("m(count="), "{out}");
        assert!(out.contains("strategy=group"), "{out}");
        assert!(out.contains("[shell32 x1]"), "{out}");
    }

    #[test]
    fn stop_not_injected_message() {
        let s = SharedState::new_default();
        assert_eq!(s.state_name(), "detached");
    }
}
