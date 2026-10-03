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
6. **双版本并行发布：非注入版（tbg-lite）为主，注入版（tbg-inject）为独立
   产物**（2026-09-28 维护者指令"生成 2 个 exe 版本，一个注入，一个非注入"，
   推翻决策 2 的"A 不实现"与 §5 存档状态）。立项评审三项结论（§5 重写）：
   ① GPL——注入版采用**清室设计**（IAT 重定向公开导出
   `shell32!SHGetPropertyStoreForWindow` + COM 委托属性存储，全部公开文档
   API，零引用 Windhawk mod 逻辑与 v1 符号表），MIT 不受传染；② 杀软——
   注入版**预期会被启发式引擎报毒**（DLL 注入 explorer 属引擎重点行为面，
   这是真实行为特征而非误报），只经独立 zip 通道发布并附验真口径；③ 维护
   成本——不依赖私有符号/偏移，按导入名解析，Windows 更新适配成本最小化。
   红线同步改写（§2）：注入代码**仅允许存在于 `inj/` 工作区成员**，
   `tbg-lite` 主包保持零注入。
7. **支持范围收缩为仅 Win11**（2026-10-01 维护者指令"只要支持 win11 就
   可以了，win10 有其他稳定软件支持"，追加指令"删除，提示用户只支持
   win11"）。落点：① 经典任务栏 CI 腿（runtime-smoke-inj job）移除——
   其首轮已完成证伪使命（§5 限制⑤扩展：经典任务栏同样走窗口原子
   快速路径，结构性一致）；② 测试脚本 ci/runtime-smoke-inj.ps1 亦删除
   （git 历史可溯）；③ §5 所记 GetPropW 拦截面扩展探针不再 pursue
   （验证收益仅限经典任务栏场景）；④ Win11 腿 Phase INJ 继续作为
   注入版门禁 + 未来金丝雀；⑤ 双 exe 启动提示：oscheck.rs（主版 +
   注入版各一）经 windows-version 0.1.1（ntdll!RtlGetVersion，不受
   清单"版本骗报"影响）读真实 build，低于 22000 打 stderr 单行警告后
   照常执行——仅提示、不阻断、不改退出码（范围决议是不再测试与承诺
   而非故意破坏；CI 全腿 build ≥ 26200，警告不进门禁日志，stdout
   机器输出不受影响）；⑥ README 头部显著声明 + 双版帮助文本 + 交付
   口径同步。Win10 行为不再测试、不再承诺。
8. **删除注入版，仓库收缩为纯 tbg-lite 单包**（2026-10-02 维护者指令
   "那就删除注入版"，终审语境：维护者目标是"社区 Windhawk 同类 mod 的
   效果但不占 Windhawk 的内存"——经核实该目标已由主版达成，注入版
   对用户零可见效果）。背景：注入版基础设施（注入/拦截/摘钩/自愈）端到端
   为绿，但 Win11 分组拦截不可达（已知限制⑤，§5 证据链：Win11 与经典
   任务栏两代实测结构性一致）。删除范围：① `inj/` 三工作区成员与
   `[workspace]` 节（Cargo.lock 同步收缩）；② CI runtime-smoke Phase INJ
   腿（27 断言）及其专属辅助函数；③ release / dev-release 的注入版
   zip 打包与独立 attestation；④ README 注入版章节（改为删除说明 +
   老用户残留清理指引）、BENCHMARK §6（改墓碑注）、AGENTS.md 双版本
   段落与红线、菜单 `[6]` 文案。红线同步改写（§2）：注入路线代码回到
   "禁止存在于本仓库"，决策 6 的"仅允许存在于 inj/"条款作废；
   AGENTS"禁止弱化 CI 断言"红线对本次 Phase INJ 移除以本决策记录为
   依据（主版门禁不得援引此例）。附注：金丝雀随注入版退役——未来
   Windows 若将分组读取路由回文档化调用，不再有自动探测信号，重见
   该信号需重启路线 A（以 git 历史实现 + §5 技术结论为基准重新立项）。
   菜单 `[6]` 的 Windhawk 协同指引保留（Explorer 文件夹窗口仍是
   非注入路线无法根治的唯一场景）。
