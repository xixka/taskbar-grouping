//! 映射表单实例互斥（审计 BUG-02，任务 22；任务 48 审查 P2-G 升级）。
//!
//! 线路二的还原映射表是"整文件读入内存 → 整文件覆盖写"模型：两个实例
//! 并发运行时，后启动实例的内存快照不含先启动实例此后写入的条目，其
//! 任意一次 `save()` 都会把对方的条目从文件中抹掉（last-writer-wins），
//! 被抹窗口的原 AUMID 在工具内不可恢复。`watch` 与 `restore` 并发同理
//! （restore 移除条目后，常驻 watch 用旧快照 save 会把陈旧条目写回）。
//!
//! 修复：所有会写映射表的进程（`watch --strategy group` 全程、`restore`
//! 处理共享 AUMID 窗口期间）必须先持有命名互斥体；已有持有者时拒绝
//! 运行并提示。线路一 `watch --strategy ungroup` 不写映射表，不参与。
//!
//! **任务 48（审查 P2-G，2026-09-25）——作用域升级**：旧名
//! `Local\tbg-lite.map` 只在**单个登录会话**内唯一——本地登录 + RDP
//! 同一用户双会话并发时互斥完全失效，映射表照旧 last-writer-wins，
//! 正是审计 BUG-02 要防的写表红线。改为 `Global\tbg-lite.map.<用户SID>`：
//! - `Global\` 跨会话可见（映射表在 `%LOCALAPPDATA%` 本就按用户共享，
//!   互斥作用域与之对齐：同用户跨会话互斥、不同用户互不干扰）；
//! - SID 经 `GetTokenInformation(TokenUser)` 读取（不可伪造的用户
//!   身份，`USERNAME` 环境变量可被同会话内随意设置，不采用）；
//! - SID 读取失败（极端环境/权限）回退 `Local\` 并告警一次——退回
//!   旧会话内行为，不阻断命令（互斥是安全网，不该成为新的故障点）。
//! 兼容性：升级窗口期新旧版本混跑时互斥对象名不同 → 并发防护短暂
//! 缺失（同机混跑罕见，release notes 已声明）。

use windows::core::{HSTRING, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_ALREADY_EXISTS, HANDLE, HLOCAL,
};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY};
use windows::Win32::System::Threading::{
    CreateMutexW, GetCurrentProcess, OpenProcessToken, ReleaseMutex,
};

/// 互斥体名（Global + 用户 SID；读不出 SID 时回退 Local）。
fn mutex_name() -> String {
    match user_sid() {
        Some(sid) => format!("Global\\tbg-lite.map.{sid}"),
        None => {
            eprintln!(
                "tbg-lite: warning: cannot read the user SID — \
                 map mutex falls back to the session-local scope"
            );
            "Local\\tbg-lite.map".to_string()
        }
    }
}

/// 读取当前进程令牌的用户 SID 字符串（`S-1-5-…`）。尽力而为：任何
/// 失败返回 None（调用方回退 `Local\` 命名空间）。
fn user_sid() -> Option<String> {
    unsafe {
        let mut token = HANDLE::default();
        // GetCurrentProcess 伪句柄；TOKEN_QUERY 足够读 TokenUser
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
        // 两段式：先问长度（预期失败 ERROR_INSUFFICIENT_BUFFER）
        let mut len = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
        if len == 0 {
            let _ = CloseHandle(token);
            return None;
        }
        let mut buf = vec![0u8; len as usize];
        let ok = GetTokenInformation(
            token,
            TokenUser,
            Some(buf.as_mut_ptr() as *mut core::ffi::c_void),
            len,
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(token);
        if !ok {
            return None;
        }
        let user = &*(buf.as_ptr() as *const windows::Win32::Security::TOKEN_USER);
        let mut sid_str = PWSTR::null();
        if ConvertSidToStringSidW(user.User.Sid, &mut sid_str).is_err() {
            return None;
        }
        let s = pwstr_to_string(sid_str);
        // 释放 advapi32 分配的字符串（失败不致命：一次性查询的微量分配）
        let _ = LocalFree(HLOCAL(sid_str.0 as *mut core::ffi::c_void));
        s
    }
}

/// NUL 结尾宽字符串 → String（读至 NUL；空指针 → None）。
fn pwstr_to_string(p: PWSTR) -> Option<String> {
    if p.0.is_null() {
        return None;
    }
    let mut wide = Vec::new();
    let mut q = p.0 as *const u16;
    unsafe {
        while *q != 0 {
            wide.push(*q);
            q = q.add(1);
        }
    }
    Some(String::from_utf16_lossy(&wide))
}

/// 命名互斥体获取（共享逻辑）：`ERROR_ALREADY_EXISTS` = 已有持有者，
/// 返回带互斥体名的拒绝消息。
fn acquire_named(name: &str) -> Result<HANDLE, String> {
    unsafe {
        let hname = HSTRING::from(name);
        let handle = CreateMutexW(None, false, &hname)
            .map_err(|e| format!("cannot create mutex '{name}': {e}"))?;
        // CreateMutexW 成功返回后 GetLastError 保留创建原因；
        // 期间不得再调其他 Win32 API（windows crate 包装在成功路径
        // 上不触碰 LastError，已核对 0.58 源码）。
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(handle);
            return Err(format!(
                "another tbg-lite instance holds '{name}' (concurrency guard); \
                 stop it before running this command"
            ));
        }
        Ok(handle)
    }
}

