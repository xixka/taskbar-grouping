//! AppUserModelID（AUMID）读写：`SHGetPropertyStoreForWindow` +
//! `PKEY_AppUserModel_ID`（docs/plan.md §4 路线 B+ 的核心公开 API，
//! 任务 5 引入）。

use windows::core::{BSTR, PROPVARIANT, Result};
use windows::Win32::Foundation::HWND;
use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};

use crate::winutil;

/// 本工具在原始 AUMID 之后追加的每窗口后缀标记（取消分组方案）。
/// 对应 docs/plan.md §2.1 的 `~Wh~w<HWND>` 思路，此处用自有标记，
/// 后缀形如 `~TBG~w<HWND的大写十六进制>`。
pub(crate) const SUFFIX_MARKER: &str = "~TBG~w";

/// AUMID 字符串长度上限（Windows 文档要求 AppUserModelID 不超过 129 字符；
/// 以真机实测为准）。
pub(crate) const AUMID_MAX_LEN: usize = 129;

unsafe fn prop_store(hwnd: HWND) -> Result<IPropertyStore> {
    // windows 0.58 提供泛型封装：按返回类型直接取类型化接口
    SHGetPropertyStoreForWindow(hwnd)
}

/// 读取窗口当前 AppUserModelID；无 AUMID（VT_EMPTY 等无法转字符串）返回空串。
pub(crate) unsafe fn get_aumid(hwnd: HWND) -> Result<String> {
    let store = prop_store(hwnd)?;
    let pv = store.GetValue(&PKEY_AppUserModel_ID)?;
    Ok(BSTR::try_from(&pv).map(|b| b.to_string()).unwrap_or_default())
}

/// 写入窗口 AppUserModelID。
///
/// 以 VT_BSTR 写入，属性存储（property store）提交时会按 PKEY 的
/// 字符串类型规范化；`PROPVARIANT` 析构时自动 `PropVariantClear`，
/// 调用方无需手工释放。
pub(crate) unsafe fn set_aumid(hwnd: HWND, value: &str) -> Result<()> {
    let store = prop_store(hwnd)?;
    let pv = PROPVARIANT::from(value);
    store.SetValue(&PKEY_AppUserModel_ID, &pv)?;
    store.Commit()
}

/// 清除窗口 AppUserModelID（写 VT_EMPTY，任务 7：还原"原本无 AUMID"的
/// 窗口时使用）。零值 PROPVARIANT 即 VT_EMPTY。
pub(crate) unsafe fn clear_aumid(hwnd: HWND) -> Result<()> {
    let store = prop_store(hwnd)?;
    let pv = PROPVARIANT::new();
    store.SetValue(&PKEY_AppUserModel_ID, &pv)?;
    store.Commit()
}

/// `suffixed_aumid` 的返回值。
pub(crate) struct SuffixedAumid {
    /// 追加后缀后的完整值。
    pub(crate) value: String,
    /// 原 AUMID 是否因超长被截断。
    pub(crate) truncated: bool,
}

/// 构造“每窗口去分组”值：`original + SUFFIX_MARKER + <HWND大写十六进制>`
/// （任务 6：事件驱动自动改写与 `set --suffix` 共用此策略）。
/// 前置条件：`original` 不含 `SUFFIX_MARKER`（由调用方保证）。
/// 总长超过 `AUMID_MAX_LEN` 时截断原值头部，保证后缀完整（值仍唯一）；
/// 此时会返回 `truncated: true` 供调用方记录。
pub(crate) fn suffixed_aumid(original: &str, hwnd: HWND) -> SuffixedAumid {
    let hex = winutil::hwnd_hex(hwnd);
    let keep = AUMID_MAX_LEN.saturating_sub(SUFFIX_MARKER.len() + hex.len());
    let truncated = original.chars().count() > keep;
    let head: String = if truncated {
        // 按字符截断，避免切在多字节字符中间
        original.chars().take(keep).collect()
    } else {
        original.to_string()
    };
    SuffixedAumid {
        value: format!("{head}{SUFFIX_MARKER}{hex}"),
        truncated,
    }
}

/// 解析携带本工具后缀的 AUMID（任务 7）：返回 `Some(原始部分)`。
/// 仅当标记之后是合法的大写十六进制 HWND 尾巴（1–16 位）时才认定为本
/// 工具所写，防止误剥其他来源的相似字符串；原始部分为空串表示"原本无
/// AUMID，还原时应清除属性"（配合 `clear_aumid`）。
pub(crate) fn strip_suffix(aumid: &str) -> Option<&str> {
    let pos = aumid.rfind(SUFFIX_MARKER)?;
    let (head, tail) = aumid.split_at(pos);
    let hex = &tail[SUFFIX_MARKER.len()..];
    let hex_ok = (1..=16).contains(&hex.len())
        && hex.bytes().all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b));
    if hex_ok {
        Some(head)
    } else {
        None
    }
}
