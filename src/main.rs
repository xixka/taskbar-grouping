//! tbg-lite — Windows 任务栏分组控制（零注入路线 B+）
//!
//! 任务 0：仓库骨架。实施计划与任务拆分见 `docs/plan.md`。
//! 本阶段只提供 CLI 骨架（`--version` / `--help`）；核心逻辑（WinEventHook
//! 监听、`PKEY_AppUserModel_ID` 改写、`.lnk` 生成）在后续任务中按 plan.md
//! 任务号逐个提交，每个任务以 CI 编译通过为验收门禁。

use std::process::ExitCode;

const HELP: &str = "\
tbg-lite — zero-injection Windows taskbar grouping controller

USAGE:
    tbg-lite [--version | --help]

STATUS:
    skeleton (task 0) — see docs/plan.md for the roadmap

PLANNED SUBCOMMANDS (per docs/plan.md):
    run      watch top-level windows, rewrite PKEY_AppUserModel_ID per rules
    revert   restore original AppUserModelIDs and exit
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("-h") | Some("--help") => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        Some("-V") | Some("--version") => {
            println!("tbg-lite {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!(
                "tbg-lite: unknown argument '{other}' (skeleton: only --version/--help are available)"
            );
            ExitCode::from(2)
        }
    }
}
