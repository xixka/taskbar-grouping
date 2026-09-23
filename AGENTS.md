# AGENTS.md

## 项目定位

tbg-lite：零注入单文件 Windows 10/11 任务栏分组工具（Rust）。用 Shell 公开属性存储
API（`SHGetPropertyStoreForWindow` + `PKEY_AppUserModel_ID`）改写运行中窗口的分组
归属，不注入任何进程。实现路线与任务拆分见 `docs/plan.md`（v2，§2 路线 /
§3 任务清单）。

**双线路并行开发（2026-09-22 维护者决策）**：两条策略线路同时开工、同等维护，
通过 CLI 参数切换（任务 8 已实现：`watch --strategy ungroup|group`）——

- 线路一 `--strategy ungroup`（取消分组）：每窗口追加后缀 `~TBG~w<HWND>`，
  每窗口独立成组；
- 线路二 `--strategy group --group <NAME>`（自定义分组）：候选窗口统一改写为
  共享 AUMID `TBG.Group.<NAME>`，原值落盘 `%LOCALAPPDATA%\tbg-lite\
  tbg-restore.tsv`（任务 22 起，原子写 + 表头 v1 + 旧表自动迁移），
  `restore` 据此复原。

两线路标记互斥、`--dry-run` 通用；`restore` 同时覆盖两线路还原路径。
Phase 0b（任务 5-10）已验收：runtime-smoke + phase0b-acceptance 断言全绿
（docs/phase0b-acceptance.md），UIA 证实双线路任务栏层效果；
任务 13 起 runtime-smoke 扩至 20 项断言（含启动扫存量），任务 14 起扩至
33 项（含交互菜单 Phase M），任务 19 起扩至 47 项（含自启 Phase I），
任务 16 起扩至 59 项（含 pin Phase P），任务 17 起扩至 72 项（含
固定/取消固定 Phase T），任务 18 起扩至 86 项（含磁贴联动 Phase L）。

**固定磁贴（任务 16/17/18，Phase 2）**：`pin` 命令为线路二分组生成带共享
AUMID `TBG.Group.<NAME>` 的 `.lnk`（`src/shortcut.rs`，mklnkwaumid
直译）：磁贴与被 watch 改写的运行中窗口共享同一 AUMID，任务栏据此
归组。落盘后独立 Load 回读自校验，不成功不报告成功；不触碰还原表故不
参与单实例互斥。任务 17：`pin --to-taskbar` 把磁贴直接写入用户固定
目录 `%APPDATA%\...\User Pinned\TaskBar`（与 `--out` 互斥），
`unpin --group` 反向删除——删前回读 AUMID 验证确为本工具所写（同名
外来快捷方式拒删退出 1）；两向辅以 `SHChangeNotify` 通知 shell
（best-effort；固定项在 explorer 重启/登录时呈现）。任务 18（维护者
2026-09-23 指令：CI 测试等同真机测试）：Phase L 在 CI 实测端到端联动
——重启 explorer 后 UIA 断言磁贴成为真实任务栏按钮、运行窗口 AUMID ==
磁贴 AUMID（同组）、按钮与运行窗口合并（按钮名报 running-window
计数）。run 35807254850 实锤"仅写固定文件夹不产生固定按钮"（Taskband
注册表才是固定项真源）→ pin/unpin 经 shell `taskbarpin`/`taskbarunpin`
动词（ShellExecuteExW 公开入口，零注入）登记/注销固定项。

**交互菜单（任务 14，2026-09-22 维护者改版）**：无参数启动 `tbg-lite` 进入
交互菜单（`src/menu.rs`）——`[1]`/`[2]` 启动线路一/线路二 watch（后台线程 +
`Arc<AtomicBool>` 停止标志）、`[3]` 优雅停止（摘钩 + 统计）、`[4]` 还原、
`[5]` inspect、`[0]` 退出（运行中先询问是否先还原；stdin EOF 同样优雅退出）；
**不需要 Ctrl+C**——退出走菜单；带参数启动 = CLI 行为不变。原任务的
Ctrl+C console control handler 与 `--restore-on-exit` 方案作废
（plan v2 §0 决策 5）。
深度代码审计修复（2026-09-22，任务 22-26/Phase R）：15 项 BUG + 5 项 SEC
全部闭合——还原表原子写/单实例互斥/迁 %LOCALAPPDATA%（P0）、标记 HWND
严格校验、NAMECHANGE 重评估、inspect --json、restore --dry-run、
actions 钉 SHA、Cargo.lock 入库 --locked 构建；31 项单元测试入 CI 门禁。
详见 docs/plan.md v2 Phase R。
据此：非注入 B+ 为主要路线，注入 A 为备用（plan v2 §5，不实现）；
默认行为 = Disable grouping on the taskbar，无排除列表；后续任务一律
按 plan v2 §3 任务清单立项。

