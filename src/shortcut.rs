//! 固定磁贴配套（任务 16，docs/plan.md v2 §3 Phase 2）。
//!
//! `pin` 命令的核心：为线路二自定义分组生成带共享 AUMID
//! `TBG.Group.<NAME>` 的 `.lnk` 快捷方式（参考 aumid-stopgap-tools
//! `mklnkwaumid` 的直译）：`CoCreateInstance(CLSID_ShellLink)` →
//! `IShellLinkW::SetPath/SetIconLocation` → QI `IPropertyStore` 写
//! `PKEY_AppUserModel_ID` → `Commit` → QI `IPersistFile::Save`。
//! 磁贴与被 `watch --strategy group` 改写的运行中窗口共享同一 AUMID，
//! 任务栏据此把两者归入同一按钮（真机视觉验收属任务 17/18）。
//!
//! 落盘后**回读自校验**：重新 `Load` 刚保存的 `.lnk` 并读取 AUMID，
//! 与期望值不一致即报错——CI 据输出行判定（任务 18 将用它做
//! ".lnk AUMID == 运行中窗口 AUMID" 的同组断言）。
//!
//! 与还原映射表无关：不触碰 `tbg-restore.tsv`，因此不参与
//! `Local\tbg-lite.map` 单实例互斥（审计 BUG-02 红线仅约束写表路径）。
//! COM 需求：调用方线程须已初始化 COM（`winutil::ComGuard`）。

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use windows::core::{BSTR, Interface, PCWSTR, PROPVARIANT};
use windows::Win32::Foundation::BOOL;
use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
use windows::Win32::System::Com::{
    CoCreateInstance, IPersistFile, CLSCTX_INPROC_SERVER, STGM_READ,
};
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;

/// 默认输出子目录名（数据目录下，`restoremap::data_dir()/pin`）。
pub(crate) const PIN_DIR_NAME: &str = "pin";

/// `create_pin` 的结果（供 CLI 层输出与测试消费）。
pub(crate) struct PinOutcome {
    /// 保存的 `.lnk` 完整路径。
    pub(crate) path: PathBuf,
    /// 保存前该路径是否已存在（重跑 = 覆盖，与 install 同语义）。
    pub(crate) replaced: bool,
    /// 从落盘文件回读出的 AUMID（已验证 == 期望值）。
    pub(crate) aumid: String,
}

/// `--icon` 规格：`<路径>` 或 `<路径>,<索引>`（索引可负：负值是资源 ID，
/// 与 `IShellLinkW::SetIconLocation` 的 iicon 语义一致）。
///
/// 解析规则（宽容式，按最后一个逗号切分）：尾巴能解析为 i32 才视为索引，
/// 否则整个字符串按"纯路径、索引 0"处理——图标路径本身含逗号（如
/// `C:\a,b\icon.dll`）不会误拆；索引拼错（`x.ico,2z`）则退回整串当路径，
/// 由后续的"文件必须存在"校验给出可定位的错误。空路径直接 Err。
pub(crate) fn parse_icon_spec(spec: &str) -> Result<(String, i32), String> {
    let (path, index) = match spec.rsplit_once(',') {
        Some((head, tail)) => match tail.parse::<i32>() {
            Ok(i) => (head, i),
            Err(_) => (spec, 0),
        },
        None => (spec, 0),
    };
    if path.is_empty() {
        return Err("icon path must not be empty".to_string());
    }
    Ok((path.to_string(), index))
}

/// 磁贴文件名：`<组名>.lnk`（组名字符集 [A-Za-z0-9._-] 经 `group_aumid`
/// 校验，天然是合法文件名；文件名 = 磁贴标签）。
pub(crate) fn lnk_file_name(group_name: &str) -> String {
    format!("{group_name}.lnk")
}

/// 输出目录拼接（纯逻辑，供默认路径与测试复用）。
pub(crate) fn pin_dir(base: &Path) -> PathBuf {
    base.join(PIN_DIR_NAME)
}

