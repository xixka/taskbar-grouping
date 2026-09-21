//! 事件驱动 AUMID 改写 PoC（任务 6，docs/plan.md §7 Phase 0b-(6)）。
//!
//! `SetWinEventHook`（EVENT_OBJECT_CREATE / EVENT_OBJECT_SHOW /
//! EVENT_OBJECT_DESTROY，WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS）
//! 监听顶层窗口事件——零注入：回调只在本进程自己的消息泵里执行。
//! 对"看起来会在任务栏出现按钮"的新窗口自动追加每窗口去分组后缀
//! （docs/plan.md §4 路线 B+）。EVENT_OBJECT_DESTROY 仅用于簿记
//! （窗口销毁后移出已处理集合，防 HWND 复用污染统计）。
//!
//! 退出时输出统计并全量复扫，给出"漏检 / 被应用回写"数据，对应计划中
//! "高频连开 50 窗口压测"的观测输入。竞态（先并组后跳变）本身需在
//! 真实 Windows 上人工观察（运行时行为待 Windows 实测）。

use std::cell::RefCell;
use std::collections::HashSet;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    EVENT_OBJECT_CREATE, EVENT_OBJECT_DESTROY, EVENT_OBJECT_SHOW, MSG, MWMO_INPUTAVAILABLE,
    MsgWaitForMultipleObjectsEx, OBJID_WINDOW, PeekMessageW, PM_REMOVE, QS_ALLINPUT,
    WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
};

use crate::appid;
use crate::winutil;

pub(crate) struct WatchOptions {
    /// 监听时长；0 = 直至 Ctrl+C（控制台默认处理为强杀进程，无统计输出）。
    pub(crate) duration: Duration,
    /// 只记录、不写入 AUMID。
    pub(crate) dry_run: bool,
    /// 额外输出被跳过的窗口与原因（not-app-window 噪声除外）。
    pub(crate) verbose: bool,
}

/// 观测统计（对应 docs/plan.md §7 Phase 0b-(6) 的压测数据需求）。
#[derive(Default)]
struct Stats {
    events_create: u64,
    events_show: u64,
    events_destroy_tracked: u64,
    candidates: u64,
    rewritten: u64,
    dry_run_hits: u64,
    truncated: u64,
    write_fail: u64,
    skipped_dupe: u64,
    skipped_baseline: u64,
    skipped_not_app_window: u64,
    skipped_shell: u64,
    skipped_cloaked: u64,
    skipped_read_fail: u64,
    skipped_already_marked: u64,
}

struct WatcherState {
    dry_run: bool,
    verbose: bool,
    started: Instant,
    /// watch 启动前就存在的应用窗口（存量，不处理、不计漏检）。
    baseline: HashSet<usize>,
    baseline_count: u64,
    /// 本会话已处理（改写 / 判定跳过）的新窗口，以 HWND 数值为键。
    handled: HashSet<usize>,
    stats: Stats,
}

thread_local! {
    /// 回调只会在安装钩子的线程（即本模块 `run` 所在线程）的消息泵里触发，
    /// 因此用 thread_local 承载状态即可，无需跨线程同步。
    static WATCHER: RefCell<Option<WatcherState>> = RefCell::new(None);
}

unsafe extern "system" fn win_event_cb(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    idobject: i32,
    idchild: i32,
    _ideventthread: u32,
    _dwmseventtime: u32,
) {
    WATCHER.with(|cell| {
        if let Some(state) = cell.borrow_mut().as_mut() {
            state.on_event(event, hwnd, idobject, idchild);
        }
    });
}

impl WatcherState {
    fn new(dry_run: bool, verbose: bool) -> Self {
        Self {
            dry_run,
            verbose,
            started: Instant::now(),
            baseline: HashSet::new(),
            baseline_count: 0,
            handled: HashSet::new(),
            stats: Stats::default(),
        }
    }

    fn ts(&self) -> String {
        format!("[{:8.3}s]", self.started.elapsed().as_secs_f32())
    }

