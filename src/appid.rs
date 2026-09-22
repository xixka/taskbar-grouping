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

/// 线路二（自定义分组，任务 8）的共享 AUMID 前缀。
/// 全部候选窗口被改写为 `TBG.Group.<name>`，任务栏据此把不同来源的
/// 窗口归入同一按钮（docs/plan.md §4 路线 B+ 的"共享前缀"分支）。
pub(crate) const GROUP_PREFIX: &str = "TBG.Group.";

/// 校验组名并构造线路二的共享 AUMID：`TBG.Group.<name>`。
/// 组名限 1..=32 个字符，字符集 [A-Za-z0-9._-]（与 AUMID 习惯一致，
/// 排除空白与控制字符）；总长恒不超过 11 + 32 = 43 < `AUMID_MAX_LEN`。
/// 组名不合法时返回 Err（CLI 层据此报错，不做静默修正）。
/// 注意：本文件顶部导入了 windows::core::Result（单泛型别名），
/// 此处需要 std 的双泛型 Result，故用全路径显式限定。
pub(crate) fn group_aumid(name: &str) -> std::result::Result<String, String> {
    let n = name.chars().count();
    let charset_ok = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if !(1..=32).contains(&n) || !charset_ok {
        return Err(format!(
            "invalid group name '{name}' (allowed: 1-32 chars of A-Z a-z 0-9 . _ -)"
        ));
    }
    Ok(format!("{GROUP_PREFIX}{name}"))
}

/// 是否本工具写入的共享分组 AUMID（线路二标记）。
/// 线路一（每窗口后缀）与线路二的标记互斥：watch 改写前据此避免叠加。
pub(crate) fn is_group_aumid(aumid: &str) -> bool {
    match aumid.strip_prefix(GROUP_PREFIX) {
        Some(rest) => !rest.is_empty(),
        None => false,
    }
}