## 构建 / 测试 / lint 命令

- `cargo build --release` —— 唯一经验证的构建命令；提取自
  `.github/workflows/ci.yml`，已由 CI 实际运行通过（windows-latest）。本仓库验收
  门禁 = 该命令在 CI 绿灯。
- `ci/runtime-smoke.ps1` —— CI 运行时冒烟（任务 9 + 13 + 14 + 19 + 16 + 17
  + 18，
  `runtime-smoke` job）：在 windows-latest 真实会话拉起 notepad 窗口，断言
  双线路 AUMID 改写/复原、任务 13 启动扫存量（Phase 0 线路一 / Phase B
  线路二预开窗口断言）、任务 14 交互菜单（Phase M 双会话：stdin 预写驱动，
  无参启动 → 菜单启停 watch/还原/退出，全程无 Ctrl+C）、任务 19 自启
  （Phase I：双线路 install 注册值逐字节比对、status 三块状态、幂等卸载、
  用法错误退出码 2）、任务 16 pin（Phase P：.lnk 磁贴落盘与回读验证、
  默认/自定义目录、覆盖重跑、用法错误×5）与任务 17 固定/取消固定
  （Phase T：--to-taskbar 写入用户固定目录、unpin 删除/幂等、同名外来
  .lnk 拒删安全阀）与任务 18 磁贴联动（Phase L：重启 explorer 后 UIA
  断言磁贴为真实任务栏按钮、运行窗口 AUMID == 磁贴 AUMID 同组、按钮
  与运行窗口合并）；86 项断言；仅由 CI 执行，
  本地未验证。
- `ci/phase0b-accept.ps1` —— CI Phase 0b 验收套件（任务 10，
  `phase0b-acceptance` job）：双线路各 50 窗口压测 + 常驻内存 <10MB 判定
  （门禁）；多应用覆盖子集、Edge 回写探针、UIA 任务栏按钮枚举
  （证据性探针，不设门禁）；结论见 docs/phase0b-acceptance.md；
  仅由 CI 执行，本地未验证。
- 禁止本地执行 cargo 构建/运行（本地无 Rust 工具链，且维护者明确禁止）；一切编译
  验证走 CI。
- `cargo test`（任务 23 起）：39 项纯逻辑单元测试（任务 19 增自启命令行组装/
  路径词典法规范化/wide 终止符/REG_SZ 编解码往返/映射表只读状态三态 5 项；
  任务 16 增 pin 图标规格解析×4 + 磁贴文件名/目录拼接 1 项；任务 17 增
  任务栏固定目录路径拼接 1 项）
  ——计数勘误：此前称 35 项系虚报，run 35700057611 build 日志实数 28 项，
  28+5+5+1=39；纳入 build job 门禁
  （`cargo test --locked`）；`cargo build --release --locked`（任务 22b 起）。
- `cargo fmt` / `cargo clippy` 未配置、未验证 → 见"待确认"（需一次性格式
  化任务，见审计 P2-14）。

## 目录导览

- `src/main.rs` —— CLI 入口：无参数 → 交互菜单（任务 14，`menu::run`）；
  `inspect`（含 `--json` 机器可读）/ `set` / `watch`（双线路
  `--strategy ungroup|group` 切换）/ `restore`（双线路还原 + `--dry-run`
  预览）/ `install` / `uninstall` / `status`（任务 19 开机自启与状态速览）/
  `pin`（任务 16 生成线路二分组磁贴 .lnk；任务 17 `--to-taskbar` 写入
  用户固定目录）/ `unpin`（任务 17 反向删除，AUMID 验证防误删、幂等）；
  panic hook 管道断裂优雅退出；用法错误退出码 2
- `src/menu.rs` —— 交互菜单（任务 14）：watch 后台线程化（停止标志优雅
  退出，取代 Ctrl+C）、复用 cmd_inspect/cmd_restore、stdin EOF 优雅退出
- `src/appid.rs` —— AUMID 读写核心（属性存储 API）；线路一/线路二标记
  （`~TBG~w` 后缀 / `TBG.Group.` 共享前缀）
- `src/winevent.rs` —— `watch` 实现：SetWinEventHook 事件驱动 + 双线路改写
  （apply_ungroup / apply_group）+ 启动扫存量（任务 13：开启即全量改写，
  幂等重入/双线路互斥）+ 统计报告
- `src/restoremap.rs` —— 线路二还原映射表（`%LOCALAPPDATA%\tbg-lite\
  tbg-restore.tsv`，任务 22 起；原子写 tmp+fsync+rename、表头 v1、旧表
  自动迁移；防 HWND 复用校验）
