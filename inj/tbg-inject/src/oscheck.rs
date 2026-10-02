//! 支持范围提示（§0-7，2026-10-01 维护者指令：仅支持 Win11）。
//!
//! 与主版 `src/oscheck.rs` 同源设计：`windows-version`（Microsoft
//! windows-rs 官方小 crate，内部 `ntdll!RtlGetVersion`，不受清单
//! "版本骗报"影响）读真实 build；低于 22000 → stderr 单行警告后
//! 照常执行——**仅提示、不阻断**（注入版在 Win10 上同样受限于
//! 结构性限制⑤，范围决议是不再测试与承诺）。CI 全腿在
//! windows-latest（build ≥ 26200）上运行，警告不进门禁日志。

use windows_version::OsVersion;

/// Windows 11 最小 build 号（21H2 = 22000）。
pub(crate) const WIN11_MIN_BUILD: u32 = 22000;

/// 纯分类函数（单测锚点）：探测到的 build 是否在支持范围外。
pub(crate) fn out_of_scope(build: Option<u32>) -> bool {
    matches!(build, Some(b) if b < WIN11_MIN_BUILD)
}

/// 启动提示：范围外时向 stderr 打一行警告（菜单/CLI 全路径共用）。
pub(crate) fn win11_only_notice() {
    let build = OsVersion::current().build;
    if out_of_scope(Some(build)) {
        eprintln!(
            "tbg-inject: warning: Windows 11 is the only supported platform \
             (detected OS build {build}); Windows 10 is not supported or \
             tested — please use an established tool there instead"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundary_builds() {
        assert!(out_of_scope(Some(10240))); // Win10 1507
        assert!(out_of_scope(Some(19045))); // Win10 22H2
        assert!(out_of_scope(Some(20348))); // Server 2022（旧经典腿环境）
        assert!(out_of_scope(Some(21999)));
        assert!(!out_of_scope(Some(WIN11_MIN_BUILD))); // Win11 21H2
        assert!(!out_of_scope(Some(26200))); // Server 2025（CI）
        assert!(!out_of_scope(None)); // 探测失败不误报
    }
}
