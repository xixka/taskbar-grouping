//! 异常熔断（任务 20，docs/plan.md v2 §3 Phase 3）。
//!
//! 目标：**防开机死循环**——自启的 watch 若在启动期连续崩溃，每次开机
//! 都会拉起一个立刻死掉的进程；熔断在连续第 `TRIP_THRESHOLD` 次异常
//! 退出后自动注销自启（HKCU Run），切断循环。
//!
//! 状态文件 `tbg-health.tsv`（数据目录，与还原表同目录；**不触碰
//! `tbg-restore.tsv`，不参与映射表单实例互斥**——那是审计 BUG-02 的
//! 写表红线，本文件是独立状态文件）：
//!
//! ```text
//! # tbg-lite health v1
//! abnormal_streak<TAB><n>
//! running<TAB><unix_start><TAB><unix_last_heartbeat>
//! ```
//!
//! 语义：
//! - `watch` 启动时 `begin()`：读状态；若上一轮留有 `running` 行（该轮
//!   未正常收尾）且存活时长 < `ABNORMAL_MAX_UPTIME_SECS`（30s）→
//!   streak+1（短命消失 = 崩溃/启动即死）；存活 ≥ 阈值后被杀（用户
//!   Ctrl+C / 任务管理器终结一个健康长驻进程）→ streak 清零（人工
//!   干预不是崩溃循环）；无 `running` 行（上轮正常收尾）→ streak 保持。
//! - **P0-B 修复（2026-09-25 审查）**：存活时长以**最后心跳时刻**推算
//!   （`heartbeat − start`），而不是"本次启动时刻 − 上轮 start"——后者
//!   在开机死循环场景 = 两次开机间隔（分钟级）恒 ≥ 30s，streak 永远
//!   清零，熔断对真实目标**永不触发**。心跳由 `winevent::run` 消息泵
//!   每 `HEARTBEAT_SECS`（5s）原子写一次；进程被杀后文件停留在最后一
//!   次心跳，下次开机的 `begin()` 据此还原上轮真实存活时长，与两次
//!   开机之间的间隔无关。
//! - 旧版 v1 行（`running<TAB><t>`，无心跳列）：按旧语义近似
//!   （`now − start`）宽容兼容——仅影响升级后的首次记账。
//! - streak ≥ `TRIP_THRESHOLD`（3）→ **熔断**：调用方（`winevent::run`）
//!   注销自启并告警；随后 streak 归零重新计数（熔断目的是切断开机循环，
//!   不是拒绝用户手动运行——本次 watch 照常执行）。
//! - 优雅退出（时长自然结束 / 菜单停止 / Ctrl+C 优雅化后）→
//!   `end_clean()`：移除 `running` 行、streak 清零。
//!
//! 尽力而为——读失败按无历史、写失败不阻断 watch（熔断是安全网，
//! 不该成为新的故障点）。写路径复用 `restoremap::atomic_write`（审计
//! BUG-03 的 tmp+sync+rename 原子替换：心跳每 5s 重写，崩溃窗口期内
//! 文件始终保持上一个完整版本）。

use std::path::PathBuf;

use crate::restoremap;

const HEALTH_FILE_NAME: &str = "tbg-health.tsv";
const HEADER: &str = "# tbg-lite health v1";
/// 单轮存活不足该时长即消失 = 异常退出（崩溃 / 启动即死）。
pub(crate) const ABNORMAL_MAX_UPTIME_SECS: u64 = 30;
/// 连续异常退出达到该次数 → 熔断（注销自启）。
pub(crate) const TRIP_THRESHOLD: u32 = 3;
/// 心跳节拍（消息泵每 5s 把 `running` 行的心跳列原子刷一次）。
/// 进程被杀后存活时长估计误差上界 = 该节拍（±5s），对 30s 阈值判定
/// 足够精确：短命崩溃（启动即死）心跳停在 ≈start；长驻进程心跳停在
/// ≈被杀时刻。
pub(crate) const HEARTBEAT_SECS: u64 = 5;

/// `running` 行状态：本轮启动时刻 + 最后心跳时刻。
/// `heartbeat == None` = 旧版 v1 行（无心跳列，按旧语义近似）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RunningState {
    pub(crate) start: u64,
    pub(crate) heartbeat: Option<u64>,
}

pub(crate) struct Health {
    path: Option<PathBuf>,
    /// 本轮 begin 记账后的 streak（心跳重写文件时原样保留）。
    streak: u32,
    /// 本轮启动时刻（`running` 行的 start 列）。
    start: u64,
    /// 本次 begin 是否触发熔断（调用方据此注销自启 + 告警）。
    pub(crate) tripped: bool,
}

