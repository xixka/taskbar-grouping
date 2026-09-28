//! 任务 35：资源编译（注入版宿主 exe）——任务 32 同款自管 RC 方案。
//!
//! winresource 仅定位 rc.exe 并挂接 .res；RC 传绝对路径，内容自管
//! （见 `tbg-inject.rc` 头注）。非 Windows 目标直接返回。

use std::env;
use std::path::Path;

use winresource::WindowsResource;

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() != "windows" {
        return;
    }

    let rc_path =
        Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap_or_default()).join("tbg-inject.rc");
    let rc_path = rc_path.to_str().unwrap_or("tbg-inject.rc");

    let mut res = WindowsResource::new();
    res.set_resource_file(rc_path);

    if let Err(e) = res.compile() {
        panic!("任务 35：编译资源脚本 tbg-inject.rc 失败: {e}");
    }
}