- `src/singleinstance.rs` —— 映射表单实例互斥（任务 22：
  `Local\tbg-lite.map` 命名互斥体，审计 BUG-02）
- `src/autostart.rs` —— 开机自启（任务 19）：HKCU Run `tbg-lite` 值的
  读/写/删（install 覆盖式、uninstall 幂等、read_command 只读；REG_SZ
  UTF-16LE 编解码含往返单测）；注册命令 = 当前 exe + `watch` 参数尾
  （`--duration 0` 常驻，宿主生命周期归任务 20）
- `src/shortcut.rs` —— 固定磁贴（任务 16/17）：`create_pin` 生成带共享
  AUMID 的 `.lnk`（IShellLinkW + IPropertyStore + IPersistFile，mklnkwaumid
  直译）与 `read_lnk_aumid` 独立回读（任务 18 同组断言复用）；任务 17：
  `pinned_taskbar_dir`（用户固定目录）、`notify_shell_dir_change`
  （SHChangeNotify best-effort）；图标规格宽容式逗号切分（纯逻辑单测）；
  不触碰还原表故不参与单实例互斥
- `src/winutil.rs` —— 窗口/COM/字符串工具（枚举、应用窗口判定、cloak 检测）
- `ci/runtime-smoke.ps1` —— CI 运行时冒烟脚本（任务 9 + 13 + 14 + 19 + 16
  + 17 + 18；
  双线路 AUMID 断言 + 启动扫存量断言 + 交互菜单 Phase M + 截图/explorer 探针
  + 自启 Phase I + pin Phase P + 固定/取消固定 Phase T + 磁贴联动 Phase L
  （含 UIA 任务栏按钮枚举与 explorer 重启辅助、taskbarpin/taskbarunpin
  动词调用），输出在 ci/out 工件）
- `ci/phase0b-accept.ps1` —— CI Phase 0b 验收套件（任务 10；50 窗口压测/
  内存/多应用覆盖/Edge 回写探针/UIA 任务栏按钮，输出在 ci/out 工件）
- `Cargo.toml` —— windows 0.58 依赖 feature 组；体积导向 release profile
- `.github/workflows/ci.yml` —— 唯一 CI workflow：`build`（release 编译门禁）
  + `lockfile`（任务 22：Cargo.lock 生成/新鲜度门禁）+ `runtime-smoke`
  （任务 9：运行时冒烟）+ `phase0b-acceptance`（任务 10：Phase 0b 验收）
  四个 job
- `docs/plan.md` —— 实施计划 v2（§0 决策 / §3 任务清单；提交一一对应任务号）
- `docs/coverage-matrix.md` —— 多应用覆盖矩阵真机记录表（任务 15 模板：应用 ×
  线路 × 生效/回写/竞态主表 + 竞态/长时/视觉/explorer 重启四专项 + §8 裁决
  回填框架；维护者真机填写，结论决定 B+ 是否持续为主）
- `docs/phase0b-acceptance.md` —— Phase 0b 验收报告（CI 量化证据与真机待验清单）
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
- 映射表单实例（2026-09-22 任务 22 增补，审计 BUG-02）：任何会写
  `tbg-restore.tsv` 的进程必须持有 `Local\tbg-lite.map` 互斥体
  （`src/singleinstance.rs`）；新增写表代码路径必须先 acquire；禁止
  移除或绕过互斥（last-writer-wins 会丢失用户窗口原值）。

## 条件路由

- 改 CI 或构建命令 → 先读 `.github/workflows/ci.yml`
- 改依赖或 release profile → 读 `Cargo.toml`；体积/内存口径参考
  docs/phase0b-acceptance.md §4（v1 §6 预算表已随 v1 存档于 git 历史）
- 实现新功能 → 在 docs/plan.md v2 §3 任务清单找到对应任务号，按任务号实现
  并单独提交；注入类条目不实现（路线 A 为备用，见 plan v2 §5）
- 改双线路行为 → 读 src/winevent.rs（apply_ungroup / apply_group）与
  src/appid.rs（标记定义），确认互斥标记与 restore 双路径不被破坏
- 改本文件 → 增量合并，不覆盖既有规则

## 待确认（未验证 / 未定）

- `cargo fmt` / `cargo clippy` 是否纳入 CI 门禁（需一次性格式化任务；审计 P2-14）
- `cargo build`（debug）未验证
- 发布流程未定（SEC-04：发布时附 SHA256 + Release attestation，任务 21）
- LICENSE 未定（建议默认：MIT）
- 配置文件路径未定（建议默认：exe 同目录 `config.toml`；若引入见 plan v2 §6-2）
