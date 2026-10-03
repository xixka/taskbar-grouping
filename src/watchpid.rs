//! 后台 watch 生命周期（任务 37，审查 P1-D，2026-09-25）。
//!
//! 问题：任务 31 的分离式保活 watch（菜单 `[0]`→`k`）**没有任何停止
//! 途径**——菜单 `[3]` 只能停本会话 spawn 的线程，`status` 不显示运行
//! 实例，也没有 `stop` 命令；提示语 "用 [3] 停止" 与实际不符。group
//! 线路的后台实例还长期持有映射表互斥体，后续 `restore` / group watch
//! 全被拒，用户只剩任务管理器/重启。
//!
//! 修复三件套：
//! 1. **PID 登记**：后台 watch（`--background`，任务 40）全面就绪后
//!    原子写数据目录 `tbg-watch.tsv`（pid / strategy / group / started
//!    四行，组名字符集经 `group_aumid` 校验无 TSV 元字符）；优雅退出
//!    自清，崩溃残留由 `stop` / `status` 检测到死 PID 时回收。
//! 2. **命名停止事件** `Local\tbg-lite.stop.<pid>`（手动重置）：watch
//!    消息泵每 ≤1s 轮询，置位即走优雅退出（与菜单标志 / 控制台信号
//!    同一条收尾路径：摘钩 + 终扫统计 + end_clean）。
//! 3. **`tbg-lite stop`**：读登记 → 存活检查 → 发事件 → 等待 ≤5s →
//!    超时才 `TerminateProcess`（优雅优先，硬杀兜底）。
//!
//! 单会话语义：命名空间 `Local\` + 事件名按 PID 区分——只命中目标
//! 进程；`Global` 跨会话场景由任务 38（互斥体 SID 化）另案处理。
//! 尽力而为：登记失败只告警不阻断 watch 本体。

use std::fs;
use std::path::PathBuf;

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentProcessId, GetExitCodeProcess, OpenEventW, OpenProcess, SetEvent,
    TerminateProcess, WaitForSingleObject, EVENT_MODIFY_STATE, PROCESS_ACCESS_RIGHTS,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
};
use windows::core::HSTRING;

use crate::restoremap;

/// 登记文件名（数据目录，与还原表/health 同目录）。
const WATCH_FILE_NAME: &str = "tbg-watch.tsv";
const HEADER: &str = "# tbg-lite watch v1";
/// 进程仍存活的 `GetExitCodeProcess` 值（STILL_ACTIVE）。
const STILL_ACTIVE: u32 = 259;

/// 登记文件内容（`stop` / `status` 消费）。
#[derive(Debug, Clone)]
pub(crate) struct WatchInfo {
    pub(crate) pid: u32,
    /// "ungroup" | "group"
    pub(crate) strategy: &'static str,
    pub(crate) group: Option<String>,
    pub(crate) started: u64,
}

fn file_path() -> Option<PathBuf> {
    restoremap::data_dir().ok().map(|d| d.join(WATCH_FILE_NAME))
}