    /// 启动时刻的存量应用窗口快照（在钩子安装之后取样：这期间新窗口的
    /// 事件已能入队，随后会在消息泵里照常处理，不会两头漏）。
    unsafe fn snapshot_baseline(&mut self) {
        for hwnd in winutil::enum_top_level_windows() {
            if winutil::is_app_window(hwnd)
                && !winutil::is_shell_window(hwnd)
                && !winutil::is_cloaked(hwnd)
            {
                self.baseline.insert(hwnd.0 as usize);
            }
        }
        self.baseline_count = self.baseline.len() as u64;
    }

    fn on_event(&mut self, event: u32, hwnd: HWND, idobject: i32, idchild: i32) {
        // 只关心"窗口对象自身"的事件，忽略子元素 / 子窗口事件
        if idobject != OBJID_WINDOW.0 || idchild != 0 || hwnd.0.is_null() {
            return;
        }
        let key = hwnd.0 as usize;
        let name = match event {
            EVENT_OBJECT_CREATE => {
                self.stats.events_create += 1;
                "CREATE"
            }
            EVENT_OBJECT_SHOW => {
                self.stats.events_show += 1;
                "SHOW"
            }
            EVENT_OBJECT_DESTROY => {
                if self.handled.remove(&key) | self.baseline.remove(&key) {
                    self.stats.events_destroy_tracked += 1;
                }
                return;
            }
            _ => return,
        };
        if self.handled.contains(&key) {
            // 已处理过（CREATE 处理后紧随的 SHOW 等）
            self.stats.skipped_dupe += 1;
            return;
        }
        if self.baseline.contains(&key) {
            // 存量窗口（含最小化/恢复、重新显示触发的 SHOW）：不处理，
            // 保证"新开 N 窗口"压测的统计纯净
            self.stats.skipped_baseline += 1;
            if self.verbose {
                self.log_skip(name, hwnd, "baseline window (existed before watch)");
            }
            return;
        }
        unsafe { self.consider(name, hwnd, key) };
    }

    unsafe fn consider(&mut self, name: &str, hwnd: HWND, key: usize) {
        if !winutil::is_app_window(hwnd) {
            // CREATE 早期窗口常尚不可见 / 无标题：不进 handled，
            // 待 SHOW 事件再评估
            self.stats.skipped_not_app_window += 1;
            return;
        }
        if winutil::is_shell_window(hwnd) {
            self.stats.skipped_shell += 1;
            if self.verbose {
                self.log_skip(name, hwnd, "shell window");
            }
            self.handled.insert(key);
            return;
        }
        if winutil::is_cloaked(hwnd) {
            // 被 cloak（如挂起的 UWP）：当前没有任务栏按钮；不进 handled，
            // 待 uncloak 后的 SHOW 事件再评估
            self.stats.skipped_cloaked += 1;
            if self.verbose {
                self.log_skip(name, hwnd, "cloaked");
            }
            return;
        }
        let aumid = match appid::get_aumid(hwnd) {
            Ok(a) => a,
            Err(e) => {
                self.stats.skipped_read_fail += 1;
                if self.verbose {
                    self.log_skip(name, hwnd, &format!("AUMID read failed: {e}"));
                }
                return;
            }
        };
        if aumid.contains(appid::SUFFIX_MARKER) {
            // 已带标记（多为上次会话遗留）：视为已处理
            self.stats.skipped_already_marked += 1;
            if self.verbose {
                self.log_skip(name, hwnd, "already marked");
            }
            self.handled.insert(key);
            return;
        }
        self.stats.candidates += 1;
        let suffixed = appid::suffixed_aumid(&aumid, hwnd);
        if suffixed.truncated {
            self.stats.truncated += 1;
        }
        if self.dry_run {
            self.stats.dry_run_hits += 1;
            println!(
                "{} {} {} [dry-run] aumid={:?} -> {:?}",
                self.ts(),
                name,
                fmt_window(hwnd),
                winutil::shown_aumid(&aumid),
                suffixed.value
            );
            self.handled.insert(key);
            return;
        }
        let t0 = Instant::now();
        match appid::set_aumid(hwnd, &suffixed.value) {
            Ok(()) => {
                self.stats.rewritten += 1;
                println!(
                    "{} {} {} aumid={:?} -> {:?} (write {:.1}ms)",
                    self.ts(),
                    name,
                    fmt_window(hwnd),
                    winutil::shown_aumid(&aumid),
                    suffixed.value,
                    t0.elapsed().as_secs_f64() * 1000.0
                );
            }
            Err(e) => {
                self.stats.write_fail += 1;
                println!(
                    "{} {} {} aumid={:?} write FAILED: {e}",
                    self.ts(),
                    name,
                    fmt_window(hwnd),
                    aumid
                );
                // 不进 handled：窗口后续的 SHOW 事件可重试
                return;
            }
        }
        self.handled.insert(key);
    }