9. **CI 工程优化三项**（2026-10-02 维护者指令"做 1，2，3"，采纳工程侧
   优化提案）：① **构建缓存**——六个编译型 job（build / runtime-smoke /
   phase0b-acceptance / release / dev-release / lint）加
   Swatinem/rust-cache v2.9.2（钉 SHA，SEC-02 口径）；此前每次推送各
   job 独立冷编依赖树（单次 release 构建约 2-4 分钟 ×5，流水线时长大
   头），缓存命中后增量构建秒级完成。门禁语义不变：各 job 仍从源码
   独立构建，release / attestation 仍本源构建（证明主题为产物摘要，
   与增量编译正交）。lockfile job 不加缓存（无编译产物，缓存只增噪声）。
   ② **静态检查门禁**（审计 P2-14 收口）——新增 `lint` job：
   `cargo fmt --all --check` + `cargo clippy --locked --workspace
   --all-targets -- -D warnings`（默认 lint 组不加 pedantic——定位是
   防漂移而非风格强加）；全仓一次性格式化随本决策完成（此前仓库从未
   rustfmt 化），clippy 首跑 9 项告警同笔清零（menu unused_mut、
   main 嵌套 unsafe / print_literal / items_after_test、autostart
   chunks_exact / is_multiple_of、winevent thread_local const /
   map_or / items_after_test——全部行为保持的机械修复）；release /
   dev-release 的 needs 纳入 lint（门禁只增不减；任务 21 / 27 原文
   "四 job"保留为历史记录，现状五 job）。③ **工具链钉版**——仓库根
   `rust-toolchain.toml`（channel = 1.99.0）+ `Cargo.toml`
   `rust-version = "1.99"` + ci.yml 全部 Install Rust 显式
   `with.toolchain: 1.99.0`；此前 @stable 为移动目标（不同日期推送的
   编译器版本不可复现），钉版同时固定 rustfmt / clippy 行为（②的
   确定性前提）。版本依据：1.99.0 自 2026-10-01 为 stable 通道且有本
   仓库绿色实绩（run 37002122332，master 6e2806f）；lockfile 新鲜度
   在 1.99.0 下已验证无漂移。发布说明 MSRV 文案同步（"stable
   toolchain" → "1.99.0 pinned"）。落地方式：预检分支 ci-preflight-0-9
   三轮（本地零 cargo 红线不破——fmt 一次性格式化与 clippy 报告由 CI
   执行并提交回分支，正式门禁由 lint job 承担；ci run 37028470998 /
   37030144922 为 fmt 诊断轮，37030670464 五门全绿）→ master 三笔提交
   （钉版 / 缓存 / lint+格式化+清零+文档），验证 run 37031746903
   （ed125b9）：五门全绿 + dev-release 滚动发布；缓存实测 build job
   全程 83 s（对照冷编约 2-4 分钟）、lint 94 s、lockfile 23 s。
10. **任务清单收束归档**（2026-10-03 维护者指令"清理已经完成的plan"）：
    §3 任务明细（任务 13–37，311 行）全部闭合，整体归档 git 历史
    （本文件被替换前的最后版本 = commit 489fabf；每任务一提交，逐任务
    记录见 git log）；§3 改留收束摘要表（Phase × 任务号 × 一行内容）
    与衔接规则（新任务号自 38 起，立项 = §0 新决策条目 + §3 新 Phase
    小节）。归档同步修正一处陈年笔误：任务 22 复选框在归档前版本为
    漏勾（`- [ ]`），实际已闭合（Phase R 验收结论与 AGENTS.md 审计
    修复段均有记载）。§0 决策 / §1 现状快照 / §2 边界 / §4 验收口径 /
    §5 技术结论 / §6 决议全量保留——台账活值在这些节。AGENTS.md 对
    §3 的四处引用同步改口径；源码注释中"任务 N（plan v2 §3）"类引用
    因收束摘要保留任务号映射而继续有效，不动。

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
  CI 五 job
  （`build` 编译+单测门禁 / `lockfile` 新鲜度 / `runtime-smoke` 104 断言 /
  `phase0b-acceptance` 30 断言 / `lint` fmt+clippy 静态门禁〔§0-9〕）。
