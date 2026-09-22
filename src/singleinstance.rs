//! 映射表单实例互斥（审计 BUG-02，任务 22）。
//!
//! 线路二的还原映射表是"整文件读入内存 → 整文件覆盖写"模型：两个实例
//! 并发运行时，后启动实例的内存快照不含先启动实例此后写入的条目，其
//! 任意一次 `save()` 都会把对方的条目从文件中抹掉（last-writer-wins），
//! 被抹窗口的原 AUMID 在工具内不可恢复。`watch` 与 `restore` 并发同理
//! （restore 移除条目后，常驻 watch 用旧快照 save 会把陈旧条目写回）。
//!
//! 修复：所有会写映射表的进程（`watch --strategy group` 全程、`restore`
//! 处理共享 AUMID 窗口期间）必须先持有命名互斥体 `Local\tbg-lite.map`
//! （会话命名空间，单用户会话内互斥）；已有持有者时拒绝运行并提示。
//! 线路一 `watch --strategy ungroup` 不写映射表，不参与互斥。

use windows::core::HSTRING;
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex};

/// 互斥体名（`Local\` 前缀 = 当前登录会话命名空间）。
const MUTEX_NAME: &str = "Local\\tbg-lite.map";

/// 映射表互斥守卫：持有至 Drop（进程退出/命令结束自动释放）。
pub(crate) struct MapMutex(HANDLE);

impl MapMutex {
    /// 获取互斥体；已有实例持有（ERROR_ALREADY_EXISTS）时返回 Err，
    /// 由调用方拒绝运行（单实例红线，审计 BUG-02 修复方案 1）。
    pub(crate) fn acquire() -> Result<Self, String> {
        unsafe {
            let name = HSTRING::from(MUTEX_NAME);
            let handle = CreateMutexW(None, false, &name)
                .map_err(|e| format!("cannot create mutex '{MUTEX_NAME}': {e}"))?;
            // CreateMutexW 成功返回后 GetLastError 保留创建原因；
            // 期间不得再调其他 Win32 API（windows crate 包装在成功路径
            // 上不触碰 LastError，已核对 0.58 源码）。
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(handle);
                return Err(format!(
                    "another tbg-lite instance holds the restore map ({MUTEX_NAME}); \
                     stop it before running this command (concurrency guard, audit BUG-02)"
                ));
            }
            Ok(Self(handle))
        }
    }
}

impl Drop for MapMutex {
    fn drop(&mut self) {
        unsafe {
            let _ = ReleaseMutex(self.0);
            let _ = CloseHandle(self.0);
        }
    }
}
