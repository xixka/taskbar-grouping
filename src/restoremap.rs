//! 线路二（共享 AUMID 分组，任务 8）的原值还原映射表。
//!
//! 共享 AUMID（`TBG.Group.<name>`）会整体覆盖每窗口原值，无法像线路一
//! （每窗口后缀）那样从当前值内联还原，因此改写前先把
//! `HWND -> (共享值, 原始 AUMID)` 落盘，供 `restore` 复原——对应
//! docs/plan.md §2.3 / 附录 B.3 引用的 ShelfyGAI `recovery` 模式。
//!
//! 文件为 TSV，与 exe 同目录（PoC 阶段的选择；正式配置路径未定，见
//! AGENTS.md 待确认项），每行三列：
//! `<HWND大写十六进制>\t<共享AUMID>\t<原始AUMID>`；原始列为空串表示
//! "原本无 AUMID，还原时应清除属性"。表空时文件删除。
//!
//! 防误还原设计：查询时要求窗口当前 AUMID 与记录的共享值完全一致才
//! 返回原值（HWND 数值可能被系统复用，不一致即视为非本条目所指窗口）。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// 映射表文件名（位于 exe 同目录）。
pub(crate) const MAP_FILE_NAME: &str = "tbg-restore.tsv";

pub(crate) struct RestoreMap {
    path: PathBuf,
    /// HWND 数值 -> (写入的共享 AUMID, 原始 AUMID；空串 = 原本无 AUMID)。
    entries: BTreeMap<usize, (String, String)>,
}

impl RestoreMap {
    /// 载入映射表；文件不存在 = 空表。文件存在但损坏时返回 Err：
    /// 宁可停止线路二运行（保住还原能力），也不静默丢弃记录。
    pub(crate) fn load(dir: &Path) -> Result<Self, String> {
        let path = dir.join(MAP_FILE_NAME);
        let mut entries = BTreeMap::new();
        if path.exists() {
            let text =
                fs::read_to_string(&path).map_err(|e| format!("restore map read failed: {e}"))?;
            for (i, line) in text.lines().enumerate() {
                if line.is_empty() {
                    continue;
                }
                let mut parts = line.splitn(3, '\t');
                let parsed = match (parts.next(), parts.next(), parts.next()) {
                    (Some(h), Some(g), Some(o)) => Some((h, g, o)),
                    _ => None,
                };
                let Some((h, g, o)) = parsed else {
                    return Err(format!(
                        "restore map corrupt at line {} (expected 3 TSV fields): '{line}'",
                        i + 1
                    ));
                };
                let key = usize::from_str_radix(h.trim_start_matches("0x"), 16)
                    .map_err(|_| format!("restore map corrupt at line {} (bad HWND '{h}')", i + 1))?;
                entries.insert(key, (g.to_string(), o.to_string()));
            }
        }
        Ok(Self { path, entries })
    }

    /// 记录一次线路二改写（改写 AUMID 之前调用，落盘成功后才真正写入）。
    pub(crate) fn record(&mut self, hwnd: usize, group_value: &str, original: &str) {
        self.entries
            .insert(hwnd, (group_value.to_string(), original.to_string()));
    }

    /// 删除条目（窗口销毁 / 改写失败回滚时）。返回条目是否先前存在。
    pub(crate) fn remove(&mut self, hwnd: usize) -> bool {
        self.entries.remove(&hwnd).is_some()
    }

    /// 取出原值：仅当窗口当前 AUMID 与记录的共享值一致时才返回并移除
    /// 条目；不一致（无条目 / HWND 已被复用）返回 None。
    pub(crate) fn take(&mut self, hwnd: usize, current_group_value: &str) -> Option<String> {
        match self.entries.get(&hwnd) {
            Some((g, _)) if g == current_group_value => {
                let original = self.entries.get(&hwnd).map(|(_, o)| o.clone())?;
                self.entries.remove(&hwnd);
                Some(original)
            }
            _ => None,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 保存映射表；表空时删除文件，保持目录干净（还原完毕即无痕）。
    pub(crate) fn save(&self) -> Result<(), String> {
        if self.is_empty() {
            if self.path.exists() {
                fs::remove_file(&self.path)
                    .map_err(|e| format!("restore map remove failed: {e}"))?;
            }
            return Ok(());
        }
        let mut out = String::new();
        for (k, (g, o)) in &self.entries {
            out.push_str(&format!("{k:X}\t{g}\t{o}\n"));
        }
        fs::write(&self.path, out).map_err(|e| format!("restore map write failed: {e}"))
    }
}