/// 校验用户经 `set --value` 直接写入的 AUMID 值（任务 23，审计 BUG-07/
/// SEC-05）：非空、≤129 个 UTF-16 码元（Windows 上限，按码元计）、不含
/// 控制字符（`\t`/`\r`/`\n` 及其他 <0x20 字符——线路二还原表是 TSV，
/// 分隔符混入会破坏整表）。与 `group_aumid` 的严格白名单不同，这里保持
/// 值本身自由（调试用途），只拦破坏性输入。
pub(crate) fn validate_aumid_value(v: &str) -> Result<(), String> {
    let units = v.encode_utf16().count();
    if units == 0 {
        return Err("value must not be empty".to_string());
    }
    if units > AUMID_MAX_LEN {
        return Err(format!(
            "value too long ({units} UTF-16 units > limit {AUMID_MAX_LEN})"
        ));
    }
    if v.chars().any(|c| (c as u32) < 0x20) {
        return Err(
            "value must not contain control characters (tab / newline / others < U+0020)".to_string(),
        );
    }
    Ok(())
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
/// 长度按 UTF-16 码元计（任务 23，审计 BUG-06：Windows 的 129 上限以
/// 码元为单位；增补平面字符 1 char = 2 码元，按 chars 计会超限）。
pub(crate) fn suffixed_aumid(original: &str, hwnd: HWND) -> SuffixedAumid {
    let hex = winutil::hwnd_hex(hwnd);
    let keep = AUMID_MAX_LEN.saturating_sub(SUFFIX_MARKER.len() + hex.len());
    let units: Vec<u16> = original.encode_utf16().collect();
    let truncated = units.len() > keep;
    let head: String = if truncated {
        // 按码元截断；若截在代理对中间，from_utf16_lossy 会把半个代理
        // 替换为 U+FFFD（截断本就是极端场景，保后缀完整优先）
        String::from_utf16_lossy(&units[..keep])
    } else {
        original.to_string()
    };
    SuffixedAumid {
        value: format!("{head}{SUFFIX_MARKER}{hex}"),
        truncated,
    }
}

/// 解析携带本工具后缀的 AUMID（任务 7）：返回 `Some(原始部分)`。
/// 仅当标记之后是合法的大写十六进制 HWND 尾巴（1–16 位）**且与当前窗口
/// 的 HWND 一致**时才认定为本工具所写（任务 23，审计 BUG-05：本工具写入
/// 时永远使用目标窗口自己的 HWND，加一致性校验后，原生 AUMID 恰含
/// "标记+合法 hex"形态的假阳性无法通过——除非它恰好等于当前窗口 HWND，
/// 概率可忽略），防止误剥其他来源的相似字符串；原始部分为空串表示"原本无
/// AUMID，还原时应清除属性"（配合 `clear_aumid`）。
pub(crate) fn strip_suffix(aumid: &str, hwnd: HWND) -> Option<&str> {
    let pos = aumid.rfind(SUFFIX_MARKER)?;
    let (head, tail) = aumid.split_at(pos);
    let hex = &tail[SUFFIX_MARKER.len()..];
    let hex_ok = (1..=16).contains(&hex.len())
        && hex.bytes().all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b));
    if hex_ok && hex == winutil::hwnd_hex(hwnd) {
        Some(head)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hwnd(v: usize) -> HWND {
        HWND(v as *mut core::ffi::c_void)
    }

    #[test]
    fn group_aumid_accepts_valid_names() {
        assert_eq!(group_aumid("work").unwrap(), "TBG.Group.work");
        assert_eq!(group_aumid("a.b_c-9").unwrap(), "TBG.Group.a.b_c-9");
        assert_eq!(group_aumid(&"x".repeat(32)).unwrap(), format!("TBG.Group.{}", "x".repeat(32)));
    }

    #[test]
    fn group_aumid_rejects_bad_names() {
        assert!(group_aumid("").is_err()); // 空
        assert!(group_aumid(&"x".repeat(33)).is_err()); // 超长
        assert!(group_aumid("has space").is_err());
        assert!(group_aumid("中文").is_err()); // 非 ASCII
        assert!(group_aumid("tab\there").is_err());
    }

    #[test]
    fn is_group_aumid_basic() {
        assert!(is_group_aumid("TBG.Group.work"));
        assert!(!is_group_aumid("TBG.Group.")); // 前缀后为空不算
        assert!(!is_group_aumid("Microsoft.Notepad"));
        assert!(!is_group_aumid(""));
    }

    #[test]
    fn suffixed_and_strip_roundtrip() {
        let h = hwnd(0x20176);
        let s = suffixed_aumid("Microsoft.Notepad", h);
        assert!(!s.truncated);
        assert_eq!(s.value, "Microsoft.Notepad~TBG~w20176");
        // 剥离必须针对同一窗口（审计 BUG-05）
        assert_eq!(strip_suffix(&s.value, h), Some("Microsoft.Notepad"));
        // 同形态但 HWND 不同 → 拒绝剥离
        assert_eq!(strip_suffix(&s.value, hwnd(0x99999)), None);
    }

    #[test]
    fn strip_suffix_rejects_fake_markers() {
        let h = hwnd(0xABC);
        // 标记后非十六进制（小写 g）
        assert_eq!(strip_suffix("app~TBG~wg00", h), None);
        // 标记后空
        assert_eq!(strip_suffix("app~TBG~w", h), None);
        // 超长 hex（17 位）
        let long = format!("app~TBG~w{}F", "0".repeat(16));
        assert_eq!(strip_suffix(&long, h), None);
        // 无标记
        assert_eq!(strip_suffix("Microsoft.Notepad", h), None);
        // 合法 hex 但与窗口 HWND 不一致（审计 BUG-05 假阳性场景）
        assert_eq!(strip_suffix("app~TBG~wABD", h), None);
    }

    #[test]
    fn strip_suffix_empty_original_means_clear() {
        let h = hwnd(0x5AC);
        let s = suffixed_aumid("", h);
        assert_eq!(s.value, "~TBG~w5AC");
        assert_eq!(strip_suffix(&s.value, h), Some(""));
    }

    #[test]
    fn suffixed_aumid_truncates_by_utf16_units() {
        // 审计 BUG-06：按 UTF-16 码元计而非字符（emoji 1 char = 2 码元）
        let h = hwnd(0xFF);
        // 4 个 emoji = 8 码元 + 后缀 5+2=7 → keep = 129-7 = 122 不截断
        let emoji = "\u{1F600}".repeat(4);
        assert_eq!(emoji.encode_utf16().count(), 8);
        let s = suffixed_aumid(&emoji, h);
        assert!(!s.truncated);
        // 超长输入按码元截断
        let long = "a".repeat(200);
        let s2 = suffixed_aumid(&long, h);
        assert!(s2.truncated);
        let keep = AUMID_MAX_LEN - SUFFIX_MARKER.len() - 2; // hex "FF" 长度 2
        assert_eq!(s2.value.encode_utf16().count(), AUMID_MAX_LEN);
        assert!(s2.value.ends_with("~TBG~wFF"));
        assert_eq!(&s2.value[..keep], "a".repeat(keep));
    }

    #[test]
    fn validate_aumid_value_rules() {
        assert!(validate_aumid_value("Microsoft.Notepad").is_ok());
        assert!(validate_aumid_value("").is_err()); // 空
        assert!(validate_aumid_value("a\tb").is_err()); // 制表
        assert!(validate_aumid_value("a\nb").is_err()); // 换行
        assert!(validate_aumid_value("a\u{1}b").is_err()); // 控制字符
        assert!(validate_aumid_value(&"x".repeat(129)).is_ok()); // 恰好 129 码元
        assert!(validate_aumid_value(&"x".repeat(130)).is_err()); // 超长
        // 审计 BUG-06：增补平面字符按 2 码元计
        assert!(validate_aumid_value(&"\u{1F600}".repeat(65)).is_err()); // 130 码元
        assert!(validate_aumid_value(&"\u{1F600}".repeat(64)).is_ok()); // 128 码元
    }
}
