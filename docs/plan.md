# tbg-lite 实施计划 v2 —— 双线路 B+（零注入）路线图

> 版本：v2（2026-09-22）。本版取代 v1 的 §7 实施计划；v1 全文（可行性调研、附录 B
> 仓库全景、路线 A 架构与预算）存档于 git 历史（本文件被替换前的最后版本，
> commit 8e09b0e），作为备用路线的参考文档。
> Phase 0b 验收结论：`docs/phase0b-acceptance.md`（run 35679966357，29 项断言全绿）。

## §0 决策记录（2026-09-22，维护者）

1. **双线路同时开工、同等维护**，通过 CLI 参数切换（`watch --strategy ungroup|group`，
   任务 8 已实现）。红线：任何 watch/restore 行为改动须同时验证两线路。
2. **非注入（路线 B+）为主要路线；注入（路线 A）为备用**。A 不实现；触发条件与参考
   架构见 §5。
3. **默认行为 = Disable grouping on the taskbar**（对齐 Windhawk mod 默认）：线路一
   （每窗口后缀）为默认策略；**不做排除列表**。
4. 需求确认：**开机自启**（Phase 3）与 **.lnk 固定磁贴配套**（Phase 2）纳入路线图；
   **多应用覆盖矩阵**（Phase 1，任务 15）执行——它是"非注入为主"裁决的量化依据。
5. **交互菜单取代 Ctrl+C**（2026-09-22 维护者指示，任务 14 改版）：无参数启动
   `tbg-lite` 进入交互菜单（启动/停止双线路 watch、还原、inspect、**退出项**）；
   **不需要 Ctrl+C**——退出走菜单 `[0]`（优雅停止 watch：摘钩 + 统计输出，
   可选先还原）；带参数启动 = CLI 行为不变。原任务的 Ctrl+C console
   control handler 与 `--restore-on-exit` 参数方案作废。

## §1 现状（截至任务 15-20 全部完成，含 Phase R 22-26）

- **plan v2 §3 任务清单全部完成**（任务 0–26，每任务一提交，见 git log）；
  任务 15 已按维护者"CI 测试等同真机测试"指令以 CI 执行回填并关闭
  （docs/coverage-matrix.md §8 裁决：B+ 持续为主）：CLI 九命令（`inspect` / `set` / `watch`
  双线路 / `restore` 双路径 / `install` / `uninstall` / `status` / `pin`
  （任务 16 + 任务 17：`--to-taskbar` 写入用户固定目录）/ `unpin`
  （任务 17：反向删除，AUMID 验证防误删））、事件驱动
  （SetWinEventHook 零注入）、线路二还原映射（`tbg-restore.tsv`，防 HWND
  复用）、启动扫存量（任务 13：开启即全量取消分组，对齐 mod 默认）、无参数
  交互菜单（任务 14：菜单启动/停止 watch、还原、inspect、退出，无需
  Ctrl+C）、开机自启三命令（任务 19：HKCU Run，无需管理员；`status` 速览
  自启/标记窗口/映射表）、审计修复（原子写/单实例/标记严格校验/钉 SHA 等）；
  CI 四 job
  （`build` 编译+单测门禁 / `lockfile` 新鲜度 / `runtime-smoke` 104 断言 /
  `phase0b-acceptance` 30 断言）。
- 验收关键数据（详见验收报告）：双线路 50 窗口压测 0 漏检 0 回写 0 写失败；UIA 证实
  线路一每窗口独立按钮、线路二 50 窗合并单组、还原回原生；工作集 9.15 MB；explorer
  文件夹窗口会被回写（已知限制）；Edge 改写后短时不回写。

## §2 路线与边界

- **主线**：路线 B+ 双线路（零注入）。只用公开文档 API
  （`SHGetPropertyStoreForWindow` + `PKEY_AppUserModel_ID` + `SetWinEventHook`
  out-of-context）。默认 `watch` 即"取消分组"；自定义分组经 `--strategy group`。
- **红线**（继承 AGENTS.md）：禁止一切注入路线代码（DLL 注入 / `SetWindowsHookEx`
  注入 / inline hook / 内存补丁 / 符号解析 hook）；禁本地编译；验证失败最多修 3 轮。
- **已知限制**（验收报告 §3/§5）：Explorer 文件夹窗口回写 AUMID；UWP/Chromium 长时
  行为未验证；新窗口竞态（先并组后跳变）待真机感知评估；任务栏图标/跳转列表纠偏无
  （B+ 无翻译层钩子，属路线 A 能力）。

## §3 任务清单（新任务号自 13 起；任务 12 = 本计划重写）