    fn log_skip(&self, name: &str, hwnd: HWND, reason: &str) {
        println!(
            "{} {} {} skip: {reason}",
            self.ts(),
            name,
            unsafe { fmt_window(hwnd) }
        );
    }

    /// 退出时的全量复扫与统计输出：
    /// - 漏检 = 新出现的应用窗口在会话结束时仍无标记；
    /// - 回退 = 本会话改写过、但标记已消失（应用回写了自身 AUMID 的证据）。
    unsafe fn final_scan_and_report(&mut self) {
        println!();
        println!("==== watch stats (task 6, docs/plan.md Phase 0b-(6)) ====");
        println!(
            "events     : CREATE={} SHOW={} DESTROY(tracked)={}",
            self.stats.events_create, self.stats.events_show, self.stats.events_destroy_tracked
        );
        println!("baseline app windows at start : {}", self.baseline_count);
        println!("new-window candidates         : {}", self.stats.candidates);
        println!(
            "skips      : dupe={} baseline={} not_app_window={} shell={} cloaked={} read_fail={} already_marked={}",
            self.stats.skipped_dupe,
            self.stats.skipped_baseline,
            self.stats.skipped_not_app_window,
            self.stats.skipped_shell,
            self.stats.skipped_cloaked,
            self.stats.skipped_read_fail,
            self.stats.skipped_already_marked
        );
        if self.dry_run {
            println!(
                "dry-run    : {} candidates logged, nothing written",
                self.stats.dry_run_hits
            );
            println!("note: taskbar button races/jumps must be observed manually on real Windows");
            return;
        }
        println!(
            "rewritten  : {} (original AUMID truncated: {})",
            self.stats.rewritten, self.stats.truncated
        );
        println!("write fails: {}", self.stats.write_fail);

        let mut missed: Vec<String> = Vec::new();
        let mut reverted: Vec<String> = Vec::new();
        let mut alive_marked: u64 = 0;
        let mut scan_read_fail: u64 = 0;
        for hwnd in winutil::enum_top_level_windows() {
            let key = hwnd.0 as usize;
            let was_handled = self.handled.contains(&key);
            let is_new = !self.baseline.contains(&key);
            if !was_handled && !is_new {
                // 存量且本会话未处理：与任务 6 无关
                continue;
            }
            let aumid = match appid::get_aumid(hwnd) {
                Ok(a) => a,
                Err(_) => {
                    scan_read_fail += 1;
                    continue;
                }
            };
            if aumid.contains(appid::SUFFIX_MARKER) {
                alive_marked += 1;
            } else if was_handled {
                reverted.push(format!("{} aumid={:?}", fmt_window(hwnd), aumid));
            } else if is_new
                && winutil::is_app_window(hwnd)
                && !winutil::is_shell_window(hwnd)
                && !winutil::is_cloaked(hwnd)
            {
                missed.push(format!("{} aumid={:?}", fmt_window(hwnd), aumid));
            }
        }
        println!("alive with marker                 : {alive_marked}");
        println!("reverted (app rewrote its AUMID)  : {}", reverted.len());
        for r in &reverted {
            println!("  reverted: {r}");
        }
        println!("missed (new app w/o marker)       : {}", missed.len());
        for m in &missed {
            println!("  missed: {m}");
        }
        if scan_read_fail > 0 {
            println!("(scan: {scan_read_fail} windows unreadable)");
        }
        println!("note: taskbar button races/jumps must be observed manually on real Windows");
    }
}