impl Health {
    /// 消息泵周期调用（`winevent::run`，节拍 `HEARTBEAT_SECS`）：把
    /// `running` 行的心跳列刷新为当前时刻。原子写；失败静默——
    /// 熔断是尽力而为的安全网，心跳写失败不该影响 watch 本体。
    pub(crate) fn heartbeat(&self) {
        let Some(path) = &self.path else { return };
        let _ = restoremap::atomic_write(
            path,
            &render(
                self.streak,
                Some(RunningState {
                    start: self.start,
                    heartbeat: Some(unix_now()),
                }),
            ),
        );
    }

    /// 优雅退出时调用：移除 `running` 行、streak 归零。尽力而为。
    pub(crate) fn end_clean(&self) {
        let Some(path) = &self.path else { return };
        let _ = restoremap::atomic_write(path, &render(0, None));
    }
}

/// watch 启动入口：读状态 → 记账 → 写入本轮 `running` 行
/// （start = heartbeat = now）。返回值携带 `tripped`（streak 达阈值），
/// 调用方负责注销自启与告警；熔断后 streak 已归零（防每次手动运行
/// 都重复告警）。
pub(crate) fn begin() -> Health {
    let path = restoremap::data_dir().ok().map(|d| d.join(HEALTH_FILE_NAME));
    let (prev_streak, prev_running) = match &path {
        Some(p) if p.exists() => std::fs::read_to_string(p)
            .map(|c| parse(&c))
            .unwrap_or((0, None)),
        _ => (0, None),
    };
    let now = unix_now();
    let (streak, tripped) = account(now, prev_running, prev_streak);
    if let Some(p) = &path {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = restoremap::atomic_write(
            p,
            &render(
                streak,
                Some(RunningState {
                    start: now,
                    heartbeat: Some(now),
                }),
            ),
        );
    }
    Health {
        path,
        streak,
        start: now,
        tripped,
    }
}

/// 记账核心（纯函数，时钟注入，单测覆盖）。返回 `(新 streak, 是否熔断)`：
/// - 上轮短命消失（存活 < `ABNORMAL_MAX_UPTIME_SECS`）→ streak+1；
/// - 上轮长寿命后被杀 → streak 清零（人工干预，非崩溃循环）；
/// - 上轮正常收尾（无 `running` 行）→ streak 保持；
/// - streak ≥ `TRIP_THRESHOLD` → 熔断，streak 归零。
///
/// P0-B：存活时长优先取**心跳差**（`heartbeat − start`，跨重启的时钟
/// 无关量）；v1 旧格式无心跳列时退回旧近似（`now − start`）。
pub(crate) fn account(now: u64, prev: Option<RunningState>, prev_streak: u32) -> (u32, bool) {
    let streak = match prev {
        Some(r) => {
            let uptime = match r.heartbeat {
                Some(h) => h.saturating_sub(r.start),
                None => now.saturating_sub(r.start),
            };
            if uptime < ABNORMAL_MAX_UPTIME_SECS {
                prev_streak.saturating_add(1)
            } else {
                0
            }
        }
        None => prev_streak,
    };
    if streak >= TRIP_THRESHOLD {
        (0, true)
    } else {
        (streak, false)
    }
}

/// 解析状态文件（宽容式：认得的行取值，认不得的忽略；表头可选）。
/// 返回 `(streak, running 行状态)`。
pub(crate) fn parse(content: &str) -> (u32, Option<RunningState>) {
    let mut streak = 0u32;
    let mut running = None;
    for line in content.lines() {
        let mut parts = line.split('\t');
        let (key, value) = match (parts.next(), parts.next()) {
            (Some(k), Some(v)) => (k.trim(), v.trim()),
            _ => continue,
        };
        match key {
            "abnormal_streak" => {
                if let Ok(n) = value.parse::<u32>() {
                    streak = n;
                }
            }
            "running" => {
                // v2：running<TAB>start<TAB>heartbeat（心跳列宽容：
                // 缺失/坏值 → None = 按旧语义近似）
                if let Ok(start) = value.parse::<u64>() {
                    let heartbeat = parts.next().and_then(|h| h.trim().parse::<u64>().ok());
                    running = Some(RunningState { start, heartbeat });
                }
            }
            _ => {} // 表头 / 未知行：忽略
        }
    }
    (streak, running)
}

