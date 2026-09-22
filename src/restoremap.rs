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
fn atomic_write(path: &Path, content: &str) -> Result<(), String> {
    let tmp = tmp_path(path);
    let mut f = File::create(&tmp)
        .map_err(|e| format!("restore map tmp create failed ({}): {e}", tmp.display()))?;
    f.write_all(content.as_bytes())
        .map_err(|e| format!("restore map tmp write failed: {e}"))?;
    // 落盘完成后再替换主文件，崩溃窗口期内主表保持上一个完整版本
    f.sync_all()
        .map_err(|e| format!("restore map tmp sync failed: {e}"))?;
    drop(f);
    fs::rename(&tmp, path)
        .map_err(|e| format!("restore map rename failed ({} -> {}): {e}", tmp.display(), path.display()))
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
