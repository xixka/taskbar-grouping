//! AppUserModelID（AUMID）读写：`SHGetPropertyStoreForWindow` +
//! `PKEY_AppUserModel_ID`（docs/plan.md §4 路线 B+ 的核心公开 API，
//! 任务 5 引入）。

use windows::core::{Interface, PCWSTR, Result};
use windows::Win32::Foundation::{E_POINTER, HWND};
use windows::Win32::System::Com::{CoTaskMemFree, PROPVARIANT};
use windows::Win32::UI::Shell::PropertiesSystem::{
    InitPropVariantFromString, PKEY_AppUserModel_ID, PropVariantClear, PropVariantToStringAlloc,
};
use windows::Win32::UI::Shell::{IPropertyStore, SHGetPropertyStoreForWindow};

use crate::winutil::{pwstr_to_string, to_wide};

/// 本工具在原始 AUMID 之后追加的每窗口后缀标记（取消分组方案）。
/// 对应 docs/plan.md §2.1 的 `~Wh~w<HWND>` 思路，此处用自有标记，
/// 后缀形如 `~TBG~w<HWND的大写十六进制>`。
pub(crate) const SUFFIX_MARKER: &str = "~TBG~w";

unsafe fn prop_store(hwnd: HWND) -> Result<IPropertyStore> {
    let mut store: Option<IPropertyStore> = None;
    SHGetPropertyStoreForWindow(
        hwnd,
        &IPropertyStore::IID,
        &mut store as *mut _ as *mut core::ffi::c_void,
    )?;
    store.ok_or_else(|| windows::core::Error::from(E_POINTER))
}

/// 读取窗口当前 AppUserModelID；无 AUMID（VT_EMPTY 等）返回空串。
pub(crate) unsafe fn get_aumid(hwnd: HWND) -> Result<String> {
    let store = prop_store(hwnd)?;
    let mut pv = PROPVARIANT::default();
    store.GetValue(&PKEY_AppUserModel_ID, &mut pv)?;
    let s = match PropVariantToStringAlloc(&pv) {
        Ok(pw) => {
            let s = pwstr_to_string(pw);
            CoTaskMemFree(Some(pw.0 as *const core::ffi::c_void));
            s
        }
        Err(_) => String::new(),
    };
    let _ = PropVariantClear(&mut pv);
    Ok(s)
}

/// 写入窗口 AppUserModelID。
pub(crate) unsafe fn set_aumid(hwnd: HWND, value: &str) -> Result<()> {
    let store = prop_store(hwnd)?;
    let wide = to_wide(value);
    let mut pv = PROPVARIANT::default();
    InitPropVariantFromString(PCWSTR::from_raw(wide.as_ptr()), &mut pv)?;
    let r = match store.SetValue(&PKEY_AppUserModel_ID, &pv) {
        Ok(()) => store.Commit(),
        Err(e) => Err(e),
    };
    let _ = PropVariantClear(&mut pv);
    r
}
