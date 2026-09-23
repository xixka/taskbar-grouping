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
//! running<TAB><unix_start>
//! ```
//!
//! 语义：
//! - `watch` 启动时 `begin()`：读状态；若上一轮留有 `running` 行（该轮
//!   未正常收尾）且存活时长 < `ABNORMAL_MAX_UPTIME_SECS`（30s）→
//!   streak+1（短命消失 = 崩溃/启动即死）；存活 ≥ 阈值后被杀（用户
//!   Ctrl+C / 任务管理器终结一个健康长驻进程）→ streak 清零（人工
//!   干预不是崩溃循环）；无 `running` 行（上轮正常收尾）→ streak 保持。
//! - streak ≥ `TRIP_THRESHOLD`（3）→ **熔断**：调用方（`winevent::run`）
//!   注销自启并告警；随后 streak 归零重新计数（熔断目的是切断开机循环，
//!   不是拒绝用户手动运行——本次 watch 照常执行）。
//! - 优雅退出（时长自然结束 / 菜单停止标志 / dry-run 结束）时
//!   `end_clean()`：移除 `running` 行、streak 归零（本轮健康跑完）。
//!
//! 记账核心 `account` 是纯函数（时钟由参数注入，单测覆盖）；文件 IO
//! 尽力而为——读失败按无历史、写失败不阻断 watch（熔断是安全网，
//! 不该成为新的故障点）。

use std::path::PathBuf;

use crate::restoremap;

const HEALTH_FILE_NAME: &str = "tbg-health.tsv";
const HEADER: &str = "# tbg-lite health v1";
/// 单轮存活不足该时长即消失 = 异常退出（崩溃 / 启动即死）。
pub(crate) const ABNORMAL_MAX_UPTIME_SECS: u64 = 30;
/// 连续异常退出达到该次数 → 熔断（注销自启）。
pub(crate) const TRIP_THRESHOLD: u32 = 3;

pub(crate) struct Health {
    path: Option<PathBuf>,
    /// 本次 begin 是否触发熔断（调用方据此注销自启 + 告警）。
    pub(crate) tripped: bool,
}

impl Health {
    /// 优雅退出时调用：移除 `running` 行、streak 归零。尽力而为。
    pub(crate) fn end_clean(&self) {
        let Some(path) = &self.path else { return };
        let _ = std::fs::write(path, render(0, None));
    }
}

/// watch 启动入口：读状态 → 记账 → 写入本轮 `running` 行。
/// 返回值携带 `tripped`（streak 达阈值），调用方负责注销自启与告警；
/// 熔断后 streak 已归零（防每次手动运行都重复告警）。
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
        let _ = std::fs::write(p, render(streak, Some(now)));
    }
    Health {
        path,
        tripped,
    }
}

/// 记账核心（纯函数，时钟注入，单测覆盖）。返回 `(新 streak, 是否熔断)`：
/// - 上轮短命消失（< `ABNORMAL_MAX_UPTIME_SECS`）→ streak+1；
/// - 上轮长寿命后被杀 → streak 清零（人工干预，非循环）；
/// - 上轮正常收尾 → streak 保持；
/// - streak ≥ `TRIP_THRESHOLD` → 熔断，streak 归零。
pub(crate) fn account(now: u64, prev_running_start: Option<u64>, prev_streak: u32) -> (u32, bool) {
    let streak = match prev_running_start {
        Some(start) => {
            if now.saturating_sub(start) < ABNORMAL_MAX_UPTIME_SECS {
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
/// 返回 `(streak, running 起始时刻)`。
pub(crate) fn parse(content: &str) -> (u32, Option<u64>) {
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
                if let Ok(t) = value.parse::<u64>() {
                    running = Some(t);
                }
            }
            _ => {} // 表头 / 未知行：忽略
        }
    }
    (streak, running)
}

/// 渲染状态文件（与 `parse` 互逆；`running == None` = 正常收尾态）。
pub(crate) fn render(streak: u32, running: Option<u64>) -> String {
    match running {
        Some(t) => format!("{HEADER}\nabnormal_streak\t{streak}\nrunning\t{t}\n"),
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

    #[test]
    fn account_short_abnormal_exit_increments() {
        // 上轮 running 起 10s 后消失（本 begin 时刻 = start+10）→ +1
        assert_eq!(account(1000, Some(990), 0), (1, false));
        // 2 + 1 = 3 达阈值 → 熔断且归零（streak 重置，防每次运行重复告警）
        assert_eq!(account(1000, Some(975), 2), (0, true));
    }

    #[test]
    fn account_long_lived_kill_resets() {
        // 上轮存活 120s 后被杀（健康长驻被人工终结）→ 清零，不累计
        assert_eq!(account(1000, Some(880), 2), (0, false));
        // 恰好等于阈值：30s 不算"短命"（< 30 才算）
        assert_eq!(account(1000, Some(970), 2), (0, false));
    }

    #[test]
    fn account_clean_finish_keeps_streak() {
        // 上轮正常收尾（无 running 行）：streak 保持（trip 已在上轮归零；
        // 正常收尾文件里 streak 本就是 0）
        assert_eq!(account(1000, None, 0), (0, false));
        assert_eq!(account(1000, None, 2), (2, false));
    }

    #[test]
    fn account_trips_exactly_at_threshold() {
        // streak 从 0 连续 3 次短命 → 第 3 次熔断
        let (_, t1) = account(1000, Some(995), 0);
        assert!(!t1);
        let (_, t2) = account(2000, Some(1995), 1);
        assert!(!t2);
        let (s3, t3) = account(3000, Some(2995), 2);
        assert!(t3);
        assert_eq!(s3, 0); // 熔断后归零
    }

    #[test]
    fn parse_render_roundtrip() {
        // 运行态
        let text = render(2, Some(1700000000));
        assert_eq!(parse(&text), (2, Some(1700000000)));
        // 收尾态（无 running 行）
        let text2 = render(0, None);
        assert_eq!(parse(&text2), (0, None));
        // 宽容式：表头/未知行/坏值忽略
        let junk = "# tbg-lite health v1\nabnormal_streak\tzz\nrunning\tabc\nwhatever\tx\n";
        assert_eq!(parse(junk), (0, None));
        // 无表头也能读
        assert_eq!(parse("abnormal_streak\t1\nrunning\t42\n"), (1, Some(42)));
    }
}
