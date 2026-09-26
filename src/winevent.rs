//! 事件驱动 AUMID 改写 PoC（任务 6/8，docs/plan.md §7 Phase 0b-(6)(8)）。
//!
//! `SetWinEventHook`（EVENT_OBJECT_CREATE / EVENT_OBJECT_SHOW /
//! EVENT_OBJECT_DESTROY，WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS）
//! 监听顶层窗口事件——零注入：回调只在本进程自己的消息泵里执行。
//! 对"看起来会在任务栏出现按钮"的新窗口按 `--strategy` 指定的线路改写
//! （docs/plan.md §4 路线 B+）：
//!
//! - 线路一 `ungroup`：每窗口后缀 `~TBG~w<HWND>`，取消任务栏分组（任务 6）；
//! - 线路二 `group`：全部候选窗口统一改写为共享 AUMID `TBG.Group.<name>`，
//!   自定义分组（任务 8）；原值落盘 `tbg-restore.tsv` 供 `restore` 复原。
//!
//! EVENT_OBJECT_DESTROY 仅用于簿记（窗口销毁后移出已处理集合并丢弃
//! 线路二的还原映射条目，防 HWND 复用污染统计与还原）。
//!
//! 任务 13（docs/plan.md v2 §3）：启动扫存量窗口——watch 一启动就把
//! 已存在的应用窗口也按当前线路改写（对齐 Windhawk mod 默认行为
//! "开启即全量取消分组"），而非只管新窗口；两条线路同等生效。
//!
//! 任务 14（docs/plan.md v2 §3，2026-09-22 维护者改版）：交互菜单模式
//! 下 watch 运行在后台线程，通过 `Arc<AtomicBool>` 停止标志优雅退出
//! （取代原 Ctrl+C 方案——维护者指示不需要 Ctrl+C，退出走菜单）：
//! 置位后消息泵在 ≤1s 内退出、摘钩、终扫并输出统计。
//!
//! 退出时输出统计并全量复扫，给出"漏检 / 被应用回写"数据，对应计划中
//! "高频连开 50 窗口压测"的观测输入。竞态（先并组后跳变）本身需在
//! 真实 Windows 上人工观察（运行时行为待 Windows 实测；任务 9 起部分
//! 行为断言已可由 CI 冒烟代行）。

use std::cell::RefCell;
use std::collections::HashSet;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    EVENT_OBJECT_CREATE, EVENT_OBJECT_DESTROY, EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_SHOW,
    GetShellWindow, GetWindowThreadProcessId, MSG, MWMO_INPUTAVAILABLE,
    MsgWaitForMultipleObjectsEx, OBJID_WINDOW, PeekMessageW, PM_REMOVE, QS_ALLINPUT,
    WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
};

use crate::appid;
use crate::health;
use crate::restoremap::RestoreMap;
use crate::ringlog::RingLog;
use crate::winutil;

/// watch 的两条实现线路（任务 8，`--strategy` 切换；
/// docs/plan.md §4 路线 B+：每窗口后缀 = 取消分组；共享 AUMID = 自定义组）。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum WatchStrategy {
    /// 线路一：每窗口后缀（`~TBG~w<HWND>`），让每个窗口落到独立的
    /// 任务栏组 = 取消分组。
    Ungroup,
    /// 线路二：全部候选窗口改写为共享 AUMID（`TBG.Group.<name>`），
    /// 不同来源的窗口合成一个任务栏组 = 自定义分组。
    Group,
}

impl WatchStrategy {
    fn label(&self) -> &'static str {
        match self {
            Self::Ungroup => "ungroup (per-window suffix, line 1)",
            Self::Group => "group (shared AUMID, line 2)",
        }
    }
}

pub(crate) struct WatchOptions {
    /// 监听时长；0 = 直至停止（CLI 参数模式下控制台默认处理为强杀进程，
    /// 无统计输出；交互菜单模式下由停止标志优雅退出，见 `stop`）。
    pub(crate) duration: Duration,
    /// 只记录、不写入 AUMID。
    pub(crate) dry_run: bool,
    /// 额外输出被跳过的窗口与原因（not-app-window 噪声除外）。
    pub(crate) verbose: bool,
    /// 改写线路（任务 8）。
    pub(crate) strategy: WatchStrategy,
    /// 线路二的组名（`--group`；strategy == Group 时必有，main 层已校验）。
    pub(crate) group_name: Option<String>,
    /// 任务 14：外部停止标志（交互菜单模式传入；CLI 参数模式为 None）。
    /// Some(_) 时：即使无 deadline 消息泵也以 1s 上限轮询该标志；置位 →
    /// 摘钩 + 终扫 + 统计输出后正常返回（优雅退出，取代 Ctrl+C 方案）。
    /// None 时行为与此前完全一致。
    pub(crate) stop: Option<Arc<AtomicBool>>,
    /// 任务 20：环形日志开关（`watch --log`；默认关）。开启后关键事件
    /// （启动/停止/扫存量/explorer 重启/重扫/熔断）落入数据目录
    /// tbg.log（256 KiB 上限，超限截半，原子替换）。
    pub(crate) ring_log: bool,
}