- 验收关键数据（详见验收报告）：双线路 50 窗口压测 0 漏检 0 回写 0 写失败；UIA 证实
  线路一每窗口独立按钮、线路二 50 窗合并单组、还原回原生；工作集 9.15 MB；explorer
  文件夹窗口会被回写（已知限制）；Edge 改写后短时不回写。

## §2 路线与边界

- **主线**：路线 B+ 双线路（零注入）。只用公开文档 API
  （`SHGetPropertyStoreForWindow` + `PKEY_AppUserModel_ID` + `SetWinEventHook`
  out-of-context）。默认 `watch` 即"取消分组"；自定义分组经 `--strategy group`。
- **红线**（继承 AGENTS.md，2026-10-02 决策 8 改写，原决策 6"仅允许存在于
  `inj/` 工作区成员"条款作废）：**注入路线代码禁止存在于本仓库**——注入版
  已按 §0-8 整体删除（git 历史可溯），仓库为纯 tbg-lite 单包，主包
  （`src/`）保持零注入；若未来重启路线 A，须先重新立项（GPL 清室红线
  见 §6-1 依旧适用；复用基准 = git 历史 + §5 技术结论）；禁本地编译；
  验证失败最多修 3 轮。禁止 `SetWindowsHookEx` 跨进程注入与 inline
  hook / 内存补丁（无清室实现依据，仍禁）。
- **已知限制**（验收报告 §3/§5）：Explorer 文件夹窗口回写 AUMID；UWP/Chromium 长时
  行为未验证；新窗口竞态（先并组后跳变）待真机感知评估；任务栏图标/跳转列表纠偏无
  （B+ 无翻译层钩子，属路线 A 能力）。

## §3 任务清单（已收束归档，§0-10）

> **状态（2026-10-03，决策 10）**：任务 13–37 全部完成并验证，明细
> （需求背景、实现细节、修复轮与 run ID 证据）整体归档 git 历史——本节
> 被替换前的最后版本为 commit 489fabf；每任务一提交，逐任务记录见
> git log。任务 0–11 属 v1 / Phase 0b 阶段（v1 全文存档 commit 8e09b0e，
> 验收见 `docs/phase0b-acceptance.md`）；任务 12 = 本计划 v2 重写。
> **新任务号自 38 起**：立项 = §0 新决策条目 + 本节新开 Phase 小节，
> 提交格式沿用 `feat(模块): 任务号-标题`。

| Phase | 任务 | 内容 | 状态 |
|---|---|---|---|
| 1 默认行为闭环 | 13–15 | 启动扫存量 / 交互菜单（取代 Ctrl+C 方案）/ 多应用覆盖矩阵（用户应用 7/7，裁决 B+ 持续为主） | 完成 |
| 2 固定磁贴 | 16–18 | `pin` 生成共享 AUMID 的 `.lnk` / `--to-taskbar` 固定 + `unpin` 防误删 / 磁贴 × 运行窗口 UIA 联动验收 | 完成 |
| 3 常驻与分发 | 19–21 | `install`/`uninstall`/`status` 自启三命令 / explorer 重启重扫 + 熔断 + 环形日志 / README + MIT + Release 流程 | 完成 |
| R 审计修复 | 22–26 | 15 BUG + 5 SEC 全闭合（原子写 / 单实例互斥 / 标记校验 / actions 钉 SHA） | 完成 |
| 维护者反馈批 | 27–31 | dev 滚动发布 / Explorer 回写 reassert / 菜单双语 / `[6]` Windhawk 协同引导 / 退出保活 `k` | 完成 |
| S 杀软误报治理 | 32 | VERSIONINFO + 应用清单嵌入（SxS 14001 两轮教训 → 自管 RC）+ README 验真与上报口径 | 完成 |
| A 注入版双轨 | 33–37 | 清室 IAT 重定向实现 + CI 双产物——已随 §0-8 删除，技术结论留档 §5 | 完成→已删 |

勘误（归档时修正）：任务 22 在归档前版本的复选框为漏勾（`- [ ]`），
实际已闭合——Phase R 验收结论（15 BUG + 5 SEC 全闭合，P0 = 22/22b）
与 AGENTS.md 审计修复段均有记载。

口径存续（当前值见 §4）：runtime-smoke 断言 104、phase0b-acceptance
断言 30、单元测试 47 项；每任务一提交；验证失败修复轮上限 3。

## §4 验收与测试（常态化）