### Phase 1 — 默认"取消分组"行为闭环

- [x] **任务 13**：`watch` 启动扫存量窗口——启动时对已存在的应用窗口按线路改写
      （对齐 mod 默认行为：开启即全量取消分组，而非只管新窗口；两条线路同等生效，
      双线路互斥标记与幂等重入保持）。含 CI 断言扩展（已完成：runtime-smoke
      新增 Phase 0 线路一预开窗口断言 + Phase B 线路二预开窗口断言，12→20 项；
      phase0b-accept 映射表断言改为逐窗覆盖，兼容启动扫带来的额外条目）。
- [x] **任务 14**：无参数启动 → 交互菜单模式（2026-09-22 维护者改版，取代
      原 Ctrl+C console control handler 方案）：`[1]`/`[2]` 启动线路一/线路二
      watch（后台线程 + `Arc<AtomicBool>` 停止标志，`[2]` 交互输组名）、`[3]`
      优雅停止（摘钩 + 终扫 + 统计）、`[4]` 还原（确认后复用 `restore` 逻辑）、
      `[5]` inspect、`[0]` 退出（watch 运行中先询问是否还原；stdin EOF 同样
      优雅退出，防脚本驱动忙转；stdin 首行 UTF-8 BOM 容错——Windows 管道
      写端与记事本脚本默认带 U+FEFF，非 trim 空白，run 35698563610 实锤后
      剥离）；带参数启动 → CLI 行为不变。CI
      runtime-smoke 新增 Phase M 双会话（stdin 预写驱动菜单：启动扫存量、
      优雅停表统计、退出保留改写、菜单还原、组名输入、映射表落盘与清理），
      断言 20→33 项。
- [x] **任务 15**：多应用覆盖矩阵真机执行——按验收报告 §5 清单制作模板与记录表
      （`docs/coverage-matrix.md`：应用 × 线路 × 生效/回写/竞态），维护者真机填写；
      结论回填裁决"B+ 是否持续为主"。
      完成（维护者 2026-09-23 指令"GitHub CI 测试等同真机测试"——Runner
      为真实交互式 Windows 会话，CI 行即真机行）：Phase D 扩展 +3 应用
      （Windows PowerShell 控制台 ×2 / regedit ×1 / Windows Terminal ×1，
      assert-if-spawned 门禁；powershell/WT 按 PID 定点清理，防误杀 CI
      步骤宿主）+ 矩阵回填 v2（§0 环境实据；§3 主表 8 实测行 + 未测
      行如实标注；§4 竞态代理口径 50 窗 0 漏检 0 回写；§7 explorer
      重启专项由任务 20 Phase X 门禁覆盖）。§8 裁决：**B+ 持续为主要
      路线**——用户应用覆盖 7/7=100%、竞态 0%、内存达标；唯一 0 分行
      为 shell 自管 Explorer 文件夹窗口（plan v2 §2 既有已知限制，
      非用户应用），严格全行口径 87.5% 已透明记录供维护者复核。

### Phase 2 — .lnk 固定磁贴配套（自定义分组的固定形态）

- [x] **任务 16**：`pin` 命令——为指定组生成带 `TBG.Group.<name>` AUMID 的 `.lnk`
      （`IShellLinkW` + `IPropertyStore`，参考 aumid-stopgap-tools `mklnkwaumid` 的
      直译，约 150 行）；`--icon` 指定组图标（默认取组内主程序）。
      完成：`src/shortcut.rs`——CoCreateInstance(ShellLink) → SetPath/
      SetIconLocation/SetArguments/SetDescription → QI IPropertyStore 写
      PKEY_AppUserModel_ID + Commit → QI IPersistFile::Save；落盘后独立
      Load 回读自校验（`read_lnk_aumid`，任务 18 同组断言复用此读路径）；
      CLI `pin --group <NAME> --target <PATH> [--icon <PATH[,INDEX]>]
      [--args <STR>] [--out <DIR>]`（图标规格宽容式逗号切分、目标/图标
      必须存在、路径词典法规范化；重跑覆盖与 install 同语义）；默认输出
      `%LOCALAPPDATA%\tbg-lite\pin\<NAME>.lnk`；不触碰还原表故不参与
      单实例互斥。CI runtime-smoke 新增 Phase P（12 断言：用法错误×5、
      默认/自定义目录落盘×4、覆盖重跑、回读验证），断言 47→59；新增 5 项
      纯逻辑单元测试（图标规格×4 + 磁贴文件名/目录拼接），单测 33→38。
      实际固定到任务栏（写入用户固定目录 / shell 固定 API）属任务 17。