/// 停止事件名（Local 命名空间，按 watch 进程 PID 区分）。
pub(crate) fn stop_event_name(pid: u32) -> String {
    format!("Local\\tbg-lite.stop.{pid}")
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// watch 进程侧：全面就绪后写登记（原子替换）。数据目录不可用时返回
/// Ok（无登记即无 stop 能力，watch 本体照常——尽力而为）。
pub(crate) fn register(strategy: &str, group: Option<&str>) -> Result<(), String> {
    let Some(p) = file_path() else { return Ok(()) };
    if let Some(dir) = p.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let pid = unsafe { GetCurrentProcessId() };
    let mut text = format!("{HEADER}\npid\t{pid}\nstrategy\t{strategy}\nstarted\t{}\n", unix_now());
    if let Some(g) = group {
        // 组名经 group_aumid 校验（[A-Za-z0-9._-]，1..=32）——无 TSV 元字符
        text.push_str("group\t");
        text.push_str(g);
        text.push('\n');
    }
    restoremap::atomic_write(&p, &text)
}

/// 读登记（宽容式：认得的行取值；坏文件/坏值 → None）。
pub(crate) fn read() -> Option<WatchInfo> {
    let p = file_path()?;
    let text = fs::read_to_string(&p).ok()?;
    let mut pid: Option<u32> = None;
    let mut strategy = String::new();
    let mut group: Option<String> = None;
    let mut started = 0u64;
    for line in text.lines() {
        let mut parts = line.splitn(2, '\t');
        match (parts.next(), parts.next()) {
            (Some("pid"), Some(v)) => pid = v.trim().parse::<u32>().ok(),
            (Some("strategy"), Some(v)) => strategy = v.trim().to_string(),
            (Some("group"), Some(v)) => group = Some(v.trim().to_string()),
            (Some("started"), Some(v)) => started = v.trim().parse::<u64>().unwrap_or(0),
            _ => {} // 表头 / 未知行 / 坏行：忽略
        }
    }
    let strategy = match strategy.as_str() {
        "group" => "group",
        _ => "ungroup",
    };
    pid.map(|pid| WatchInfo {
        pid,
        strategy,
        group,
        started,
    })
}

/// watch 进程侧：优雅退出时清除本进程的登记（登记属于其他进程时不误删）。
pub(crate) fn clear_if_owned() {
    let mine = read().is_some_and(|i| i.pid == unsafe { GetCurrentProcessId() });
    if mine {
        if let Some(p) = file_path() {
            let _ = fs::remove_file(&p);
        }
    }
}

/// 外部（`stop` 命令）按 PID 清除登记。
pub(crate) fn clear(pid: u32) {
    if read().is_some_and(|i| i.pid == pid) {
        if let Some(p) = file_path() {
            let _ = fs::remove_file(&p);
        }
    }
}

/// watch 进程侧的停止事件（手动重置；Drop 关句柄）。
pub(crate) struct StopEvent(HANDLE);

impl StopEvent {
    /// 创建命名事件。已存在的同名残留事件是良性的：手动重置 + 本次
    /// 创建置非信号态（`false`）。
    pub(crate) fn create() -> Option<StopEvent> {
        let name = HSTRING::from(stop_event_name(unsafe { GetCurrentProcessId() }));
        let handle = unsafe { CreateEventW(None, true, false, &name).ok()? };
        Some(StopEvent(handle))
    }

    /// 是否已收到停止信号（0 超时轮询，消息泵每 ≤1s 一次）。
    pub(crate) fn signaled(&self) -> bool {
        unsafe { WaitForSingleObject(self.0, 0) == WAIT_OBJECT_0 }
    }
}

impl Drop for StopEvent {
    fn drop(&mut self) {
        unsafe { let _ = CloseHandle(self.0); };
    }
}

/// 打开进程句柄（查询 + 同步等待 + 终止权限）。None = 进程不存在
/// 或权限不足（跨用户）。
pub(crate) fn open_process(pid: u32) -> Option<HANDLE> {
    let access = PROCESS_ACCESS_RIGHTS(
        PROCESS_QUERY_LIMITED_INFORMATION.0 | PROCESS_SYNCHRONIZE.0 | PROCESS_TERMINATE.0,
    );
    unsafe { OpenProcess(access, false, pid).ok() }
}

/// 进程是否仍在运行（OpenProcess + GetExitCodeProcess != STILL_ACTIVE；
/// 终止但对象未释放的进程不误报为存活）。
pub(crate) fn is_running(pid: u32) -> bool {
    let Some(h) = open_process(pid) else { return false };
    let mut code = 0u32;
    let ok = unsafe { GetExitCodeProcess(h, &mut code) }.is_ok();
    unsafe { let _ = CloseHandle(h); };
    ok && code == STILL_ACTIVE
}

/// 向目标 watch 发停止信号（打开同名事件并 SetEvent）。
/// false = 事件不存在（目标进程是旧版本 / 已退出）。
pub(crate) fn signal_stop(pid: u32) -> bool {
    let name = HSTRING::from(stop_event_name(pid));
    let Ok(h) = (unsafe { OpenEventW(EVENT_MODIFY_STATE, false, &name) }) else {
        return false;
    };
    let ok = unsafe { SetEvent(h) }.is_ok();
    unsafe { let _ = CloseHandle(h); };
    ok
}

/// 等待进程退出（超时返回 false）。
pub(crate) fn wait_exit(handle: HANDLE, timeout_ms: u32) -> bool {
    unsafe { WaitForSingleObject(handle, timeout_ms) == WAIT_OBJECT_0 }
}

/// 硬杀兜底（`stop` 优雅等待超时后）。返回是否成功。
pub(crate) fn terminate(handle: HANDLE) -> bool {
    unsafe { TerminateProcess(handle, 1) }.is_ok()
}

/// 关闭外部打开的进程句柄。
pub(crate) fn close_handle(handle: HANDLE) {
    unsafe { let _ = CloseHandle(handle); };
}

/// `stop` 的完整流程（CLI `tbg-lite stop` 与菜单 `[3]` 共用）。
/// 优雅优先（命名事件 → 消息泵 ≤1s 轮询 → 摘钩/终扫/end_clean/自清），
/// 5s 超时才硬杀；登记不存在/进程已死/操作失败逐项上报。
pub(crate) fn stop_registered() -> StopResult {
    let Some(info) = read() else {
        return StopResult::NoWatch;
    };
    let Some(hproc) = open_process(info.pid) else {
        clear(info.pid);
        return StopResult::StaleCleared { pid: info.pid };
    };
    let out = if signal_stop(info.pid) {
        if wait_exit(hproc, 5000) {
            StopResult::Graceful { pid: info.pid }
        } else if terminate(hproc) {
            StopResult::TerminatedTimeout { pid: info.pid }
        } else {
            StopResult::Failed { pid: info.pid }
        }
    } else if terminate(hproc) {
        // 事件不可达（旧版本后台实例 / 事件句柄损坏）：退化为硬杀
        StopResult::TerminatedNoEvent { pid: info.pid }
    } else {
        StopResult::Failed { pid: info.pid }
    };
    close_handle(hproc);
    // 收尾清登记：优雅路径进程退出前已自清（幂等）；非优雅路径进程已
    // 死，这里兜底。pid 校验防止误删新实例的登记。
    clear(info.pid);
    out
}

/// `stop_registered` 的结果（调用方各自渲染输出文案）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StopResult {
    /// 无登记（没有后台实例）。
    NoWatch,
    /// 登记的进程已死，陈旧登记已回收。
    StaleCleared { pid: u32 },
    /// 优雅退出成功（收到事件、5s 内退出）。
    Graceful { pid: u32 },
    /// 停止事件不可达，直接终止成功。
    TerminatedNoEvent { pid: u32 },
    /// 优雅等待超时，终止成功。
    TerminatedTimeout { pid: u32 },
    /// 终止失败（权限不足等，建议 taskkill）。
    Failed { pid: u32 },
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::WAIT_EVENT;

    #[test]
    fn stop_event_name_contains_pid() {
        assert_eq!(stop_event_name(42), "Local\\tbg-lite.stop.42");
        assert_eq!(stop_event_name(0), "Local\\tbg-lite.stop.0");
    }

    #[test]
    fn wait_object_0_is_signaled() {
        // WaitForSingleObject 轮询判定的等价断言：WAIT_OBJECT_0 == 0
        assert_eq!(WAIT_OBJECT_0.0, 0u32);
        assert_ne!(WAIT_EVENT(258).0, WAIT_OBJECT_0.0);
    }
}
