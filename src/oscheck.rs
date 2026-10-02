//! 支持范围提示（§0-7，2026-10-01 维护者指令：仅支持 Win11）。
//!
//! 经 `windows-version`（Microsoft windows-rs 官方小 crate，内部走
//! `ntdll!RtlGetVersion`，不受应用清单 supportedOS"版本骗报"机制影响）
//! 读取真实 OS build；低于 22000（Windows 11 21H2 阈值）→ stderr 单行
//! 警告后照常执行。**仅提示、不阻断、不改退出码**：范围决议是不再
//! 测试与承诺 Win10（Win10 用户用其他成熟工具），而非故意破坏——
//! tbg-lite 的文档化 API 路线在 Win10 上事实上仍工作。CI 全部腿跑在
//! windows-latest（Server 2025，build ≥ 26200），该警告不会出现在任何
//! 门禁日志里；`inspect --json` 等机器可读输出走 stdout，不受 stderr
//! 警告影响。

use windows_version::OsVersion;

/// Windows 11 最小 build 号（21H2 = 22000；Win10 全系 ≤ 19045，
/// Server 2022 = 20348 同样范围外）。
pub(crate) const WIN11_MIN_BUILD: u32 = 22000;

/// 纯分类函数（单测锚点）：探测到的 build 是否在支持范围外。
/// `None`（探测失败）按范围内处理——宁可漏报不可误报。
pub(crate) fn out_of_scope(build: Option<u32>) -> bool {
    matches!(build, Some(b) if b < WIN11_MIN_BUILD)
}

/// 启动提示：范围外（build < 22000）时向 stderr 打一行警告。
/// 每次进程启动早期调用一次；在范围内零输出、零开销分支。
pub(crate) fn win11_only_notice() {
    let build = OsVersion::current().build;
    if out_of_scope(Some(build)) {
        eprintln!(
            "tbg-lite: warning: Windows 11 is the only supported platform \
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
        // Win10 全系（1507 … 22H2）与旧经典腿环境（Server 2022）均在范围外
        assert!(out_of_scope(Some(10240)));
        assert!(out_of_scope(Some(19045)));
        assert!(out_of_scope(Some(20348)));
        assert!(out_of_scope(Some(21999)));
        // Win11 21H2 起在范围内（含 CI 的 Server 2025）
        assert!(!out_of_scope(Some(WIN11_MIN_BUILD)));
        assert!(!out_of_scope(Some(22631)));
        assert!(!out_of_scope(Some(26200)));
        // 探测失败 → 不警告（宁可漏报不可误报）
        assert!(!out_of_scope(None));
    }
}