/// 单行窗口摘要（日志 / 漏检清单用）。
unsafe fn fmt_window(hwnd: HWND) -> String {
    format!(
        "0x{} pid={} class={} title={:?}",
        winutil::hwnd_hex(hwnd),
        winutil::window_pid(hwnd),
        clip(&winutil::class_name(hwnd), 32),
        clip(&winutil::window_text(hwnd), 40)
    )
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('~');
        t
    }
}

pub(crate) fn run(opts: WatchOptions) -> Result<(), String> {
    // AUMID 读写（IPropertyStore）要求本线程已初始化 COM
    let _com = winutil::ComGuard::init()?;

    // 1. 线程本地状态先行就位（回调里 if-let 判空，绝不 panic）。
    //    注意：基线快照必须在钩子安装之后做，见下方第 3 步。
    WATCHER.with(|cell| {
        *cell.borrow_mut() = Some(WatcherState::new(opts.dry_run, opts.verbose));
    });

    // 2. 安装 winevent 钩子（零注入：WINEVENT_OUTOFCONTEXT 回调只在本进程执行）
    let flags = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;
    let events = [EVENT_OBJECT_CREATE, EVENT_OBJECT_SHOW, EVENT_OBJECT_DESTROY];
    let mut hooks: Vec<HWINEVENTHOOK> = Vec::with_capacity(events.len());
    for &ev in &events {
        let h = unsafe { SetWinEventHook(ev, ev, HMODULE::default(), Some(win_event_cb), 0, 0, flags) };
        if h.is_invalid() {
            for installed in &hooks {
                unsafe { let _ = UnhookWinEvent(*installed); };
            }
            WATCHER.with(|cell| *cell.borrow_mut() = None);
            return Err(format!("watch: SetWinEventHook failed for event {ev:#06x}"));
        }
        hooks.push(h);
    }

    println!("tbg-lite watch (task 6): SetWinEventHook CREATE/SHOW/DESTROY, out-of-context, skip-own-process");
    println!("note: DESTROY hook is bookkeeping only (tracked-window cleanup)");
    if opts.duration.is_zero() {
        println!("duration: until Ctrl+C (hard exit, no stats printed)");
    } else {
        println!("duration: {:?} (Ctrl+C = hard exit without stats)", opts.duration);
    }
    if opts.dry_run {
        println!("mode: dry-run (no AUMID writes)");
    }
    if opts.verbose {
        println!("mode: verbose skips");
    }
    println!();

    // 3. 基线快照：此刻已满足“应用窗口”判定的视为启动前存量（不处理、不计漏检）。
    //    必须在钩子安装之后取样：这期间新窗口的事件已能入队，不会两头漏。
    WATCHER.with(|cell| {
        if let Some(state) = cell.borrow_mut().as_mut() {
            unsafe { state.snapshot_baseline() };
        }
    });

    // 4. 消息泵：winevent 回调在 PeekMessage 检索期间由系统调用；
    //    MsgWaitForMultipleObjectsEx 让消息一到就醒来（压测时延观察更真实）
    let deadline = (!opts.duration.is_zero()).then(|| Instant::now() + opts.duration);
    let mut msg = MSG::default();
    loop {
        let wait_ms = match deadline {
            Some(d) => {
                let now = Instant::now();
                if now >= d {
                    break;
                }
                let remain = d - now;
                remain.as_millis().min(u32::MAX as u128) as u32
            }
            None => u32::MAX, // INFINITE
        };
        unsafe {
            let _ = MsgWaitForMultipleObjectsEx(None, wait_ms, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
            while PeekMessageW(&mut msg, HWND::default(), 0, 0, PM_REMOVE).as_bool() {
                // 线程消息无窗口过程，检索即消费；winevent 回调在其中触发
            }
        }
    }

    // 5. 摘钩子
    for h in &hooks {
        if !unsafe { UnhookWinEvent(*h).as_bool() } {
            eprintln!("watch: warning: UnhookWinEvent failed");
        }
    }

    // 6. 退出扫描与统计输出
    WATCHER.with(|cell| {
        if let Some(mut state) = cell.borrow_mut().take() {
            unsafe { state.final_scan_and_report() };
        }
    });
    Ok(())
}
