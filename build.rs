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
//! - 应用清单（`tbg-lite.exe.manifest`，仓库根）：requestedExecutionLevel =
//!   asInvoker（显式声明，与 README "no admin rights" 口径一致）+
//!   Windows 10/11 supportedOS。
//!
//! 实现注意（run 36372461818 实测教训）：清单必须走 set_manifest_file
//! 文件直嵌（RC 语句 `1 24 "<file>"`，rc.exe 对文件内容字节原样拷贝）。
//! set_manifest 的 RC 字符串块路线经 CI 实测会损坏清单，加载器报
//! SxS 14001（side-by-side configuration is incorrect），二进制直接无法
//! 启动——弃用。
//!
//! 依赖：winresource 走 [build-dependencies]（default-features = false，
//! 关闭 toml 特性，仅 version_check 一个构建依赖；SEC-03 新鲜度门禁要求
//! Cargo.lock 与 Cargo.toml 同笔提交）。MSVC 目标经宿主 Windows SDK 的
//! rc.exe 编译 .res（windows-latest 自带，无需额外安装）。版本升级时记得
//! 同步清单文件里的 assemblyIdentity version（四段式：0.1.0 → 0.1.0.0）。

use std::env;
use std::path::Path;

use winresource::WindowsResource;

fn main() {
    // 按目标（而非宿主）判断：仅 Windows 目标嵌入资源；交叉编译同样生效，
    // 非 Windows 目标直接返回，不依赖宿主机上的任何 SDK 工具。
    if env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() != "windows" {
        return;
    }

    // 清单文件绝对路径（cargo 下 build 脚本 cwd 即包根，非 cargo 环境退化为
    // 相对路径同样成立）；rc.exe 对文件资源不做任何编码/转义转换。
    let manifest_path = Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap_or_default())
        .join("tbg-lite.exe.manifest");
    let manifest_path = manifest_path.to_str().unwrap_or("tbg-lite.exe.manifest");

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
        .set_manifest_file(manifest_path);

    if let Err(e) = res.compile() {
        panic!("任务 32：嵌入 VERSIONINFO / 应用清单失败: {e}");
    }
}