/// 释放互斥体（Drop 共享）。
fn release_mutex(handle: HANDLE) {
    unsafe {
        let _ = ReleaseMutex(handle);
        let _ = CloseHandle(handle);
    }
}

/// 映射表互斥守卫：持有至 Drop（进程退出/命令结束自动释放）。
pub(crate) struct MapMutex(HANDLE);

impl MapMutex {
    /// 获取互斥体；已有实例持有（ERROR_ALREADY_EXISTS）时返回 Err，
    /// 由调用方拒绝运行（单实例红线，审计 BUG-02 修复方案 1）。
    pub(crate) fn acquire() -> Result<Self, String> {
        let name = mutex_name();
        let handle = acquire_named(&name).map_err(|_| {
            format!(
                "another tbg-lite instance holds the restore map ({name}); \
                 stop it before running this command (concurrency guard, audit BUG-02)"
            )
        })?;
        Ok(Self(handle))
    }
}

impl Drop for MapMutex {
    fn drop(&mut self) {
        release_mutex(self.0);
    }
}

/// 任务 49（审查 P3-M，2026-09-25）：watch 单实例互斥。
///
/// 同一策略线路（+组名）的 watch 并发运行没有意义且互相打架：改写同
/// 一批窗口、标记互冲、统计互相污染、shell 重启重扫重复执行。互斥名
/// `Local\tbg-lite.watch.<strategy>[.<group>]`（ungroup 一把、每个组名
/// 一把）——与映射表互斥（Global+SID）**正交**：ungroup watch 不写映射
/// 表不持 MapMutex，但同线路双开仍拒；不同线路可并存（既有语义）。
/// 守卫持有至 `winevent::run` 返回（线程结束/进程退出自动释放）。
pub(crate) struct WatchMutex(HANDLE);

impl WatchMutex {
    pub(crate) fn acquire(strategy: &str, group: Option<&str>) -> Result<Self, String> {
        let name = match group {
            Some(g) => format!("Local\\tbg-lite.watch.{strategy}.{g}"),
            None => format!("Local\\tbg-lite.watch.{strategy}"),
        };
        let handle = acquire_named(&name).map_err(|_| {
            format!(
                "another tbg-lite watch is already running on this strategy line ({name}); \
                 stop it first: 'tbg-lite stop' (background), menu [3], or Ctrl+C"
            )
        })?;
        Ok(Self(handle))
    }
}

impl Drop for WatchMutex {
    fn drop(&mut self) {
        release_mutex(self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pwstr_null_is_none() {
        assert!(pwstr_to_string(PWSTR::null()).is_none());
    }

    #[test]
    fn pwstr_reads_until_nul() {
        // "S-1-5-21-..." + NUL + 尾随垃圾：只读至 NUL
        let mut wide: Vec<u16> = "S-1-5-21-x".encode_utf16().collect();
        wide.push(0);
        wide.push(0xdead);
        let p = PWSTR(wide.as_mut_ptr());
        assert_eq!(pwstr_to_string(p).as_deref(), Some("S-1-5-21-x"));
    }
}
