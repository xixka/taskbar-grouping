//! 任务 32：杀软误报治理（构建侧）——为产物嵌入 VERSIONINFO 与应用清单。
//!
//! 背景：发布物是"单文件 + strip + 未签名"的小体积 exe，行为面又天然贴近
//! 启发式引擎的关注区：改写其他进程窗口的 AUMID 属性、全局 SetWinEventHook
//! 监听、HKCU Run 自启注册、任务栏磁贴固定（全部是公开文档 API，见
//! README）。Kaspersky 等引擎对"无版本信息 + 无清单 + 无签名"的小 exe 会
//! 叠加启发式评分，产生非特征性的 generic 误报（UDS / PDM / Heur 一类）。
//!
//! 本脚本给产物补齐"可核验的产品身份"，纯资源注入、零运行时行为变化：
//! - VERSIONINFO：Product/File 版本（取自 CARGO_PKG_VERSION）、文件描述、
//!   版权、项目主页——资源管理器"属性→详细信息"与任务管理器可见；
//! - 应用清单：requestedExecutionLevel = asInvoker（显式声明，与 README
//!   "no admin rights" 口径一致）+ Windows 10/11 supportedOS。
//!
//! 依赖：winresource 走 [build-dependencies]（default-features = false，
//! 关闭 toml 特性，仅 version_check 一个构建依赖；SEC-03 新鲜度门禁要求
//! Cargo.lock 与 Cargo.toml 同笔提交）。MSVC 目标经宿主 Windows SDK 的
//! rc.exe 编译 .res（windows-latest 自带，无需额外安装）。版本升级时记得
//! 同步下方清单里的 assemblyIdentity version（四段式：0.1.0 → 0.1.0.0）。

use std::env;

use winresource::WindowsResource;

/// 应用清单（资源类型 24 = RT_MANIFEST，ID 1 = CREATEPROCESS_MANIFEST_RESOURCE_ID）。
/// assemblyIdentity version 为四段式，跟随 Cargo.toml 的版本号（0.1.0 → 0.1.0.0）。
const APP_MANIFEST: &str = r#"<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="tbg-lite" version="0.1.0.0" processorArchitecture="*"/>
  <description>tbg-lite taskbar grouping controller</description>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Guid="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
</assembly>"#;

fn main() {
    // 按目标（CARGO_CFG_TARGET_OS，而非宿主）判断：仅 Windows 目标嵌入资源，
    // 交叉编译同样生效；非 Windows 目标直接返回，不触碰宿主机的任何 SDK 工具。
    if env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() != "windows" {
        return;
    }

    // FileVersion / ProductVersion / ProductName 由 WindowsResource::new() 取
    // CARGO_PKG_* 环境变量默认填好（与 Cargo.toml 同源），此处只覆盖身份字段。
    let mut res = WindowsResource::new();
    res.set("FileDescription", "tbg-lite - Windows taskbar grouping controller")
        .set("ProductName", "tbg-lite")
        .set("InternalName", "tbg-lite")
        .set("OriginalFilename", "tbg-lite.exe")
        .set("CompanyName", "tbg-lite open-source project")
        .set(
            "LegalCopyright",
            "MIT License - https://github.com/xixka/taskbar-grouping",
        )
        .set(
            "Comments",
            "Taskbar grouping via documented Shell property-store APIs. Source and CI builds: https://github.com/xixka/taskbar-grouping",
        )
        .set_manifest(APP_MANIFEST);

    if let Err(e) = res.compile() {
        panic!("任务 32：嵌入 VERSIONINFO / 应用清单失败: {e}");
    }
}