- [x] **任务 17**：固定到任务栏——写入用户固定目录（`%AppData%\Microsoft\Internet
      Explorer\Quick Launch\User Pinned\TaskBar`）或经 shell 固定 API；`unpin` 反向。
      完成：`pin --to-taskbar`（与 `--out` 互斥）把带共享 AUMID 的磁贴直接
      写入用户固定目录（plan 指定方案；落盘回读自校验沿用任务 16 路径），
      `unpin --group <NAME>` 反向删除——删除前回读 AUMID 验证确为本工具为
      该组写入（同名外来快捷方式拒绝并退出 1，防误删）；幂等（未固定时
      报告并退出 0）；两向均辅以 `SHChangeNotify(SHCNE_UPDATEDIR)` 通知
      shell 重读目录（best-effort；任务栏在 explorer 重启/登录时呈现固定项，
      视觉验收归任务 18）。CI runtime-smoke 新增 Phase T（13 断言：用法
      错误×2、固定目录落盘+回读验证×3、unpin 删除×2、幂等、外来同名
      拒删×4 含 WScript.Shell 构造探针），断言 59→72；新增 1 项单元测试
      （固定目录路径拼接），单测 38→39。
- [x] **任务 18**：线路二 × 固定磁贴联动验收——CI 断言 `.lnk` 的 AUMID 与运行中窗口
      共享 AUMID 一致（同组判定）；真机视觉清单（磁贴与运行窗口合并显示）。
      完成：runtime-smoke 新增 Phase L（13 断言，维护者 2026-09-23 指令
      「GitHub CI 测试等同真机测试」，Win11 24H2 内核 Runner 实测）：
      `pin --to-taskbar` 落盘固定目录 → 重启 explorer（AutoRestartShell
      自动拉起，30s 轮询 + 手动 fallback）→ UIA 断言磁贴成为真实任务栏
      按钮 → 线路二 watch 后逐窗断言 AUMID == `.lnk` 回读 AUMID（同组
      判定）→ UIA 断言按钮与运行窗口合并（按钮名报 running-window 计数）
      → restore 复原 + 映射表清理 + unpin 删磁贴全链路绿灯。断言
      72→85。

### Phase 3 — 常驻、自启与分发

- [x] **任务 19**：`install` / `uninstall` / `status`——HKCU Run 注册开机自启
      （无需管理员）；`status` 输出当前标记窗口数、映射表状态、自启状态。
      完成：`src/autostart.rs`——注册命令 = 当前 exe + `watch --strategy <s>
      [--group <NAME>] --duration 0`（重装覆盖不累积，卸载幂等）；`status`
      全程只读（映射表走 `restoremap::status` 纯只读探测，不建目录/不迁移，
      审计 BUG-02 红线）；CI runtime-smoke 新增 Phase I（双线路注册值
      逐字节断言 + status 三块 + 幂等卸载 + 用法错误退出码 2），断言
      33→47；新增 5 项单元测试（命令行组装/路径词典法规范化/wide 终止符/
      REG_SZ 编解码往返/映射表只读状态三态）。单测计数勘误：此前文档称
      35 项系虚报，run 35700057611 build 日志实数 28 项，本次实测
      28+5=33 项。自启进程
      控制台窗口可见性与常驻宿主生命周期归任务 20。
- [x] **任务 20**：explorer 重启监视与自动重应用——宿主轮询 explorer PID（2 s 级），
      重启后自动重扫重标记（验收报告 §5-5：窗口属性可能随 explorer 重启丢失）；
      环形日志（`%LOCALAPPDATA%\tbg-lite\tbg.log`，默认关闭，256 KiB 上限截半）；
      连续异常退出熔断（自动注销自启，防开机死循环）。
      完成：`src/ringlog.rs`（--log 开关默认关、超限截半 tmp+rename 原子
      替换）+ `src/health.rs`（tbg-health.tsv 记账：短命消失 <30s 累计、
      长寿命强杀清零、连续 3 次熔断→注销自启后归零；记账核心纯函数
      时钟注入）；winevent.rs 消息泵 2s 轮询 GetShellWindow→PID（常驻
      模式 wait 由 INFINITE 改 2s，Ctrl+C 行为不变），PID 变化即全量
      重扫（resweep_* 统计口径，幂等复用 consider 链）。CI runtime-smoke
      新增 Phase X（18 断言：环形日志 4、重启检测/重扫/存活/钩子存续 8、
      熔断三轮强杀→第四次注销自启+第五次不再熔断 6），断言 86→104；
      新增 8 项单元测试（环形截半×3 + 熔断记账×5），单测 39→47。