/// 渲染状态文件（与 `parse` 互逆；`running == None` = 正常收尾态）。
pub(crate) fn render(streak: u32, running: Option<RunningState>) -> String {
    match running {
        Some(r) => match r.heartbeat {
            Some(h) => format!(
                "{HEADER}\nabnormal_streak\t{streak}\nrunning\t{}\t{h}\n",
                r.start
            ),
            // v1 兼容渲染（roundtrip 测试用；begin/heartbeat 恒写心跳列）
            None => format!("{HEADER}\nabnormal_streak\t{streak}\nrunning\t{}\n", r.start),
        },
        None => format!("{HEADER}\nabnormal_streak\t{streak}\n"),
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running(start: u64, heartbeat: u64) -> Option<RunningState> {
        Some(RunningState {
            start,
            heartbeat: Some(heartbeat),
        })
    }

    fn running_v1(start: u64) -> Option<RunningState> {
        Some(RunningState {
            start,
            heartbeat: None,
        })
    }

    #[test]
    fn account_short_abnormal_exit_increments() {
        // 上轮心跳停在 start+10（存活 10s 后消失）→ +1
        assert_eq!(account(1000, running(990, 1000), 0), (1, false));
        // 2 + 1 = 3 达阈值 → 熔断且归零（streak 重置，防每次运行重复告警）
        assert_eq!(account(1000, running(975, 985), 2), (0, true));
    }

    #[test]
    fn account_boot_loop_minutes_gap_still_counts() {
        // P0-B 直接回归（审查实证）：开机死循环——上轮 watch 开机即死
        // （心跳停在 ≈start），本次开机在几分钟后。旧实现用
        // now − start（= 两次开机间隔，分钟级）近似存活时长 → 恒 ≥ 30s
        // → streak 永远清零。心跳语义下取 heartbeat − start ≈ 0 → 正常累计。
        let boot1 = 1_700_000_000u64;
        let boot2 = boot1 + 600; // 10 分钟后的下一次开机
        let (s1, t1) = account(boot2, running(boot1, boot1 + 1), 0);
        assert_eq!((s1, t1), (1, false));
        let (s2, t2) = account(boot2 + 600, running(boot2, boot2 + 2), s1);
        assert_eq!((s2, t2), (2, false));
        let (s3, t3) = account(boot2 + 1200, running(boot2 + 600, boot2 + 600 + 3), s2);
        assert_eq!((s3, t3), (0, true)); // 第 3 次 → 熔断
    }

    #[test]
    fn account_long_lived_kill_resets() {
        // 上轮存活 120s 后被杀（健康长驻被人工终结，心跳停在 ≈被杀时刻）
        // → 清零，不累计。注意 now 可以远晚于心跳（几天后才再次启动）
        assert_eq!(account(9_999_999, running(880, 1000), 2), (0, false));
        // 恰好等于阈值：30s 不算"短命"（< 30 才算）
        assert_eq!(account(1000, running(970, 1000), 2), (0, false));
    }

    #[test]
    fn account_clean_finish_keeps_streak() {
        // 上轮正常收尾（无 running 行）：streak 保持
        assert_eq!(account(1000, None, 0), (0, false));
        assert_eq!(account(1000, None, 2), (2, false));
    }

    #[test]
    fn account_trips_exactly_at_threshold() {
        // streak 从 0 连续 3 次短命 → 第 3 次熔断
        let (_, t1) = account(1000, running(995, 1000), 0);
        assert!(!t1);
        let (_, t2) = account(2000, running(1995, 2000), 1);
        assert!(!t2);
        let (s3, t3) = account(3000, running(2995, 3000), 2);
        assert!(t3);
        assert_eq!(s3, 0); // 熔断后归零
    }

    #[test]
    fn account_v1_running_line_keeps_legacy_semantics() {
        // v1 旧格式（无心跳列）：沿用旧近似 now − start。
        // 上轮 start=880，now=1000 → 存活近似 120s ≥ 30 → 清零
        assert_eq!(account(1000, running_v1(880), 2), (0, false));
        // 上轮 start=995，now=1000 → 近似 5s < 30 → +1
        assert_eq!(account(1000, running_v1(995), 0), (1, false));
    }

    #[test]
    fn account_heartbeat_before_start_clamped() {
        // 防御：时钟回拨导致心跳 < start → saturating 到 0 → 按短命计
        assert_eq!(account(1000, running(1000, 500), 0), (1, false));
    }

    #[test]
    fn parse_render_roundtrip() {
        // 运行态（v2 三列）
        let text = render(2, running(1_700_000_000, 1_700_000_500));
        assert_eq!(parse(&text), (2, running(1_700_000_000, 1_700_000_500)));
        // 收尾态（无 running 行）
        let text2 = render(0, None);
        assert_eq!(parse(&text2), (0, None));
        // 宽容式：表头/未知行/坏值忽略
        let junk = "# tbg-lite health v1\nabnormal_streak\tzz\nrunning\tabc\nwhatever\tx\n";
        assert_eq!(parse(junk), (0, None));
        // 无表头也能读
        assert_eq!(parse("abnormal_streak\t1\nrunning\t42\n"), (1, running_v1(42)));
        // v1 行 + v2 行同文件（后行覆盖前行，宽容）
        assert_eq!(
            parse("running\t10\nrunning\t20\t30\n"),
            running(20, 30)
        );
        // v2 心跳列坏值 → 降级 v1（heartbeat = None），start 仍可读
        assert_eq!(parse("running\t20\tzz\n"), running_v1(20));
    }
}