| 层面 | 方法 |
|---|---|
| 编译门禁 | CI `build`（windows-latest，`cargo build --release --locked` + `cargo test --locked`，任务 22b/23） |
| 静态门禁 | CI `lint`（`cargo fmt --all --check` + `cargo clippy --locked --workspace --all-targets -- -D warnings`，§0-9，审计 P2-14 收口） |
| 双线路行为回归 | CI `runtime-smoke`（104 断言，含启动扫存量、交互菜单、自启 Phase I、pin Phase P、固定/取消固定 Phase T、磁贴联动 Phase L、常驻加固 Phase X；原注入版 Phase INJ 27 断言已随注入版按 §0-8 移除）+ `phase0b-acceptance`（30 断言），每次 push |
| 固定磁贴联动 | CI 断言（任务 18，Phase L：.lnk AUMID == 运行窗口 AUMID + UIA 合并按钮，已绿） |
| 真机清单 | 竞态感知率、覆盖矩阵全量、长时回写、视觉细节、explorer 重启（验收报告 §5） |
| 内存/体积 | 验收报告口径；发布前回填 `BENCHMARK.md`（任务 21） |

## §5 注入版路线 A（Phase A，2026-09-28 重启实现——原"不实现仅存档"作废）

> **状态（2026-10-02，决策 8）**：注入版已整体删除（`inj/` 代码、Phase INJ
> 门禁、双 zip 发布通道）。本节保留为**技术结论与证据链档案**——限制⑤等
> 结论对未来任何路线 A 重启仍有约束力；实现细节以 git 历史为基准。

- **决策**：维护者 2026-09-28 指令"生成 2 个 exe 版本"（§0-6）。原触发条件
  （B+ 覆盖 <90% / 竞态 ≥5%）与 v1 参考架构（符号链路）不再适用。
- **清室架构**（与 v1/Windhawk 符号钩子路线的实质差异）：不解析任何私有符号、
  不做 inline hook / 内存补丁——宿主 `tbg-inject.exe` 经
  `CreateRemoteThread + LoadLibraryW` 把 `tbg_hook.dll` 装入 explorer，
  再远程调用其导出 `tbg_hook_init`；DLL 遍历本进程模块导入表，将
  `shell32.dll!SHGetPropertyStoreForWindow` 的 **IAT 槽位**重定向到自有
  桩函数；桩函数调用原函数后，把返回的 `IPropertyStore` 包进**委托对象**
  （COM 聚合语义的最小实现），仅对 `PKEY_AppUserModel_ID` 的
  `GetValue` 注入线路语义（线路一 `~TBG~w<HWND>` 后缀 / 线路二
  `TBG.Group.<NAME>` 共享值），其余键原样透传。任务栏在进程内读到改写后
  的 AUMID——**零属性回写竞争**（B+ 的已知竞态在 A 中结构性消失）。
- **立项评审三项**（决策 6）：GPL——零 Windhawk/v1 引用，公开文档 API 直译，
  MIT 不传染（§6-1 前提重评通过）；杀软——注入版预期报毒（真实行为特征），
  独立 zip 通道 + README 风险声明 + 验真口径；维护成本——导入名解析无符号
  依赖，Windows 更新零偏移适配。
- **已知限制**（Phase A 立项时点）：① 若任务栏读 AUMID 不经
  `SHGetPropertyStoreForWindow` 导入（运行时 GetProcAddress / delay-load /
  内部直读窗口属性），IAT 槽位补丁数与拦截计数将为 0——Phase INJ 断言
  直接暴露该情形，后续以诊断证据决定二级路线（如 GetPropW 观测）；
  ② 卸载竞态：摘钩后 1.5 s 宽限再 FreeLibrary，理论上存在在途调用窗口
  （业界同类工具普遍常驻规避，本版选择完整卸载 + 宽限，残余风险文档化）；
  ③ 注入版 stub 在 explorer 进程内运行，其代码缺陷可能拖垮 explorer
  （CI 门禁 + 最小化 stub 逻辑缓解）；④ 不做 explorer 重启自动重注入
  （宿主非常驻；重启后需手动再 inject，Phase INJ 断言该路径可用）。
