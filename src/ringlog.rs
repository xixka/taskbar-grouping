//! 环形日志（任务 20，docs/plan.md v2 §3 Phase 3）。
//!
//! 默认关闭；`watch --log` 开启。文件位于数据目录
//! （`%LOCALAPPDATA%\tbg-lite\tbg.log`，与还原表同目录）。
//! "环形"语义：文件超过 `MAX_BYTES`（256 KiB）时保留后半段（从
//! 中点起的下一行边界），tmp + rename 原子替换（与还原表同款写法，
//! 审计 BUG-03 的纪律）——日志永远有界，长驻 watch 不会写满磁盘。
//!
//! 事件选择：只记排障关键事件（启动/停止/启动扫存量/explorer 重启/
//! 重扫/写入失败/熔断），不逐窗口记流（stdout 已有全量；环形日志的
//! 价值在进程被杀后留下现场）。时间戳为 Unix 秒（无时区依赖，
//! 排障时由阅读者换算）。
//!
//! 单线程假设：仅 watch 线程（`winevent::run`）持有并写入，无锁。

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use crate::restoremap;

/// 日志文件名（位于数据目录，见 `restoremap::data_dir`）。
pub(crate) const LOG_FILE_NAME: &str = "tbg.log";

/// 文件上限：超过即截半（保留后半段）。
const MAX_BYTES: u64 = 256 * 1024;

pub(crate) struct RingLog {
    /// None = 未启用（默认）。启用但数据目录不可用时也为 None（告警一次）。
    path: Option<PathBuf>,
}

impl RingLog {
    /// `enabled == false` → 空操作日志（零开销）。启用时惰性建目录：
    /// 建不出目录则降级为禁用并告警（日志永远不该阻断 watch 本体）。
    pub(crate) fn open(enabled: bool) -> Self {
        if !enabled {
            return Self { path: None };
        }
        let path = match restoremap::data_dir() {
            Ok(dir) => {
                if let Err(e) = fs::create_dir_all(&dir) {
                    eprintln!(
                        "watch: --log: cannot create the data directory ({}): {e}; logging disabled",
                        dir.display()
                    );
                    return Self { path: None };
                }
                Some(dir.join(LOG_FILE_NAME))
            }
            Err(e) => {
                eprintln!("watch: --log: {e}; logging disabled");
                None
            }
        };
        Self { path }
    }

    pub(crate) fn enabled(&self) -> bool {
        self.path.is_some()
    }

    /// 追加一行（无 IO 错误上抛：日志失败不影响主流程）。
    pub(crate) fn log(&self, msg: &str) {
        let Some(path) = &self.path else { return };
        let ts = unix_now();
        let line = format!("[{ts}] {msg}\n");
        if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = f.write_all(line.as_bytes());
            let _ = f.flush();
        }
        // 截半：超上限 → 保留后半段（tmp + rename 原子替换）
        if let Ok(md) = fs::metadata(path) {
            if md.len() > MAX_BYTES {
                if let Ok(content) = fs::read_to_string(path) {
                    let keep = trim_keep_tail(&content, (content.len() as u64) / 2);
                    let tmp = PathBuf::from(format!("{}.tmp", path.display()));
                    if fs::write(&tmp, keep).is_ok() {
                        let _ = fs::rename(&tmp, path);
                    }
                }
            }
        }
    }
}

/// 截半核心（纯函数，单测）：返回 `content` 从 `target_start` 起的
/// 首个**完整行**——`target_start` 恰在行边界（行首）时从该行保留；
/// 落在行中间时丢弃该半行，从下一行行首保留；`target_start` 起再无
/// 换行（极端：单行超长）→ 整段保留（宁多勿丢）。
pub(crate) fn trim_keep_tail(content: &str, target_start: u64) -> &str {
    if content.is_empty() {
        return content;
    }
    let start = (target_start as usize).min(content.len());
    if start >= content.len() {
        // 越界（极端输入）：整段保留，不丢日志
        return content;
    }
    if start == 0 || content.as_bytes()[start - 1] == b'\n' {
        // 恰在行边界：从该行行首保留
        return &content[start..];
    }
    match content[start..].find('\n') {
        Some(i) => &content[start + i + 1..],
        None => content,
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
    fn trim_cuts_at_next_newline_after_half() {
        // 4 行等长；中点恰落在第 3 行行首（content[9]=='\n'）→ 从 "cccc" 行保留
        let content = "aaaa\nbbbb\ncccc\ndddd\n";
        assert_eq!(trim_keep_tail(content, 10), "cccc\ndddd\n");
        // 中点落在行中间（content[7]=='b'，非边界）→ 丢半行，从下一行行首保留
        assert_eq!(trim_keep_tail("aaaa\nbbbb\ncccc\n", 7), "cccc\n");
    }

    #[test]
    fn trim_start_at_zero_or_beyond_len() {
        // start=0 = 行边界：整段保留（日志尚未超限时不会走到这里，防御语义）
        assert_eq!(trim_keep_tail("a\nb\nc\n", 0), "a\nb\nc\n");
        // 越界（target_start 超过长度）：整段保留，不丢日志
        assert_eq!(trim_keep_tail("a\nb\nc\n", 999), "a\nb\nc\n");
    }

    #[test]
    fn trim_without_newline_keeps_everything() {
        // 单行超长无换行：宁多勿丢
        assert_eq!(trim_keep_tail("abcdefgh", 3), "abcdefgh");
        assert_eq!(trim_keep_tail("", 0), "");
    }
}
