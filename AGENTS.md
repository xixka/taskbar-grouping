# AGENTS.md

## 项目定位

tbg-lite：零注入单文件 Windows 10/11 任务栏分组工具（Rust）。用 Shell 公开属性存储
API（`SHGetPropertyStoreForWindow` + `PKEY_AppUserModel_ID`）改写运行中窗口的分组
归属，不注入任何进程。实现路线与任务拆分见 `docs/plan.md`（路线 B+，§4）。

## 构建 / 测试 / lint 命令

- `cargo build --release` —— 唯一经验证的构建命令；提取自
  `.github/workflows/ci.yml`，已由 CI 实际运行通过（windows-latest）。本仓库验收
  门禁 = 该命令在 CI 绿灯。
- `ci/runtime-smoke.ps1` —— CI 运行时冒烟（任务 9，`runtime-smoke` job）：
  在 windows-latest 真实会话拉起 notepad 窗口，断言双线路 AUMID 改写/复原；
  仅由 CI 执行，本地未验证。
- 禁止本地执行 cargo 构建/运行（本地无 Rust 工具链，且维护者明确禁止）；一切编译
  验证走 CI。
- `cargo fmt` / `cargo clippy` / `cargo test` 未配置、未验证 → 见"待确认"。

## 目录导览

- `src/main.rs` —— CLI 入口（骨架，功能按任务号增量提交）
- `ci/runtime-smoke.ps1` —— CI 运行时冒烟脚本（任务 9；双线路 AUMID 断言
  + 截图/explorer 探针，输出在 ci/out 工件）
- `Cargo.toml` —— windows 0.58 依赖 feature 组；体积导向 release profile
- `.github/workflows/ci.yml` —— 唯一 CI workflow：`build`（release 编译门禁）
  + `runtime-smoke`（任务 9：运行时冒烟）两个 job
- `docs/plan.md` —— 实施计划与任务号（提交一一对应任务号）
- `README.md` —— 对外项目定位

## 完成的定义（DoD）

1. CI（windows-latest，`cargo build --release`）通过；
2. 每个任务单独提交，格式 `feat(模块): 任务号-标题`；
3. 涉及 Windows 运行时行为的改动，在提交信息中注明"运行时行为待 Windows 实测"
   （CI 只验证编译，不验证行为）。任务 9 起补充：双线路 AUMID 行为已由
   `runtime-smoke` job 在 CI 断言（任务栏视觉/竞态/多应用覆盖仍需真机实测）。
   
## 硬约束（红线）

- 禁止注入路线代码：DLL 注入、`SetWindowsHookEx` 注入、inline hook、内存补丁、
  符号解析 hook——路线 B+ 零注入是项目边界（docs/plan.md §4）。
- 禁止弱化或删除 CI 步骤 / 验证断言；验证失败只能修复，最多 3 轮。
- 禁止本地编译或本地运行 cargo。
- 禁止提交机密（令牌、密钥、私有配置）。
- 直推 master；禁止 force push，回滚一律 `git revert`。
- 提交身份固定为 xaxka <xka@live.com>。
- 实际情况与 docs/plan.md 冲突时：停下报告，不许将错就错。

## 条件路由

- 改 CI 或构建命令 → 先读 `.github/workflows/ci.yml`
- 改依赖或 release profile → 读 `Cargo.toml` 与 docs/plan.md §6 预算表
- 实现新功能 → 在 docs/plan.md（§4 路线 B+、§7 Phase 0b/1/2/3）找到对应任务号，
  按任务号实现并单独提交
- 改本文件 → 增量合并，不覆盖既有规则

## 待确认（未验证 / 未定）

- `cargo fmt` / `cargo clippy` / `cargo test` 是否纳入 CI 门禁（当前未配置未验证）
- `cargo build`（debug）未验证
- 发布流程未定（建议默认：暂不发布，后续手动 tag + GitHub Release）
- LICENSE 未定（建议默认：MIT）
- 配置文件路径未定（建议默认：exe 同目录 `config.toml`）