- **已知限制⑤（2026-09-30 实测确立，任务 36 收尾）**：Win11 任务栏的
  分组 AUMID 读取**不经** `SHGetPropertyStoreForWindow`。证据链：三层
  拦截面全开（静态 IAT 5 槽 + GPA 213 槽 + delay-load），任务栏确实
  经钩子取到代理（calls=2/wrapped=2）但对每个存储只读一次
  `System.Taskbar.TabList`（fmtid 57086C23-…-8F47F pid=3）即 Release，
  从未读 PKEY_AppUserModel_ID（run 36653273590 插桩）；重启 explorer
  后**先注入再开窗**（排除任何指针缓存）按钮仍合并（run 36654985869
  决定性实验）；外部 AUMID 写入可改变分组（Phase L 证明读取方存在，
  但走 WinRT/CTaskBand 内部管线）；社区参考实现（Windhawk
  taskbar-grouping，m417z）以 hook Taskbar.dll 私有符号达成同效——
  本项目 §5 红线禁止。处置：注入版基础设施（注入/摘钩/共享节生命周期/
  重注入）端到端验证为绿；分组拦截在 Win11 定位为不可达，Phase INJ
  断言转为稳定性性质（钩子激活期间原生分组不受扰动）+ 证据日志，
  兼作未来 Windows 若改走文档化调用时的金丝雀。
  **扩展（2026-10-01，经典任务栏腿 run 36805691251）**：Server 2022
  经典任务栏（Win10 式 shell）同样如此——calls=2/wrapped=2（确实经
  钩子取存储）但代理只服务 `System.Taskbar.TabList` 读取、注入后
  新开窗口 aumid-served 仍为 0。结论：限制⑤是**结构性**的（两代
  任务栏一致），分组 AUMID 走窗口原子属性快速路径（GetPropW 原子，
  PKEY 的底层存储），不经 COM 属性存储的 GetValue。若要路线 A 真正
  达成分组改写，拦截面需扩展到 `user32!GetPropW`（explorer 内高频
  热路径，风险显著上升）——属架构决策，留维护者裁决。经典腿 CI
  断言同 Win11 腿改为稳定性 + 金丝雀；另记录：windows-2022 镜像
  UIA 树不暴露任务栏按钮（计数断言降级为证据性，总按钮数样本已
  入日志）。后续可选探针：注入状态下为窗口显式写 AUMID（tbg-lite
  mark 一次性写入）再观察 aumid-served——判定显式 AUMID 窗口是否
  走 COM 读（区分"无 AUMID 故不读"与"恒走原子路径"）。
  **范围决议（§0-7，2026-10-01 维护者指令）**：本项目仅支持 Win11
  （Win10 用户有其他成熟稳定工具）。据此：经典任务栏 CI 腿移除、
  GetPropW 拦截面扩展探针**不再 pursue**（该路径只服务经典任务栏
  场景的验证收益，Win11 上限制⑤结论不受影响）；限制⑤证据链与
  脚本留档，Win11 腿（runtime-smoke.ps1 Phase INJ）继续作为门禁
  与未来金丝雀。（§0-8 后记：该门禁与金丝雀已随注入版删除一并
  退役——2026-10-02。）
- v1 原文（符号链路架构、预算表、mod 拆解）仍存档于 git 历史 commit
  8e09b0e，仅供历史参考，**禁止实现引用**（GPL 红线）。

## §6 开放问题（2026-09-23 任务 21 收尾决议）

1. **已决**：LICENSE = MIT（任务 21；零注入路线未引用 mod 逻辑代码，GPL 不
   传染；若未来引用 mod 逻辑描述再重评）。**2026-09-28 复核（决策 6/任务
   33）**：注入版重启后此结论仍成立——Phase A 为清室设计（§5），未引用
   Windhawk mod 逻辑、v1 符号表或任何 GPL 代码；后续注入相关改动维持
   零引用红线，若需参考 mod 行为须先重评许可。
2. **维持不需要**：配置文件（全 CLI 参数 + 无排除列表决策不变；覆盖矩阵
   回填后唯一回写行为 shell 自管 Explorer 文件夹窗口，属路线边界非配置
   可解——plan v2 §2 已知限制）。
3. **已决**：宿主常驻（任务 20 落地：常驻 + explorer 重启重扫 + 熔断；
   对应 v1 §9-3 的倾向性判断成为实现事实）。