/// OsStr → NUL 结尾 UTF-16（不丢代理项，比 to_string_lossy 稳）。
fn wide_os(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// 生成带共享 AUMID 的 `.lnk`（mklnkwaumid 直译）。前置条件（调用方
/// `cmd_pin` 已校验）：`target`/`icon_path` 是已存在文件的规范化绝对
/// 路径；`aumid` 是 `group_aumid` 产物；`out_path` 的父目录已创建。
/// 运行时错误（COM/IO）以 `pin: ` 前缀 Err 上抛。
pub(crate) unsafe fn create_pin(
    aumid: &str,
    target: &str,
    arguments: Option<&str>,
    icon: Option<(&str, i32)>,
    out_path: &Path,
) -> Result<PinOutcome, String> {
    let replaced = out_path.exists();

    // 外部聚合参数 None（不聚合）；显式绑定类型，绕开泛型推断歧义
    let outer: Option<&windows::core::IUnknown> = None;
    let link: IShellLinkW = CoCreateInstance(&ShellLink, outer, CLSCTX_INPROC_SERVER)
        .map_err(|e| format!("pin: CoCreateInstance(ShellLink) failed: {e}"))?;

    let w_target = wide_os(OsStr::new(target));
    link.SetPath(PCWSTR::from_raw(w_target.as_ptr()))
        .map_err(|e| format!("pin: SetPath failed: {e}"))?;
    // 描述里带组名，便于用户在属性面板辨认磁贴来源
    let desc = format!("tbg-lite group tile ({aumid})");
    let w_desc = wide_os(OsStr::new(&desc));
    link.SetDescription(PCWSTR::from_raw(w_desc.as_ptr()))
        .map_err(|e| format!("pin: SetDescription failed: {e}"))?;
    if let Some(args) = arguments {
        let w_args = wide_os(OsStr::new(args));
        link.SetArguments(PCWSTR::from_raw(w_args.as_ptr()))
            .map_err(|e| format!("pin: SetArguments failed: {e}"))?;
    }
    if let Some((icon_path, index)) = icon {
        let w_icon = wide_os(OsStr::new(icon_path));
        link.SetIconLocation(PCWSTR::from_raw(w_icon.as_ptr()), index)
            .map_err(|e| format!("pin: SetIconLocation failed: {e}"))?;
    }
    // 未指定 --icon 时不调 SetIconLocation：磁贴默认取目标程序自身图标
    // （计划任务 16 的"默认取组内主程序"语义）

    // AUMID 写入快捷方式属性存储（SetValue + Commit 后再 Save，
    // mklnkwaumid 的顺序）
    let store: IPropertyStore = link
        .cast()
        .map_err(|e| format!("pin: ShellLink -> IPropertyStore failed: {e}"))?;
    let pv = PROPVARIANT::from(aumid);
    store
        .SetValue(&PKEY_AppUserModel_ID, &pv)
        .map_err(|e| format!("pin: SetValue(PKEY_AppUserModel_ID) failed: {e}"))?;
    store
        .Commit()
        .map_err(|e| format!("pin: property store Commit failed: {e}"))?;

    let persist: IPersistFile = link
        .cast()
        .map_err(|e| format!("pin: ShellLink -> IPersistFile failed: {e}"))?;
    let w_out = wide_os(out_path.as_os_str());
    persist
        .Save(
            PCWSTR::from_raw(w_out.as_ptr()),
            BOOL(1), // fRemember = TRUE
        )
        .map_err(|e| format!("pin: Save ({}) failed: {e}", out_path.display()))?;

    // 落盘回读自校验：独立 Load + GetValue，验证的是文件内容而非内存态
    let readback = read_lnk_aumid(out_path)?;
    if readback != aumid {
        return Err(format!(
            "pin: AUMID verification failed: expected '{aumid}', file says '{readback}' ({})",
            out_path.display()
        ));
    }
    Ok(PinOutcome {
        path: out_path.to_path_buf(),
        replaced,
        aumid: readback,
    })
}

/// 读取 `.lnk` 内嵌的 AppUserModelID（无 AUMID 返回空串，与窗口读取
/// 口径一致）。独立于 `create_pin` 的代码路径：全新 ShellLink 实例 +
/// `IPersistFile::Load` + `IPropertyStore::GetValue`。前置条件：调用方
/// 线程已初始化 COM。任务 18 的 CI 同组断言将复用此读路径。
pub(crate) fn read_lnk_aumid(path: &Path) -> Result<String, String> {
    unsafe {
        let outer: Option<&windows::core::IUnknown> = None;
        let link: IShellLinkW = CoCreateInstance(&ShellLink, outer, CLSCTX_INPROC_SERVER)
            .map_err(|e| format!("pin: CoCreateInstance(ShellLink) failed: {e}"))?;
        let persist: IPersistFile = link
            .cast()
            .map_err(|e| format!("pin: ShellLink -> IPersistFile failed: {e}"))?;
        let w_path = wide_os(path.as_os_str());
        persist
            .Load(PCWSTR::from_raw(w_path.as_ptr()), STGM_READ)
            .map_err(|e| format!("pin: Load ({}) failed: {e}", path.display()))?;
        let store: IPropertyStore = link
            .cast()
            .map_err(|e| format!("pin: ShellLink -> IPropertyStore failed: {e}"))?;
        let pv = store
            .GetValue(&PKEY_AppUserModel_ID)
            .map_err(|e| format!("pin: GetValue(PKEY_AppUserModel_ID) failed: {e}"))?;
        Ok(BSTR::try_from(&pv)
            .map(|b| b.to_string())
            .unwrap_or_default())
    }
}

// 供 future CI 断言引用的说明：本模块所有 Windows API 调用均为公开文档
// 接口（Shell COM），与项目零注入红线一致（AGENTS.md）。

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_spec_plain_path_defaults_index_zero() {
        assert_eq!(
            parse_icon_spec(r"C:\icons\a.ico").unwrap(),
            (r"C:\icons\a.ico".to_string(), 0)
        );
        // 相对路径同样接受（存在性校验归 cmd_pin）
        assert_eq!(parse_icon_spec("a.ico").unwrap(), ("a.ico".to_string(), 0));
    }

    #[test]
    fn icon_spec_explicit_index() {
        assert_eq!(
            parse_icon_spec(r"C:\icons\a.ico,2").unwrap(),
            (r"C:\icons\a.ico".to_string(), 2)
        );
        // 负索引 = 资源 ID（SetIconLocation iicon 语义）
        assert_eq!(
            parse_icon_spec(r"C:\icons\a.ico,-3").unwrap(),
            (r"C:\icons\a.ico".to_string(), -3)
        );
    }

    #[test]
    fn icon_spec_comma_inside_path_stays_whole() {
        // 路径含逗号且尾巴非数字 → 整串当路径、索引 0（宽容式不误拆）
        assert_eq!(
            parse_icon_spec(r"C:\a,b\icon.dll").unwrap(),
            (r"C:\a,b\icon.dll".to_string(), 0)
        );
        // 尾巴拼错（2z）同样退回整串：交给文件存在性校验报可定位的错
        assert_eq!(
            parse_icon_spec(r"C:\icons\a.ico,2z").unwrap(),
            (r"C:\icons\a.ico,2z".to_string(), 0)
        );
    }

    #[test]
    fn icon_spec_empty_rejected() {
        assert!(parse_icon_spec("").is_err());
        // 逗号开头：head 为空路径 → 拒绝（尾巴 "2" 虽是合法索引也不补空路径）
        assert!(parse_icon_spec(",2").is_err());
    }

    #[test]
    fn lnk_file_name_and_pin_dir() {
        assert_eq!(lnk_file_name("work"), "work.lnk");
        assert_eq!(lnk_file_name("a.b_c-9"), "a.b_c-9.lnk");
        // 组名字符集 [A-Za-z0-9._-] 经 group_aumid 校验，无路径分隔符风险
        assert_eq!(
            pin_dir(Path::new(r"C:\Users\x\AppData\Local\tbg-lite")),
            PathBuf::from(r"C:\Users\x\AppData\Local\tbg-lite\pin")
        );
    }
}
