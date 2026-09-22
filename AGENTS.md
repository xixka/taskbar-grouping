# AGENTS.md

## 项目定位

tbg-lite：零注入单文件 Windows 10/11 任务栏分组工具（Rust）。用 Shell 公开属性存储
API（`SHGetPropertyStoreForWindow` + `PKEY_AppUserModel_ID`）改写运行中窗口的分组
归属，不注入任何进程。实现路线与任务拆分见 `docs/plan.md`（路线 B+，§4）。

**双线路并行开发（2026-09-22 维护者决策）**：两条策略线路同时开工、同等维护，
通过 CLI 参数切换（任务 8 已实现：`watch --strategy ungroup|group`）——

- 线路一 `--strategy ungroup`（取消分组）：每窗口追加后缀 `~TBG~w<HWND>`，
  每窗口独立成组；
- 线路二 `--strategy group --group <NAME>`（自定义分组）：候选窗口统一改写为
  共享 AUMID `TBG.Group.<NAME>`，原值落盘 exe 同目录 `tbg-restore.tsv`，
  `restore` 据此复原。

两线路标记互斥、`--dry-run` 通用；`restore` 同时覆盖两线路还原路径。
Phase 0b（任务 5-9）已完成，`runtime-smoke` CI 绿灯（12 项断言）。
plan.md §7 Phase 1-3 原文为路线 A（注入 hook 移植），与本项目零注入红线
冲突；按本决策，后续任务一律在双线路 B+ 范围内立项（plan.md 对应章节待重写，
未重写前以本节为准）。

## 构建 / 测试 / lint 命令

- `cargo build --release` —— 唯一经验证的构建命令；提取自
  `.github/workflows/ci.yml`，已由 CI 实际运行通过（windows-latest）。本仓库验收
  门禁 = 该命令在 CI 绿灯。
- `ci/runtime-smoke.ps1` —— CI 运行时冒烟（任务 9，`runtime-smoke` job）：
  在 windows-latest 真实会话拉起 notepad 窗口，断言双线路 AUMID 改写/复原
  （12 项断言已全绿，run 35669840422）；仅由 CI 执行，本地未验证。
- `ci/phase0b-accept.ps1` —— CI Phase 0b 验收套件（任务 10，
  `phase0b-acceptance` job）：双线路各 50 窗口压测 + 常驻内存 <10MB 判定
  （门禁）；多应用覆盖子集、Edge 回写探针、UIA 任务栏按钮枚举
  （证据性探针，不设门禁）；结论见 docs/phase0b-acceptance.md；
  仅由 CI 执行，本地未验证。
- 禁止本地执行 cargo 构建/运行（本地无 Rust 工具链，且维护者明确禁止）；一切编译
  验证走 CI。
- `cargo fmt` / `cargo clippy` / `cargo test` 未配置、未验证 → 见"待确认"。

## 目录导览

- `src/main.rs` —— CLI 入口：`inspect` / `set` / `watch`（双线路
  `--strategy ungroup|group` 切换）/ `restore`（双线路还原）
- `src/appid.rs` —— AUMID 读写核心（属性存储 API）；线路一/线路二标记
  （`~TBG~w` 后缀 / `TBG.Group.` 共享前缀）
- `src/winevent.rs` —— `watch` 实现：SetWinEventHook 事件驱动 + 双线路改写
  （apply_ungroup / apply_group）+ 统计报告
- `src/restoremap.rs` —— 线路二还原映射表（`tbg-restore.tsv`；防 HWND 复用校验）
- `src/winutil.rs` —— 窗口/COM/字符串工具（枚举、应用窗口判定、cloak 检测）
- `ci/runtime-smoke.ps1` —— CI 运行时冒烟脚本（任务 9；双线路 AUMID 断言
  + 截图/explorer 探针，输出在 ci/out 工件）
- `ci/phase0b-accept.ps1` —— CI Phase 0b 验收套件（任务 10；50 窗口压测/
  内存/多应用覆盖/Edge 回写探针/UIA 任务栏按钮，输出在 ci/out 工件）
- `Cargo.toml` —— windows 0.58 依赖 feature 组；体积导向 release profile
- `.github/workflows/ci.yml` —— 唯一 CI workflow：`build`（release 编译门禁）
  + `runtime-smoke`（任务 9：运行时冒烟）+ `phase0b-acceptance`
  （任务 10：Phase 0b 验收）三个 job
- `docs/plan.md` —— 实施计划与任务号（提交一一对应任务号）
- `README.md` —— 对外项目定位

## 完成的定义（DoD）

1. CI（windows-latest，`cargo build --release`）通过；
2. 每个任务单独提交，格式 `feat(模块): 任务号-标题`；
3. 涉及 Windows 运行时行为的改动，在提交信息中注明"运行时行为待 Windows 实测"
   （CI 只验证编译，不验证行为）。任务 9 起：双线路 AUMID 行为已由
   `runtime-smoke` job 在 CI 断言通过（任务栏视觉/竞态/多应用覆盖仍需真机实测）。
   
## 硬约束（红线）

- 禁止注入路线代码：DLL 注入、`SetWindowsHookEx` 注入、inline hook、内存补丁、
  符号解析 hook——路线 B+ 零注入是项目边界（docs/plan.md §4）。
- 禁止弱化或删除 CI 步骤 / 验证断言；验证失败只能修复，最多 3 轮。
- 禁止本地编译或本地运行 cargo。
- 禁止提交机密（令牌、密钥、私有配置）。
- 直推 master；禁止 force push，回滚一律 `git revert`。
- 提交身份固定为 xaxka <xka@live.com>。
- 实际情况与 docs/plan.md 冲突时：停下报告，不许将错就错。
- 双线路同等维护（2026-09-22 增补）：线路一/线路二同步演进，任何 watch/
  restore 行为改动须同时验证两线路（runtime-smoke 两个 Phase 均须保持
  绿灯）；禁止只修/只留一条线路。

## 条件路由

- 改 CI 或构建命令 → 先读 `.github/workflows/ci.yml`
- 改依赖或 release profile → 读 `Cargo.toml` 与 docs/plan.md §6 预算表
- 实现新功能 → 在 docs/plan.md（§4 路线 B+、§7 Phase 0b/1/2/3）找到对应任务号，
  按任务号实现并单独提交；Phase 1-3 的路线 A 条目须先按“项目定位”双线路
  决策改写为 B+ 范围再立项，注入类条目不实现
- 改双线路行为 → 读 src/winevent.rs（apply_ungroup / apply_group）与
  src/appid.rs（标记定义），确认互斥标记与 restore 双路径不被破坏
- 改本文件 → 增量合并，不覆盖既有规则

## 待确认（未验证 / 未定）

- `cargo fmt` / `cargo clippy` / `cargo test` 是否纳入 CI 门禁（当前未配置未验证）
- `cargo build`（debug）未验证
- 发布流程未定（建议默认：暂不发布，后续手动 tag + GitHub Release）
- LICENSE 未定（建议默认：MIT）
- 配置文件路径未定（建议默认：exe 同目录 `config.toml`）
- docs/plan.md Phase 1-3 按双线路 B+ 范围重写（2026-09-22 决策后的待办）
