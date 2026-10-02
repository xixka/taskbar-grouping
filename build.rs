//! 任务 32：杀软误报治理（构建侧）——为产物嵌入 VERSIONINFO 与应用清单。
//!
//! 背景：发布物是"单文件 + strip + 未签名"的小体积 exe，行为面又天然贴近
//! 启发式引擎的关注区：改写其他进程窗口的 AUMID 属性、全局 SetWinEventHook
//! 监听、HKCU Run 自启注册、任务栏磁贴固定（全部是公开文档 API，见
//! README）。Kaspersky 等引擎对"无版本信息 + 无清单 + 无签名"的小 exe 会
//! 叠加启发式评分，产生非特征性的 generic 误报（UDS / PDM / Heur 一类）。
//!
//! 本脚本给产物补齐"可核验的产品身份"，纯资源注入、零运行时行为变化：
//! - VERSIONINFO：版本 / 描述 / 版权 / 项目主页（`tbg-lite.rc`，资源管理器
//!   "属性→详细信息"与任务管理器可见）；
//! - 应用清单（`tbg-lite.exe.manifest`）：requestedExecutionLevel =
//!   asInvoker（显式声明，与 README "no admin rights" 口径一致）+
//!   Windows 10/11 supportedOS。
//!
//! 实现路线（两轮 CI 实测教训，run 36372461818 / 36373209674）：
//! winresource 自动生成的 RC（强制 `#pragma code_page(65001)`，清单走
//! 转义路径或字符串块）在两轮中均使加载器报 SxS 14001（side-by-side
//! configuration is incorrect，二进制无法启动）。终案对齐 alacritty 的
//! 成熟方案：`set_resource_file` 整体自管 RC——winresource 只负责定位
//! Windows SDK 的 rc.exe、传 `/I` 与 `/fo`、再以 `cargo:rustc-link-arg`
//! 挂接产物 .res，RC 内容不经其生成（`tbg-lite.rc` 纯 ASCII、无
//! code-page pragma、相对路径引用清单文件）。
//!
//! 依赖：winresource 走 [build-dependencies]（default-features = false，
//! 关闭 toml 特性，仅 version_check 一个构建依赖；SEC-03 新鲜度门禁要求
//! Cargo.lock 与 Cargo.toml 同笔提交）。MSVC 目标经宿主 Windows SDK 的
//! rc.exe 编译 .res（windows-latest 自带，无需安装）。版本升级时记得同步
//! `tbg-lite.rc` 与 `tbg-lite.exe.manifest` 中的版本号（0.1.0 → 0,1,0,0 /
//! 0.1.0.0）。

use std::env;
use std::path::Path;

use winresource::WindowsResource;

fn main() {
    // 按目标（而非宿主）判断：仅 Windows 目标嵌入资源；交叉编译同样生效，
    // 非 Windows 目标直接返回，不依赖宿主机上的任何 SDK 工具。
    if env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() != "windows" {
        return;
    }

    // RC 传绝对路径（Command 参数原样直达 rc.exe，无 shell/RC 转义环节）；
    // RC 内的清单文件用相对路径，由 rc.exe 的 cwd（= build 脚本 cwd
    // = 包根）解析。
    let rc_path =
        Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap_or_default()).join("tbg-lite.rc");
    let rc_path = rc_path.to_str().unwrap_or("tbg-lite.rc");

    let mut res = WindowsResource::new();
    res.set_resource_file(rc_path);

    if let Err(e) = res.compile() {
        panic!("任务 32：编译资源脚本 tbg-lite.rc 失败: {e}");
    }
}