/// 观测统计（对应 docs/plan.md §7 Phase 0b-(6) 的压测数据需求）。
#[derive(Default)]
struct Stats {
    events_create: u64,
    events_show: u64,
    /// 任务 24（审计 BUG-04）：NAMECHANGE 事件计数（标题后置窗口的
    /// 重评估入口）。
    events_namechange: u64,
    events_destroy_tracked: u64,
    candidates: u64,
    rewritten: u64,
    dry_run_hits: u64,
    truncated: u64,
    write_fail: u64,
    /// 任务 28：回写对抗（reassert）——已处理窗口的标记被应用 / shell
    /// 改写回去（典型：Explorer 文件夹窗口导航后 shell 重落自家 AUMID）
    /// 后，本会话自动补写的次数（NAMECHANGE 重验 + 5s 周期复核两个入口）。
    reasserted: u64,
    skipped_dupe: u64,
    skipped_not_app_window: u64,
    skipped_shell: u64,
    skipped_cloaked: u64,
    skipped_read_fail: u64,
    skipped_already_marked: u64,
    /// 带着另一条线路的标记出现（防止两种改写叠加而跳过）。
    skipped_cross_line: u64,
    /// 任务 13：启动扫存量中改写的存量应用窗口数。
    startup_sweep_rewritten: u64,
    /// 任务 13：启动扫存量中已带本线路标记的窗口数（幂等：重复
    /// watch 不会二次叠加后缀，直接计为已处理）。
    startup_sweep_marked: u64,
    /// 任务 20：检测到的 explorer 重启次数（shell PID 变化）。
    shell_restarts: u64,
    /// 任务 20：explorer 重启后全量重扫改写的窗口数。
    resweep_rewritten: u64,
    /// 任务 20：重扫中已带本线路标记的窗口数（幂等命中）。
    resweep_marked: u64,
}

struct WatcherState {
    dry_run: bool,
    verbose: bool,
    strategy: WatchStrategy,
    /// 线路二的共享 AUMID（`strategy == Ungroup` 时为空串）。
    group_value: String,
    /// 线路二的还原映射表（`strategy == Ungroup` 时为 None）。
    map: Option<RestoreMap>,
    started: Instant,
    /// 启动扫存量时刻的全部顶层窗口（含非应用窗口）：供退出复扫区分
    /// "启动时已存在"与"会话中新出现"（漏检只对后者计数），并继续
    /// 承担 DESTROY 簿记（任务 13 前仅收录应用窗口且不处理）。
    baseline: HashSet<usize>,
    baseline_count: u64,
    /// 本会话已处理（改写 / 判定跳过）的窗口（含启动扫存量的产出），
    /// 以 HWND 数值为键。
    handled: HashSet<usize>,
    /// 当前扫描类别（任务 13 启动扫 / 任务 20 重启重扫；None = 事件驱动），
    /// 供 consider 链路由改写与幂等命中计数。
    sweep_kind: SweepKind,
    stats: Stats,
}

/// 扫描类别（统计归集用；任务 13 启动扫 / 任务 20 explorer 重启重扫）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum SweepKind {
    None,
    Startup,
    Resweep,
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
    // 审计 BUG-10（任务 24）：panic 跨 extern "system" 回调边界是 UB
    // （debug 构建下；release panic=abort 天然中止，本层不生效）。
    // catch_unwind 拦截后丢弃该事件，保住进程与后续回调。
    let _ = catch_unwind(AssertUnwindSafe(|| {
        WATCHER.with(|cell| {
            if let Some(state) = cell.borrow_mut().as_mut() {
                state.on_event(event, hwnd, idobject, idchild);
            }
        });
    }));
}

impl WatcherState {
    fn new(
        opts: &WatchOptions,
        group_value: String,
        map: Option<RestoreMap>,
    ) -> Self {
        Self {
            dry_run: opts.dry_run,
            verbose: opts.verbose,
            strategy: opts.strategy,
            group_value,
            map,
            started: Instant::now(),
            baseline: HashSet::new(),
            baseline_count: 0,
            handled: HashSet::new(),
            sweep_kind: SweepKind::None,
            stats: Stats::default(),
        }
    }

    fn ts(&self) -> String {
        format!("[{:8.3}s]", self.started.elapsed().as_secs_f32())
    }

    /// 任务 13：启动扫存量窗口。watch 启动时把已存在的应用窗口也按
    /// 当前线路改写——对齐 mod 默认行为"开启即全量取消分组"，而非只
    /// 管新窗口（docs/plan.md v2 §3 任务 13）。两条线路同等生效。
    ///
    /// 全部顶层窗口仍记入 baseline（含非应用窗口）：供退出复扫区分
    /// "启动时已存在"与"会话中新出现"，并继续承担 DESTROY 簿记。
    /// 判定与改写全部复用 consider（应用窗口 / Shell / cloaked / 双线路
    /// 标记互斥检查都在里面）：非应用窗口不进 handled，若随后真正显示，
    /// SHOW 事件会再评估；已带本线路标记的按已处理计（幂等）；带另一
    /// 线路标记的跳过（互斥红线，任务 8）。
    unsafe fn startup_sweep(&mut self) -> Result<(), String> {
        self.sweep_kind = SweepKind::Startup;
        // 审计 BUG-11（任务 24）：枚举失败上抛——基线为空会导致存量窗口
        // 在后续事件中被误当新窗口全量重标
        for hwnd in winutil::enum_top_level_windows()? {
            let key = hwnd.0 as usize;
            self.baseline.insert(key);
            self.consider("SWEEP", hwnd, key);
        }
        self.sweep_kind = SweepKind::None;
        self.baseline_count = self.baseline.len() as u64;
        println!(
            "startup sweep (task 13): pre-existing windows rewritten={} already-marked={} baseline-top-level={}",
            self.stats.startup_sweep_rewritten,
            self.stats.startup_sweep_marked,
            self.baseline_count
        );
        Ok(())
    }

