//! 任务 34：资源编译（DLL 版）——任务 32 同款自管 RC 方案。
//!
//! winresource 仅定位 rc.exe 并挂接 .res（windows-latest 自带 Windows SDK）；
//! RC 路径传绝对路径，内容完全自管（见 `tbg-hook.rc` 头注）。非 Windows
//! 目标直接返回。

use std::env;
use std::path::Path;

use winresource::WindowsResource;

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() != "windows" {
        return;
    }

    let rc_path = Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap_or_default()).join("tbg-hook.rc");
    let rc_path = rc_path.to_str().unwrap_or("tbg-hook.rc");

    let mut res = WindowsResource::new();
    res.set_resource_file(rc_path);

    if let Err(e) = res.compile() {
        panic!("任务 34：编译资源脚本 tbg-hook.rc 失败: {e}");
    }
}