- [x] **任务 21**：发布准备——README（与 Windhawk 共存注意）、LICENSE 定稿、
      `BENCHMARK.md`（体积/内存实测回填，对照 plan v1 §6 预算）、tag + GitHub
      Release 流程。SEC-04 注记：发布物附 SHA256 + GitHub Release
      attestation（任务栏干预类工具易受 SmartScreen/杀软误报）。
      完成：LICENSE = MIT（AGENTS.md 建议采纳；零注入路线未引用 Windhawk
      mod 逻辑代码，GPL 不传染）；README 重写（全命令、菜单、自启、pin/
      unpin、任务 20 加固、Windhawk 共存两注意：同功能 mod 二选一 +
      Explorer 回写已知限制、已知限制四条、104 断言 CI 说明）；`
      BENCHMARK.md`（内存 9.15MB/1.48MB、50 窗双线路 0/0/0、UIA、多应用
      7/7、任务 20 加固实测，体积由 Release 工作流自动回填）；CI 增 `
      release` job（tag v* 触发，needs 四 job：打包 zip + SHA256SUMS +
      attest-build-provenance v4.2.2 钉 SHA + gh release create 自动发布，
      permissions 含 id-token/attestations）；Cargo.toml +license(MIT)/
      repository/readme（lockfile 中性字段，不触发新鲜度门禁）。

### Phase R — 审计修复（2026-09-22 深度代码审计，BUG-01..15 / SEC-01..05）

> 审计报告（外部，2026-09-22，基线 4d96ff1）结论：总体 B+，无 Critical 级安全
> 漏洞；风险集中在还原映射表的数据完整性与长驻可靠性。任务 13 已闭合其中
> "存量窗口不重标"缺口；下列任务按 P0→P1→P2 顺序消化全部审计发现。