    /// 任务 20：explorer 重启后的全量重扫重标记。窗口属性可能随 shell
    /// 重启丢失（验收报告 §5-5），重扫把全部顶层窗口再过一遍 consider
    /// 链（幂等：已标记窗口不二次叠加；被回写/丢失的当场补标）。
    /// 与启动扫的差异仅在计数口径（resweep_*）——baseline 继续承担
    /// DESTROY 簿记与退出复扫的"新窗口"判定，重扫改写的窗口并入
    /// handled（退出复扫不误判为漏检）。枚举失败降级为空扫（watch
    /// 本体继续：退出复扫仍会给出全量清单）。
    unsafe fn resweep_all(&mut self) {
        self.stats.shell_restarts += 1;
        self.sweep_kind = SweepKind::Resweep;
        for hwnd in winutil::enum_top_level_windows().unwrap_or_default() {
            let key = hwnd.0 as usize;
            self.baseline.insert(key);
            self.consider("RESWEEP", hwnd, key);
        }
        self.sweep_kind = SweepKind::None;
        println!(
            "resweep (task 20): shell-restart re-sweep rewritten={} already-marked={}",
            self.stats.resweep_rewritten, self.stats.resweep_marked
        );
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
            // 任务 24（审计 BUG-04）：标题/类名后置就绪的窗口（先 SHOW 后
            // SetWindowText，如部分 Qt/Electron 应用、启动闪屏转主窗）在
            // CREATE/SHOW 两次评估时都不满足候选条件，此后无事件可再触发。
            // NAMECHANGE 事件让它们获得重评估机会。
            EVENT_OBJECT_NAMECHANGE => {
                self.stats.events_namechange += 1;
                "NAME"
            }
            EVENT_OBJECT_DESTROY => {
                let was_handled = self.handled.remove(&key);
                let was_baseline = self.baseline.remove(&key);
                if was_handled | was_baseline {
                    self.stats.events_destroy_tracked += 1;
                }
                // 线路二：已分组窗口销毁 → 丢弃还原映射（HWND 可能被复用）
                if was_handled && matches!(self.strategy, WatchStrategy::Group) {
                    if let Some(map) = self.map.as_mut() {
                        if map.remove(key) {
                            let _ = map.save();
                        }
                    }
                }
                return;
            }
            _ => return,
        };
        if self.handled.contains(&key) {
            // 已处理过（CREATE 处理后紧随的 SHOW 等）。NAMECHANGE 例外：
            // 任务 24 口径下它是标题后置窗口的重评估入口；任务 28 起对
            // 已处理窗口承担第二职责——回写检测入口（reassert：标记
            // 丢失当场补写，标记完好零动作零输出）。
            if event == EVENT_OBJECT_NAMECHANGE {
                unsafe { self.reassert(hwnd, key) };
                return;
            }
            self.stats.skipped_dupe += 1;
            return;
        }
        // 任务 13：存量窗口不再整体跳过——启动扫未改写成功的存量窗口
        // （扫时还不是应用窗口 / 写入失败）在后续 SHOW 事件中重试；
        // 已成功改写的走上面的 dupe 分支。启动前就正确标记的存量窗口
        // 在扫存量阶段已按已处理计（幂等）。
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
        // 双线路互斥标记检查（任务 8）：已带本线路标记 = 已处理；
        // 带另一线路标记 = 跳过（防止两种改写叠加成不可还原的状态）。
        // 任务 23（审计 BUG-05）：线路一的"已标记"判定走 strip_suffix 严格
        // 校验（标记 + 合法 hex + hex == 当前窗口 HWND），排除原生 AUMID
        // 恰含标记形态的假阳性；cross-line 判定保持宽松 contains——那只是
        // 保守跳过（宁可漏标，不可误叠），方向安全。
        let marked: Option<&str> = match self.strategy {
            WatchStrategy::Ungroup => {
                if appid::strip_suffix(&aumid, hwnd).is_some() {
                    Some("already marked (line 1 suffix)")
                } else if aumid.contains(appid::SUFFIX_MARKER) {
                    // 含标记形态但 hex 与本窗口不符（假阳性 / 异窗残留）：
                    // 保守跳过，保住 apply_ungroup 的"原值不含标记"前置
                    // 条件，杜绝双标记叠加
                    Some("already marked (suffix-like value not written by this tool for this window; skipped to avoid stacking)")
                } else if appid::is_group_aumid(&aumid) {
                    Some("cross-line marker (line 2 group AUMID)")
                } else {
                    None
                }
            }
            WatchStrategy::Group => {
                if appid::is_group_aumid(&aumid) {
                    Some("already marked (line 2 group AUMID)")
                } else if aumid.contains(appid::SUFFIX_MARKER) {
                    Some("cross-line marker (line 1 suffix)")
                } else {
                    None
                }
            }
        };
        if let Some(reason) = marked {
            if reason.starts_with("cross-line") {
                self.stats.skipped_cross_line += 1;
            } else if self.sweep_kind == SweepKind::Startup {
                // 任务 13：启动扫存量遇已带本线路标记 → 幂等，计已处理
                self.stats.startup_sweep_marked += 1;
            } else if self.sweep_kind == SweepKind::Resweep {
                // 任务 20：重启重扫遇已带本线路标记 → 幂等命中
                self.stats.resweep_marked += 1;
            } else {
                self.stats.skipped_already_marked += 1;
            }
            if self.verbose {
                self.log_skip(name, hwnd, reason);
            }
            self.handled.insert(key);
            return;
        }
        self.stats.candidates += 1;
        match self.strategy {
            WatchStrategy::Ungroup => self.apply_ungroup(name, hwnd, key, aumid),
            WatchStrategy::Group => self.apply_group(name, hwnd, key, aumid),
        }
    }

    /// 线路一：追加每窗口后缀（任务 6 原逻辑）。
    unsafe fn apply_ungroup(&mut self, name: &str, hwnd: HWND, key: usize, aumid: String) {
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
                match self.sweep_kind {
                    SweepKind::Startup => self.stats.startup_sweep_rewritten += 1,
                    SweepKind::Resweep => self.stats.resweep_rewritten += 1,
                    SweepKind::None => {}
                }
                println!(
                    "{} {} {} aumid={:?} -> {:?} (write {:.1}ms)",
                    self.ts(),
                    name,
                    fmt_window(hwnd),
                    winutil::shown_aumid(&aumid),
                    suffixed.value,
                    t0.elapsed().as_secs_f64() * 1000.0
                );
                self.handled.insert(key);
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
            }
        }
    }

    /// 线路二：改写为共享 AUMID（任务 8）。先落盘还原映射，再写属性；
    /// 映射保存失败则放弃改写（保住还原能力优先于分组生效）。
    unsafe fn apply_group(&mut self, name: &str, hwnd: HWND, key: usize, aumid: String) {
        let shared = self.group_value.clone();
        if self.dry_run {
            self.stats.dry_run_hits += 1;
            println!(
                "{} {} {} [dry-run] aumid={:?} -> {:?} (group)",
                self.ts(),
                name,
                fmt_window(hwnd),
                winutil::shown_aumid(&aumid),
                shared
            );
            self.handled.insert(key);
            return;
        }
        let Some(map) = self.map.as_mut() else {
            self.stats.write_fail += 1;
            println!(
                "{} {} {} group write FAILED: no restore map loaded",
                self.ts(),
                name,
                fmt_window(hwnd)
            );
            return;
        };
        map.record(key, &shared, &aumid);
        if let Err(e) = map.save() {
            map.remove(key);
            self.stats.write_fail += 1;
            println!(
                "{} {} {} group write FAILED: {e} (AUMID left untouched)",
                self.ts(),
                name,
                fmt_window(hwnd)
            );
            return;
        }
        let t0 = Instant::now();
        match appid::set_aumid(hwnd, &shared) {
            Ok(()) => {
                self.stats.rewritten += 1;
                match self.sweep_kind {
                    SweepKind::Startup => self.stats.startup_sweep_rewritten += 1,
                    SweepKind::Resweep => self.stats.resweep_rewritten += 1,
                    SweepKind::None => {}
                }
                println!(
                    "{} {} {} aumid={:?} -> {:?} (group, write {:.1}ms)",
                    self.ts(),
                    name,
                    fmt_window(hwnd),
                    winutil::shown_aumid(&aumid),
                    shared,
                    t0.elapsed().as_secs_f64() * 1000.0
                );
                self.handled.insert(key);
            }
            Err(e) => {
                // 回滚映射条目：AUMID 未动，条目已作废
                if let Some(map) = self.map.as_mut() {
                    map.remove(key);
                    let _ = map.save();
                }
                self.stats.write_fail += 1;
                println!(
                    "{} {} {} aumid={:?} group write FAILED: {e}",
                    self.ts(),
                    name,
                    fmt_window(hwnd),
                    aumid
                );
                // 不进 handled：窗口后续的 SHOW 事件可重试
            }
        }
    }

    fn log_skip(&self, name: &str, hwnd: HWND, reason: &str) {
        println!(
            "{} {} {} skip: {reason}",
            self.ts(),
            name,
            unsafe { fmt_window(hwnd) }
        );
    }

    /// 任务 28：回写对抗（reassert）——对已处理窗口核对标记是否仍在；
    /// 丢失（应用 / shell 改写了自家窗口的 AUMID，典型：Explorer 文件夹
    /// 窗口导航后 shell 重落原生值）则按当前线路立即补写。
    ///
    /// 背景调研结论（2026-09-24）：非注入路线**无法阻止**属主进程改写
    /// 自家窗口属性（MSDN《AppUserModelIDs》：AUMID 本就由窗口属主设置，
    /// `SHGetPropertyStoreForWindow` 只提供外部读写）；可行的对抗只有
    /// "检测 + 补写"：NAMECHANGE（导航/标题变化即触发）+ 5s 周期复核
    /// （`reverify_handled`）双入口。代价：补写瞬间按钮可能有一次
    /// 分组/独立跳动（闪烁）。
    ///
    /// 安全边界与 consider 同口径：另一线路标记 / 标记形态异常值一律
    /// 不补（宁可漏标，不可误叠，任务 23 审计 BUG-05 原则）；shell
    /// 窗口（桌面/任务栏本体，consider 阶段就只记簿不改写）跳过；
    /// 读取失败（窗口已销毁但 DESTROY 未及清理）静默返回，簿记交
    /// DESTROY。dry-run 从未写入，无补写对象。
    unsafe fn reassert(&mut self, hwnd: HWND, key: usize) {
        if self.dry_run {
            return;
        }
        if winutil::is_shell_window(hwnd) {
            return;
        }
        let aumid = match appid::get_aumid(hwnd) {
            Ok(a) => a,
            Err(_) => return,
        };
        let (target, truncated) = match self.strategy {
            WatchStrategy::Ungroup => {
                if appid::strip_suffix(&aumid, hwnd).is_some() {
                    return; // 标记完好
                }
                // 保守跳过：标记形态异常 / 另一线路标记（防叠加）
                if aumid.contains(appid::SUFFIX_MARKER) || appid::is_group_aumid(&aumid) {
                    return;
                }
                let suffixed = appid::suffixed_aumid(&aumid, hwnd);
                (suffixed.value, suffixed.truncated)
            }
            WatchStrategy::Group => {
                if appid::is_group_aumid(&aumid) {
                    return; // 标记完好
                }
                if aumid.contains(appid::SUFFIX_MARKER) {
                    return; // 另一线路标记：保守跳过
                }
                // 已有还原映射条目（本会话或前会话线路二记录）→ 只补写
                // 共享值，不动映射表：原值已录，避免 Explorer 每次导航都
                // 触发一次落盘
                if !self
                    .map
                    .as_ref()
                    .map_or(false, |m| m.peek(key, &self.group_value).is_some())
                {
                    // 无条目（跨会话残留 / 竞态遗漏）：按首次改写补录原值，
                    // 落盘失败则不写（还原能力优先于分组生效，apply_group 同则）
                    let Some(map) = self.map.as_mut() else {
                        self.stats.write_fail += 1;
                        eprintln!(
                            "{} REASSERT {} group reassert FAILED: no restore map loaded",
                            self.ts(),
                            fmt_window(hwnd)
                        );
                        return;
                    };
                    map.record(key, &self.group_value, &aumid);
                    if let Err(e) = map.save() {
                        map.remove(key);
                        self.stats.write_fail += 1;
                        eprintln!(
                            "{} REASSERT {} map save FAILED: {e} (AUMID left untouched)",
                            self.ts(),
                            fmt_window(hwnd)
                        );
                        return;
                    }
                }
                (self.group_value.clone(), false)
            }
        };
        if truncated {
            self.stats.truncated += 1;
        }
        let t0 = Instant::now();
        match appid::set_aumid(hwnd, &target) {
            Ok(()) => {
                self.stats.reasserted += 1;
                println!(
                    "{} REASSERT {} aumid={:?} -> {:?} (marker lost, re-applied, write {:.1}ms)",
                    self.ts(),
                    fmt_window(hwnd),
                    winutil::shown_aumid(&aumid),
                    target,
                    t0.elapsed().as_secs_f64() * 1000.0
                );
            }
            Err(e) => {
                self.stats.write_fail += 1;
                eprintln!(
                    "{} REASSERT {} write FAILED: {e}",
                    self.ts(),
                    fmt_window(hwnd)
                );
            }
        }
    }

    /// 任务 28：5s 周期复核全部已处理窗口——兜底无标题变化的静默回写
    /// （NAMECHANGE 入口覆盖导航/标题变化场景，此入口覆盖其余）。快照
    /// 键集后逐个 reassert；读失败即跳过（DESTROY 簿记负责清理）。
    unsafe fn reverify_handled(&mut self) {
        let keys: Vec<usize> = self.handled.iter().copied().collect();
        for key in keys {
            let hwnd = HWND(key as *mut _);
            self.reassert(hwnd, key);
        }
    }

    /// 退出时的全量复扫与统计输出：
    /// - 漏检 = 新出现的应用窗口在会话结束时仍无本线路标记；
    /// - 回退 = 本会话改写过、但标记已消失（应用回写了自身 AUMID 的证据）。
    unsafe fn final_scan_and_report(&mut self) {
        println!();
        println!(
            "==== watch stats (task 6/8/13, docs/plan.md v2 §3, strategy: {}) ====",
            self.strategy.label()
        );
        println!(
            "events (per event): CREATE={} SHOW={} NAMECHANGE={} DESTROY(tracked)={}",
            self.stats.events_create, self.stats.events_show, self.stats.events_namechange, self.stats.events_destroy_tracked
        );
        println!(
            "startup sweep (task 13): pre-existing rewritten={} already-marked={}",
            self.stats.startup_sweep_rewritten, self.stats.startup_sweep_marked
        );
        println!(
            "shell restarts (task 20)             : {} (resweep rewritten={} already-marked={})",
            self.stats.shell_restarts, self.stats.resweep_rewritten, self.stats.resweep_marked
        );
        println!(
            "baseline top-level windows at start : {}",
            self.baseline_count
        );
        println!("new-window candidates         : {}", self.stats.candidates);
        // 审计 BUG-13（任务 24）：注明口径——CREATE/SHOW/NAMECHANGE 与
        // not_app_window 按"事件"累计（同一窗口多次事件重复计数）
        println!(
            "skips (per event) : dupe={} not_app_window={} shell={} cloaked={} read_fail={} already_marked={} cross_line={}",
            self.stats.skipped_dupe,
            self.stats.skipped_not_app_window,
            self.stats.skipped_shell,
            self.stats.skipped_cloaked,
            self.stats.skipped_read_fail,
            self.stats.skipped_already_marked,
            self.stats.skipped_cross_line
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
        println!(
            "reasserted (marker lost & re-applied, task 28): {}",
            self.stats.reasserted
        );

        let mut missed: Vec<String> = Vec::new();
        let mut reverted: Vec<String> = Vec::new();
        let mut alive_marked: u64 = 0;
        let mut scan_read_fail: u64 = 0;
        // 本线路的"已标记"判定：线路一认后缀标记，线路二认共享前缀
        let is_marked = |aumid: &str| match self.strategy {
            WatchStrategy::Ungroup => aumid.contains(appid::SUFFIX_MARKER),
            WatchStrategy::Group => appid::is_group_aumid(aumid),
        };
        for hwnd in winutil::enum_top_level_windows().unwrap_or_default() {
            let key = hwnd.0 as usize;
            let was_handled = self.handled.contains(&key);
            let is_new = !self.baseline.contains(&key);
            if !was_handled && !is_new {
                // 启动时已存在且本会话未处理（启动扫判非应用窗口后一直
                // 没显示、或写入失败已由 write_fail 统计）：与漏检/回退
                // 判定无关
                continue;
            }
            let aumid = match appid::get_aumid(hwnd) {
                Ok(a) => a,
                Err(_) => {
                    scan_read_fail += 1;
                    continue;
                }
            };
            if is_marked(&aumid) {
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
        if matches!(self.strategy, WatchStrategy::Group) {
            let remaining = self.map.as_ref().map_or(0, |m| m.len());
            println!(
                "restore-map entries alive         : {remaining} ({}, %LOCALAPPDATA%\\tbg-lite)",
                crate::restoremap::MAP_FILE_NAME
            );
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

#[cfg(test)]
mod tests {
    use super::clip;

    #[test]
    fn clip_marks_truncation() {
        assert_eq!(clip("short", 10), "short");
        assert_eq!(clip("0123456789", 10), "0123456789"); // 恰好 10 不截
        // "exactly-10!" 是 11 字符 → 截为前 9 字符 + '~'
        assert_eq!(clip("exactly-10!", 10), "exactly-1~");
        assert_eq!(clip("a-bit-too-long-value", 10), "a-bit-too~");
        assert_eq!(clip("中文窗口标题很长", 5), "中文窗口~");
    }
}

pub(crate) fn run(opts: WatchOptions) -> Result<(), String> {
    // 任务 20（最先执行，早于一切可能快速失败的前置）：异常熔断记账。
    // 上一轮短命消失（< 30s，崩溃/启动即死）累计到阈值 → 注销自启，
    // 防开机死循环；本次 watch 照常执行（熔断不是拒绝手动使用）。
    // 注意：此后的任何早退（互斥获取失败/表加载失败/钩子失败）都不会
    // 走 end_clean → 按异常退出累计——自启进程反复快速失败正是要熔断
    // 的场景。
    let health = health::begin();
    if health.tripped {
        eprintln!(
            "watch: circuit breaker (task 20): {} consecutive abnormal exits detected - removing the autostart entry to break a potential boot loop",
            health::TRIP_THRESHOLD
        );
        match crate::autostart::uninstall() {
            Ok(crate::autostart::UninstallOutcome::Removed(prev)) => {
                eprintln!("watch: circuit breaker: autostart removed (was: {prev})");
            }
            Ok(crate::autostart::UninstallOutcome::NotInstalled) => {
                eprintln!("watch: circuit breaker: no autostart entry present (nothing to remove)");
            }
            Err(e) => eprintln!("watch: circuit breaker: removing autostart failed: {e}"),
        }
    }

    // 任务 20：环形日志（--log，默认关）；只记关键事件，256 KiB 上限
    let ring = RingLog::open(opts.ring_log);
    ring.log(&format!(
        "watch start: strategy={} group={:?} duration={:?} dry_run={}",
        if matches!(opts.strategy, WatchStrategy::Ungroup) { "ungroup" } else { "group" },
        opts.group_name,
        opts.duration,
        opts.dry_run
    ));
    if health.tripped {
        ring.log("circuit breaker tripped: autostart uninstalled");
    }

    // AUMID 读写（IPropertyStore）要求本线程已初始化 COM
    let _com = winutil::ComGuard::init()?;

    // 审计 BUG-02（任务 22）：线路二会写映射表，全程持有映射表互斥
    // （守卫存活至 run 返回，与 restore 互斥）；获取失败即拒绝启动。
    let _map_mutex = match opts.strategy {
        WatchStrategy::Group => Some(crate::singleinstance::MapMutex::acquire()?),
        WatchStrategy::Ungroup => None,
    };

    // 任务 8：线路二需要共享 AUMID 与还原映射表；组名在 main 层校验过，
    // 这里再算一次具体值；映射表加载失败即中止（线路二没有还原映射就
    // 不该跑）。审计 SEC-01（任务 22）：表位于 %LOCALAPPDATA%\tbg-lite。
    let (group_value, map) = match opts.strategy {
        WatchStrategy::Group => {
            let name = opts.group_name.clone().unwrap_or_default();
            let value = appid::group_aumid(&name)?;
            let dir = crate::restoremap::data_dir()?;
            let map = RestoreMap::load(&dir)?;
            (value, Some(map))
        }
        WatchStrategy::Ungroup => (String::new(), None),
    };
    let group_display = group_value.clone(); // 状态创建后仍需打印

    // 1. 线程本地状态先行就位（回调里 if-let 判空，绝不 panic）。
    //    注意：启动扫存量必须在钩子安装之后做，见下方第 3 步。
    WATCHER.with(|cell| {
        *cell.borrow_mut() = Some(WatcherState::new(&opts, group_value, map));
    });

    // 2. 安装 winevent 钩子（零注入：WINEVENT_OUTOFCONTEXT 回调只在本进程执行）。
    //    任务 24（审计 BUG-04）：+EVENT_OBJECT_NAMECHANGE——标题后置窗口的重评估入口
    let flags = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;
    let events = [
        EVENT_OBJECT_CREATE,
        EVENT_OBJECT_SHOW,
        EVENT_OBJECT_NAMECHANGE,
        EVENT_OBJECT_DESTROY,
    ];
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

    println!("tbg-lite watch (task 6/8/13/24): SetWinEventHook CREATE/SHOW/NAMECHANGE/DESTROY, out-of-context, skip-own-process");
    println!("strategy : {}", opts.strategy.label());
    if ring.enabled() {
        println!(
            "log      : ring log enabled (%LOCALAPPDATA%\\tbg-lite\\{}), 256 KiB cap (task 20)",
            crate::ringlog::LOG_FILE_NAME
        );
    }
    println!("startup  : pre-existing app windows are rewritten at launch (task 13: enabling = ungroup everything)");
    if matches!(opts.strategy, WatchStrategy::Group) {
        println!(
            "group    : every candidate window (incl. pre-existing) gets the shared AUMID {group_display:?}"
        );
        println!(
            "restore  : originals persisted to {} (in %LOCALAPPDATA%\\tbg-lite) for `restore`",
            crate::restoremap::MAP_FILE_NAME
        );
    }
    println!("note: DESTROY hook is bookkeeping only (tracked-window cleanup)");
    println!("reassert : markers lost to app/shell rewrites are re-applied (task 28: NAMECHANGE check + 5s periodic verify)");
    if opts.duration.is_zero() {
        if opts.stop.is_some() {
            // 任务 14：菜单模式——由停止标志优雅退出（无需 Ctrl+C）
            println!("duration: until stopped from the interactive menu (graceful stop: hooks removed + stats printed)");
        } else {
            println!("duration: until Ctrl+C (hard exit, no stats printed)");
        }
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

    // 3. 启动扫存量（任务 13）：把已存在的应用窗口也按当前线路改写
    //    （对齐 mod 默认"开启即全量取消分组"）。必须在钩子安装之后执行：
    //    这期间新窗口的事件已能入队，不会两头漏。
    //    审计 BUG-11：枚举失败 → 摘钩清理后上抛（基线为空比停跑更危险）。
    let sweep_result = WATCHER.with(|cell| {
        if let Some(state) = cell.borrow_mut().as_mut() {
            unsafe { state.startup_sweep() }
        } else {
            Ok(())
        }
    });
    if let Err(e) = sweep_result {
        for h in &hooks {
            if !unsafe { UnhookWinEvent(*h).as_bool() } {
                eprintln!("watch: warning: UnhookWinEvent failed");
            }
        }
        WATCHER.with(|cell| *cell.borrow_mut() = None);
        return Err(format!("watch: startup sweep failed: {e}"));
    }
    ring.log("startup sweep done");

    // 4. 消息泵：winevent 回调在 PeekMessage 检索期间由系统调用；
    //    MsgWaitForMultipleObjectsEx 让消息一到就醒来（压测时延观察更真实）
    //    任务 20：常驻模式（--duration 0 且无停止标志）下 wait 封顶 2s
    //    （原为 INFINITE）——宿主以 2s 级轮询 explorer PID，重启即全量
    //    重扫重标记；Ctrl+C 硬杀行为不变
    let deadline = (!opts.duration.is_zero()).then(|| Instant::now() + opts.duration);
    let mut stopped_by_request = false;
    let mut msg = MSG::default();
    // 任务 20：explorer 重启监视（GetShellWindow → 其属主进程 PID）。
    // 初始 None（watch 启动时无 shell，如 CI 探针场景）→ 首次见到 Some
    // 记为基线不触发；PID 变化 = shell 重启 → 全量重扫。NULL 窗口
    // （重启中）不更新基线，等新 shell 出现。
    let mut last_shell_pid = current_shell_pid();
    let mut last_shell_poll = Instant::now();
    // 任务 28：回写对抗的周期复核节拍（5s；NAMECHANGE 入口之外的兜底）
    let mut last_reassert_poll = Instant::now();
    // 任务 33（P0-B）：熔断心跳节拍——每 5s 原子刷 health 文件的
    // `running` 行，开机死循环判定以“心跳 − start”为存活时长，
    // 与两次开机的间隔无关（见 health.rs 模块注释）
    let mut last_health_beat = Instant::now();
    loop {
        // 任务 14：菜单模式的停止标志——置位即优雅退出（摘钩 + 终扫 +
        // 统计，见循环后的公共退出路径）。启动扫存量在进泵前无条件跑完，
        // 因此标志在扫存量期间置位也不会丢改写。
        if let Some(flag) = &opts.stop {
            if flag.load(Ordering::Relaxed) {
                stopped_by_request = true;
                break;
            }
        }
        let wait_ms = match deadline {
            Some(d) => {
                let now = Instant::now();
                if now >= d {
                    break;
                }
                let remain = d - now;
                // 审计 BUG-08（任务 24）：封顶 1s——同时覆盖两个边界：
                // duration > ~49.7 天时 as_millis 会被 cap 成 u32::MAX
                // （== INFINITE，无输入则永不醒检查时钟）；以及
                // MWMO_INPUTAVAILABLE 在 PeekMessage 取不走输入状态时的
                // 理论忙转。每秒醒一次检查 deadline，成本可忽略
                remain.as_millis().min(1000) as u32
            }
            // 无 deadline（常驻）：菜单模式（有停止标志）→ 1s 轮询标志；
            // CLI 参数模式 → 2s 轮询 explorer PID（任务 20，原 INFINITE；
            // Ctrl+C 强杀行为不变）
            None => {
                if opts.stop.is_some() {
                    1000
                } else {
                    2000
                }
            }
        };
        unsafe {
            let _ = MsgWaitForMultipleObjectsEx(None, wait_ms, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
            while PeekMessageW(&mut msg, HWND::default(), 0, 0, PM_REMOVE).as_bool() {
                // 线程消息无窗口过程，检索即消费；winevent 回调在其中触发
            }
        }
        // 任务 20：explorer 重启监视（2s 级轮询，与消息泵同线程零锁）
        if last_shell_poll.elapsed() >= Duration::from_secs(2) {
            last_shell_poll = Instant::now();
            let cur = current_shell_pid();
            match (last_shell_pid, cur) {
                (Some(prev), Some(cur_pid)) if cur_pid != prev => {
                    // shell 重启：全量重扫重标记（窗口属性可能丢失，幂等补标）
                    println!(
                        "watch: shell restart detected (task 20): explorer pid {prev} -> {cur_pid}; re-sweeping all windows"
                    );
                    ring.log(&format!("shell restart: explorer pid {prev} -> {cur_pid}"));
                    WATCHER.with(|cell| {
                        if let Some(state) = cell.borrow_mut().as_mut() {
                            unsafe { state.resweep_all() };
                        }
                    });
                    ring.log("resweep done");
                    last_shell_pid = Some(cur_pid);
                }
                (None, Some(cur_pid)) => {
                    // 首次见到 shell（如 CI 探针先启动 watch 后拉 explorer）：
                    // 记为基线，不触发重扫
                    last_shell_pid = Some(cur_pid);
                }
                _ => {}
            }
        }
        // 任务 28：回写对抗——5s 周期复核已处理窗口的标记。NAMECHANGE
        // 入口覆盖导航/标题变化场景（Explorer 回写的主通道）；此入口兜底
        // 无标题变化的静默回写（如部分浏览器后台周期性重落 AUMID）。
        // 与 shell 轮询同线程零锁；读 AUMID 为廉价 COM 属性读。
        if last_reassert_poll.elapsed() >= Duration::from_secs(5) {
            last_reassert_poll = Instant::now();
            WATCHER.with(|cell| {
                if let Some(state) = cell.borrow_mut().as_mut() {
                    unsafe { state.reverify_handled() };
                }
            });
        }
        // 任务 33（P0-B）：熔断心跳——与回写复核同节拍独立计数，写失败
        // 静默（health 内部尽力而为）
        if last_health_beat.elapsed() >= Duration::from_secs(health::HEARTBEAT_SECS) {
            last_health_beat = Instant::now();
            health.heartbeat();
        }
    }

    if stopped_by_request {
        println!();
        println!("watch: stop requested (task 14) — removing hooks and running the final scan");
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

    // 任务 20：优雅退出——健康记账收尾（移除 running 行、streak 清零）
    // + 环形日志收尾。到不了这里的退出（崩溃/强杀/早退 Err）即异常退出，
    // 下一轮 begin 据此累计熔断计数
    health.end_clean();
    ring.log("watch stop (graceful)");
    Ok(())
}

/// 任务 20：当前 shell（explorer）进程 PID。`GetShellWindow` 返回桌面
/// 窗口（Progman，属 explorer 进程）；无 shell / 尚未就绪时 None。
fn current_shell_pid() -> Option<u32> {
    unsafe {
        let hwnd = GetShellWindow();
        if hwnd.0.is_null() {
            return None;
        }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            None
        } else {
            Some(pid)
        }
    }
}
