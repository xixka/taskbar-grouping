//! 线路二（共享 AUMID 分组，任务 8）的原值还原映射表。
//!
//! 共享 AUMID（`TBG.Group.<name>`）会整体覆盖每窗口原值，无法像线路一
//! （每窗口后缀）那样从当前值内联还原，因此改写前先把
//! `HWND -> (共享值, 原始 AUMID)` 落盘，供 `restore` 复原——对应
//! docs/plan.md §2.3 / 附录 B.3 引用的 ShelfyGAI `recovery` 模式。
//!
//! 持久化位置与完整性（审计修复，任务 22）：
//! - 路径迁至 `%LOCALAPPDATA%\tbg-lite\`（审计 SEC-01）：避开 exe 同目录
//!   的本地篡改面，兼解决部署到 Program Files 等只读目录时线路二静默
//!   失效的问题；首次发现旧位置（exe 同目录）存在旧表时自动整体迁移。
//! - 原子写（审计 BUG-03）：先写同目录临时文件并 `sync_all`，再
//!   `fs::rename`（Windows 上为 `MoveFileEx(MOVEFILE_REPLACE_EXISTING)`
//!   语义的原子替换）。Ctrl+C 强杀/崩溃至多丢失临时文件，主表永不为
//!   半行残文。
//! - 表头 magic `# tbg-restore.tsv v1`：完整性辅助识别；解析时跳过
//!   `#` 开头的行，无表头的 v0 旧表仍可读（迁移/兼容）。
//!
//! 文件为 TSV，每行三列：`<HWND大写十六进制>\t<共享AUMID>\t<原始AUMID>`；
//! 原始列为空串表示"原本无 AUMID，还原时应清除属性"。表空时文件删除。
//!
//! 防误还原设计：查询时要求窗口当前 AUMID 与记录的共享值完全一致才
//! 返回原值（HWND 数值可能被系统复用，不一致即视为非本条目所指窗口）。
//! 并发保护（审计 BUG-02）由 `singleinstance::MapMutex` 承担：写表的
//! watch（group）与 restore 互斥运行，杜绝 last-writer-wins 丢原值。

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

/// 映射表文件名（位于数据目录，见 `data_dir`）。
pub(crate) const MAP_FILE_NAME: &str = "tbg-restore.tsv";

/// 表头 magic 行（审计 BUG-03：完整性辅助识别 + 格式版本声明）。
pub(crate) const MAP_HEADER: &str = "# tbg-restore.tsv v1";

/// 数据目录名（`%LOCALAPPDATA%` 下；审计 SEC-01）。
const DATA_DIR_NAME: &str = "tbg-lite";

pub(crate) struct RestoreMap {
    path: PathBuf,
    /// HWND 数值 -> (写入的共享 AUMID, 原始 AUMID；空串 = 原本无 AUMID)。
    entries: BTreeMap<usize, (String, String)>,
}

/// 映射表数据目录：`%LOCALAPPDATA%\tbg-lite`（审计 SEC-01）。
/// `LOCALAPPDATA` 缺失（极端环境）时退回 exe 同目录，保持可用。
pub(crate) fn data_dir() -> Result<PathBuf, String> {
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        if !local.is_empty() {
            return Ok(PathBuf::from(local).join(DATA_DIR_NAME));
        }
    }
    exe_dir()
}

fn exe_dir() -> Result<PathBuf, String> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .ok_or_else(|| "cannot locate exe directory".to_string())
}

fn tmp_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.tmp", path.display()))
}

/// 原子写（审计 BUG-03）：同目录临时文件 + fsync + rename 原子替换。
/// 任务 33（审查 P0-B/O）：提升为 `pub(crate)` 供 health 状态文件复用
/// （熔断心跳每 5s 重写一次，同样不能落半行残文）；错误文案保持通用。
pub(crate) fn atomic_write(path: &Path, content: &str) -> Result<(), String> {
    let tmp = tmp_path(path);
    let mut f = File::create(&tmp)
        .map_err(|e| format!("atomic write: tmp create failed ({}): {e}", tmp.display()))?;
    f.write_all(content.as_bytes())
        .map_err(|e| format!("atomic write: tmp write failed: {e}"))?;
    // 落盘完成后再替换主文件，崩溃窗口期内主表保持上一个完整版本
    f.sync_all()
        .map_err(|e| format!("atomic write: tmp sync failed: {e}"))?;
    drop(f);
    fs::rename(&tmp, path)
        .map_err(|e| format!("atomic write: rename failed ({} -> {}): {e}", tmp.display(), path.display()))
}

fn serialize(entries: &BTreeMap<usize, (String, String)>) -> String {
    let mut out = String::from(MAP_HEADER);
    out.push('\n');
    for (k, (g, o)) in entries {
        out.push_str(&format!("{k:X}\t{g}\t{o}\n"));
    }
    out
}

