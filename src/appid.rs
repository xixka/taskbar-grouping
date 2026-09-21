//! AppUserModelID（AUMID）读写：`SHGetPropertyStoreForWindow` +
//! `PKEY_AppUserModel_ID`（docs/plan.md §4 路线 B+ 的核心公开 API，
//! 任务 5 引入）。

use windows::core::{BSTR, PROPVARIANT, Result};
use windows::Win32::Foundation::HWND;
use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};

/// 本工具在原始 AUMID 之后追加的每窗口后缀标记（取消分组方案）。
/// 对应 docs/plan.md §2.1 的 `~Wh~w<HWND>` 思路，此处用自有标记，
/// 后缀形如 `~TBG~w<HWND的大写十六进制>`。
pub(crate) const SUFFIX_MARKER: &str = "~TBG~w";

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