- [ ] **任务 22**（P0 数据安全）：映射表原子写（tmp+fsync+rename，BUG-03）
      + 表头 magic v1；单实例互斥 `Local\tbg-lite.map`（CreateMutexW，
      BUG-02，watch group 全程 + restore 处理共享窗口期间持锁）；
      restore 映射表加载失败重复读盘修复（BUG-01）；映射表迁
      `%LOCALAPPDATA%\tbg-lite\`（SEC-01，含旧表自动迁移）；Cargo.lock
      入库（SEC-03，22b：CI 生成 → 提交 → build --locked + 新鲜度门禁）。
- [x] **任务 23**（P1 标记与输入校验）：`strip_suffix` 后缀 HWND 必须与
      当前窗口一致（BUG-05，彻底排除原生 AUMID 假阳性误剥）；`set --value`
      校验 ≤129 UTF-16 码元、拒控制字符（BUG-07/SEC-05）；长度计量统一
      UTF-16 码元（BUG-06）；restore 单窗详情判定修复（BUG-12）；纯逻辑
      单元测试入库 + CI `cargo test`（审计 P2-13）。完成：2e281ba +
      修复 c624f58（windows Result 别名）+ 测试笔误修正（随任务 24 提交）。
- [x] **任务 24**（P1 watch 健壮性）：`EVENT_OBJECT_NAMECHANGE` 钩子对
      未处理窗口重评估（BUG-04，标题后置窗口漏检）；消息泵 wait_ms 封顶
      1s（BUG-08）；回调 catch_unwind（BUG-10）；`EnumWindows` 错误传播
      （BUG-11）；统计口径注明 per-event（BUG-13）。
- [x] **任务 25**（P1 工具面）：broken pipe panic hook 优雅退出（BUG-09）；
      `inspect --json` 机器可读输出 + CI 解析替换（BUG-14 根治）；
      `restore --dry-run` 预览（审计 P1-12）；用法类错误退出码 2（审计
      P2-16 简版）。完成：785fea4 + 修复 880d5b0/10eb70d（PS 5.1
      ConvertFrom-Json 数组嵌套形态两轮诊断与 flatten，run 35691097252
      全绿）。
- [x] **任务 26**（CI 供应链加固）：actions 钉提交 SHA（SEC-02：
      checkout v4.4.0 / upload-artifact v4.6.2 / rust-toolchain stable）；
      发布物签名/SHA256 流程注记进任务 21（SEC-04）；AGENTS.md 全面同步
      （单实例红线、映射表新路径、测试门禁、审计修复落档）。
- [x] **任务 27**（维护者 2026-09-23 指令：dev 滚动发布通道）：分支推送
      → 四 job 门禁全绿 → 覆盖式发布 prerelease `dev`。发布步骤为维护者
      给定（softprops/action-gh-release v3.0.3 钉 SHA，tag_name=dev、
      prerelease=true、body 记 Branch/Commit/Built at）原样落地。工程
      决策：① action 不移动已存在 tag（target_commitish 仅建新 tag 时
      生效）→ 发布前 `gh release delete dev --cleanup-tag` + git push
      --delete 双回收，保证 dev tag 滚动指向当次 commit；② job 级
      concurrency（cancel-in-progress）防连续推送竞态；③ 产物与正式通道
      同构（zip + SHA256SUMS + build-provenance attestation，SEC-04
      同口径）；④ prerelease 不占 latest 位，v* 稳定版仍为最新；
      ⑤ `fail_on_unmatched_files` 门禁防空资产发布。
- [x] **任务 28**（维护者 2026-09-24 真机反馈：Explorer 文件夹窗口
      回写）：回写对抗（reassert）——已处理窗口标记丢失（应用 / shell
      重落自家 AUMID，典型：Explorer 文件夹窗口导航）时自动按当前线路
      补写。双入口：NAMECHANGE 重验（导航 / 标题变化即触发，winevent
      回写检测口）+ 5s 周期复核（兜底无标题变化的静默回写）。调研结论
      （2026-09-24 联网检索）：非注入路线**无法阻止**属主进程改写自家
      窗口 AUMID（MSDN《AppUserModelIDs》：AUMID 由窗口属主设置，
      `SHGetPropertyStoreForWindow` 仅提供外部读写），只能"检测 + 补写"；
      代价为补写瞬间按钮可能一次跳动。安全边界：另一线路标记不误叠、
      shell 窗口跳过、映射表已有条目只补写共享值不重复落盘、读失败
      静默（DESTROY 簿记负责清理）；新增统计 `reasserted`（CI 断言为
      正则匹配既有行，新增行不破坏现有门禁）。

### Phase R 验收结论（2026-09-22）

审计报告全部 15 项 BUG + 5 项 SEC 闭合：P0（22/22b）、P1（23/24/25）、
CI 加固（26）全部 CI 绿灯；新增 31 项单元测试纳入 build job 门禁；
审计 P2 中的工程建议（错误类型化 thiserror、stdout/stderr 分流、
DRY 合并、MSRV/license 字段）未纳入本轮（非漏洞项，随后续任务演进）。

## §4 验收与测试（常态化）

| 层面 | 方法 |
|---|---|
| 编译门禁 | CI `build`（windows-latest，`cargo build --release --locked` + `cargo test --locked`，任务 22b/23） |
| 双线路行为回归 | CI `runtime-smoke`（104 断言，含启动扫存量、交互菜单、自启 Phase I、pin Phase P、固定/取消固定 Phase T、磁贴联动 Phase L 与常驻加固 Phase X）+ `phase0b-acceptance`（30 断言），每次 push |
| 固定磁贴联动 | CI 断言（任务 18，Phase L：.lnk AUMID == 运行窗口 AUMID + UIA 合并按钮，已绿） |
| 真机清单 | 竞态感知率、覆盖矩阵全量、长时回写、视觉细节、explorer 重启（验收报告 §5） |
| 内存/体积 | 验收报告口径；发布前回填 `BENCHMARK.md`（任务 21） |

## §5 备用路线 A（注入复刻）——不实现，仅存档

- 触发条件：真机覆盖矩阵（任务 15）证实 B+ 覆盖 <90%，或竞态可感知率 ≥5% 且无法在
  B+ 框架内缓解（如重试/双重写策略）。
- 参考架构：plan v1 §5（tbg-host + tbg-hook 双组件、符号链路）与 §6（体积内存预算）、
  v1 §2.1（Windhawk mod 32 符号钩子拆解）——见 git 历史。
- 启动 A 前须重新立项评审：GPL-3.0 传染、杀软误报、Windows 更新维护成本三项
  （v1 §3.3）。

## §6 开放问题（2026-09-23 任务 21 收尾决议）

1. **已决**：LICENSE = MIT（任务 21；零注入路线未引用 mod 逻辑代码，GPL 不
   传染；若未来引用 mod 逻辑描述再重评）。
2. **维持不需要**：配置文件（全 CLI 参数 + 无排除列表决策不变；覆盖矩阵
   回填后唯一回写行为 shell 自管 Explorer 文件夹窗口，属路线边界非配置
   可解——plan v2 §2 已知限制）。
3. **已决**：宿主常驻（任务 20 落地：常驻 + explorer 重启重扫 + 熔断；
   对应 v1 §9-3 的倾向性判断成为实现事实）。