/// 解析表文本（v1 表头行与 v0 无表头格式均兼容；`#` 开头行跳过）。
fn parse(text: &str) -> Result<BTreeMap<usize, (String, String)>, String> {
    let mut entries = BTreeMap::new();
    for (i, line) in text.lines().enumerate() {
        if line.is_empty() || line.starts_with('#') {
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
    Ok(entries)
}

impl RestoreMap {
    /// 载入映射表（`dir` 应传 `data_dir()`）；文件不存在 = 空表。文件存在但
    /// 损坏时返回 Err：宁可停止线路二运行（保住还原能力），也不静默丢弃记录。
    ///
    /// 旧表迁移（审计 SEC-01 路径变更的兼容）：新位置不存在而 exe 同目录
    /// 存在旧表时，整体搬到新位置（解析 → 原子写 → 删旧）；迁移失败不阻断
    /// 载入（旧表原样保留，下次再试）。
    pub(crate) fn load(dir: &Path) -> Result<Self, String> {
        fs::create_dir_all(dir)
            .map_err(|e| format!("restore map dir create failed ({}): {e}", dir.display()))?;
        let path = dir.join(MAP_FILE_NAME);
        if !path.exists() {
            migrate_legacy(&path)?;
        }
        let mut entries = BTreeMap::new();
        if path.exists() {
            let text =
                fs::read_to_string(&path).map_err(|e| format!("restore map read failed: {e}"))?;
            entries = parse(&text)?;
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

    /// 预览原值（`restore --dry-run`，任务 25）：与 `take` 同样的校验，
    /// 但不移除条目、不改动表。
    pub(crate) fn peek(&self, hwnd: usize, current_group_value: &str) -> Option<String> {
        match self.entries.get(&hwnd) {
            Some((g, o)) if g == current_group_value => Some(o.clone()),
            _ => None,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 保存映射表（原子写）；表空时删除文件，保持目录干净（还原完毕即无痕）。
    pub(crate) fn save(&self) -> Result<(), String> {
        if self.is_empty() {
            if self.path.exists() {
                fs::remove_file(&self.path)
                    .map_err(|e| format!("restore map remove failed: {e}"))?;
            }
            let tmp = tmp_path(&self.path);
            if tmp.exists() {
                let _ = fs::remove_file(&tmp);
            }
            return Ok(());
        }
        atomic_write(&self.path, &serialize(&self.entries))
    }
}

/// 映射表只读状态（任务 19 `status`）。
#[derive(Debug)]
pub(crate) enum MapStatus {
    /// 文件不存在（无待还原的线路二原值）。
    Absent,
    /// 可解析，含 `entries` 条记录。
    Intact { entries: usize },
    /// 文件存在但损坏（原文保留供人工处置——fail-safe，不静默丢弃）。
    Corrupt(String),
}

/// 只读探测映射表状态：**不建目录、不触发旧表迁移、不写任何文件**
/// （审计 BUG-02 红线：任何写 `tbg-restore.tsv` 的路径必须先持
/// `Local\tbg-lite.map` 互斥体；与 `RestoreMap::load` 的副作用路径刻意
/// 分离——load 会 create_dir_all + 迁移旧表，status 绝不碰盘）。
pub(crate) fn status(dir: &Path) -> MapStatus {
    let path = dir.join(MAP_FILE_NAME);
    if !path.exists() {
        return MapStatus::Absent;
    }
    match fs::read_to_string(&path) {
        Err(e) => MapStatus::Corrupt(format!("read failed: {e}")),
        Ok(text) => match parse(&text) {
            Ok(entries) => MapStatus::Intact {
                entries: entries.len(),
            },
            Err(e) => MapStatus::Corrupt(e),
        },
    }
}

/// 旧表一次性迁移（exe 同目录 → 新数据目录）。失败仅忽略：旧表原样保留。
fn migrate_legacy(new_path: &Path) -> Result<(), String> {
    let Ok(legacy_dir) = exe_dir() else {
        return Ok(());
    };
    let legacy = legacy_dir.join(MAP_FILE_NAME);
    if !legacy.exists() || legacy == new_path {
        return Ok(());
    }
    let text = match fs::read_to_string(&legacy) {
        Ok(t) => t,
        Err(_) => return Ok(()),
    };
    let entries = match parse(&text) {
        Ok(e) => e,
        Err(_) => return Ok(()), // 旧表损坏：不动，留待人工处置（fail-safe）
    };
    if atomic_write(new_path, &serialize(&entries)).is_ok() {
        let _ = fs::remove_file(&legacy);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个测试独立的临时目录（不引第三方依赖）。
    fn test_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir()
            .join(format!("tbg-restoremap-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn record_take_roundtrip_and_hwnd_reuse_guard() {
        let dir = test_dir("roundtrip");
        let mut m = RestoreMap::load(&dir).unwrap();
        assert!(m.is_empty());
        m.record(0x20176, "TBG.Group.work", "Microsoft.Notepad");
        m.record(0x30148, "TBG.Group.work", "");
        // take 校验共享值一致（防 HWND 复用误还原）
        assert_eq!(m.take(0x20176, "TBG.Group.work").unwrap(), "Microsoft.Notepad");
        assert_eq!(m.take(0x30148, "TBG.Group.work").unwrap(), ""); // 原空 → clear 语义
        assert_eq!(m.take(0x99999, "TBG.Group.work"), None); // 无条目
        // 已 take 的条目不复存在
        assert_eq!(m.take(0x20176, "TBG.Group.work"), None);
        // 共享值不一致（HWND 被复用成别的组）→ 拒绝
        m.record(0x500AC, "TBG.Group.a", "App.A");
        assert_eq!(m.take(0x500AC, "TBG.Group.b"), None);
        assert_eq!(m.take(0x500AC, "TBG.Group.a").unwrap(), "App.A");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn peek_does_not_remove() {
        let dir = test_dir("peek");
        let mut m = RestoreMap::load(&dir).unwrap();
        m.record(0x123, "TBG.Group.work", "Orig");
        assert_eq!(m.peek(0x123, "TBG.Group.work").unwrap(), "Orig");
        assert_eq!(m.len(), 1); // peek 不移除
        assert_eq!(m.peek(0x123, "TBG.Group.other"), None);
        assert_eq!(m.peek(0x456, "TBG.Group.work"), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_load_roundtrip_with_header() {
        let dir = test_dir("roundtrip-file");
        {
            let mut m = RestoreMap::load(&dir).unwrap();
            m.record(0xAB, "TBG.Group.work", "App.1");
            m.record(0xCD, "TBG.Group.work", ""); // 原空值
            m.save().unwrap();
        }
        // 落盘内容含表头 + 两行 TSV；无 tmp 残留
        let text = fs::read_to_string(dir.join(MAP_FILE_NAME)).unwrap();
        assert!(text.starts_with(MAP_HEADER));
        assert!(text.contains("AB\tTBG.Group.work\tApp.1\n"));
        assert!(text.contains("CD\tTBG.Group.work\t\n"));
        assert!(!dir.join(format!("{MAP_FILE_NAME}.tmp")).exists());
        // 重新载入：条目完整
        let m2 = RestoreMap::load(&dir).unwrap();
        assert_eq!(m2.len(), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_save_removes_file() {
        let dir = test_dir("empty-save");
        {
            let mut m = RestoreMap::load(&dir).unwrap();
            m.record(0x1, "TBG.Group.work", "x");
            m.save().unwrap();
        }
        assert!(dir.join(MAP_FILE_NAME).exists());
        let mut m = RestoreMap::load(&dir).unwrap();
        assert!(m.remove(0x1));
        m.save().unwrap(); // 表空 → 删文件
        assert!(!dir.join(MAP_FILE_NAME).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_file_rejected_fail_safe() {
        let dir = test_dir("corrupt");
        fs::write(dir.join(MAP_FILE_NAME), "not-tsv\n").unwrap();
        // 损坏表拒载（fail-safe，不静默吞记录）
        assert!(RestoreMap::load(&dir).is_err());
        // 半行残文（原子写修复前的典型损伤形态）同样拒载
        fs::write(dir.join(MAP_FILE_NAME), "# tbg-restore.tsv v1\n20176\tTBG.Group").unwrap();
        assert!(RestoreMap::load(&dir).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn v0_legacy_headerless_table_still_parses() {
        let dir = test_dir("v0");
        fs::write(dir.join(MAP_FILE_NAME), "20176\tTBG.Group.work\tApp.1\n").unwrap();
        let m = RestoreMap::load(&dir).unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!(m.peek(0x20176, "TBG.Group.work").unwrap(), "App.1");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn bad_hwnd_rejected() {
        let dir = test_dir("badhwnd");
        fs::write(dir.join(MAP_FILE_NAME), "ZZZZ\tTBG.Group.work\tApp.1\n").unwrap();
        assert!(RestoreMap::load(&dir).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn status_read_only_three_states() {
        let dir = test_dir("status");
        // 不存在 → Absent（status 绝不创建目录/文件）
        assert!(matches!(status(&dir), MapStatus::Absent));
        assert!(!dir.join(MAP_FILE_NAME).exists());
        // 有条目 → Intact（计条数，无副作用：文件内容不变）
        let mut m = RestoreMap::load(&dir).unwrap();
        m.record(0x11, "TBG.Group.work", "A");
        m.record(0x22, "TBG.Group.work", "B");
        m.save().unwrap();
        let before = fs::read_to_string(dir.join(MAP_FILE_NAME)).unwrap();
        match status(&dir) {
            MapStatus::Intact { entries } => assert_eq!(entries, 2),
            other => panic!("expected Intact, got {other:?}"),
        }
        assert_eq!(fs::read_to_string(dir.join(MAP_FILE_NAME)).unwrap(), before);
        // 空表文件（仅表头）→ Intact { entries: 0 }
        fs::write(dir.join(MAP_FILE_NAME), MAP_HEADER).unwrap();
        match status(&dir) {
            MapStatus::Intact { entries } => assert_eq!(entries, 0),
            other => panic!("expected Intact(0), got {other:?}"),
        }
        // 损坏 → Corrupt（不 panic、不删文件）
        fs::write(dir.join(MAP_FILE_NAME), "garbage-not-tsv").unwrap();
        assert!(matches!(status(&dir), MapStatus::Corrupt(_)));
        assert!(dir.join(MAP_FILE_NAME).exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
