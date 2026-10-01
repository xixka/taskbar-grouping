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
- **红线**（继承 AGENTS.md，2026-09-28 决策 6 改写）：**注入路线代码仅允许存在于
  `inj/` 工作区成员**（tbg-proto / tbg-hook / tbg-inject，含 DLL 注入与 IAT
  重定向），`tbg-lite` 主包（`src/`）保持零注入；注入版不提供自启注册、仅用户
  显式启动；注入实现禁止引用 Windhawk mod 代码或 v1 私有符号表（GPL 清室
  红线，见 §6-1）；禁本地编译；验证失败最多修 3 轮。禁止
  `SetWindowsHookEx` 跨进程注入与 inline hook / 内存补丁（本版技术选型为
  IAT 重定向，见 §5——其余注入机制无清室实现依据，仍禁）。
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
- [x] **任务 29**（维护者 2026-09-24 真机反馈：菜单需要中英文）：
      交互菜单文案双语（en/zh）。语言 = `GetUserDefaultUILanguage` 主语言
      ID 0x04（中文，含 zh-CN/zh-TW/zh-HK）自动检测，默认英文；菜单内
      `L` 键随时切换（会话级，不落盘——§6-2 配置维持不需要）。范围：
      仅菜单层字符串；watch / inspect / restore 技术输出保持英文（CI
      断言与文档口径）。兼容性：en-US CI Runner 走 EN 分支，Phase M
      菜单流与 `interactive menu` 断言不变；`L` 键 CI 脚本不发送，stdin
      序列对齐不被扰动（ZH 横幅保留英文子串 `interactive menu` 双保险）。
      Cargo feature +`Win32_Globalization`（feature 增改不影响 lockfile）。
- [x] **任务 30**（维护者 2026-09-24 真机反馈：菜单无注入选择项）：
      菜单 `[6]` 注入路线入口——**信息 + 协同引导，零注入代码**（§5
      红线不破：路线 A 仍为存档备用，不实现）。内容：路线 A 现状说明
      （符号钩子逐版本维护 / 杀软误报 / GPL 风险）、本机 Windhawk 安装
      检测（ProgramFiles 与 LOCALAPPDATA 两处常见布局，纯文件系统探测
      无新依赖）、与 tbg 的冲突规则（watch 运行中先 [3] 停止 + [4]
      还原，README Coexistence 同口径）、操作步骤（Windhawk → Explore
      mods 搜 taskbar group）与 Win11 23H2+ 原生"永不合并"备注。CI
      兼容：`6` 键 Phase M 脚本不发送，stdin 序列对齐不变。
- [x] **任务 31**（维护者 2026-09-24 真机反馈：菜单退出后效果丢失）：
      退出保活——`[0]` 退出确认新增 `k` 选项：先优雅停止本进程 watch
      （互斥体随线程 Drop 释放），再以 `std::process::Command`
      （CREATE_NO_WINDOW + CREATE_NEW_PROCESS_GROUP，分离不随父进程
      退出）重启 `watch --duration 0` 子进程接续监听——新窗口继续被
      标记、Explorer 回写继续被任务 28 补写。顺序红线：必须先停后启
      （group 线路 `Local\tbg-lite.map` 互斥体，审计 BUG-02）。stdio
      显式导向 `%LOCALAPPDATA%\tbg-lite\tbg-background.log`（append，
      打不开回退 NUL）——CREATE_NO_WINDOW 隐式 stdout 为无效句柄，
      println! 写失败会 panic 杀死后台进程。y/n 原语义不变（CI M1 发
      `n` 走原路径）；`k` 键 CI 不发送，stdin 对齐不变。开机延续仍走
      `install`（任务 19），本项只解决"退出即失效"。

### Phase R 验收结论（2026-09-22）

审计报告全部 15 项 BUG + 5 项 SEC 闭合：P0（22/22b）、P1（23/24/25）、
CI 加固（26）全部 CI 绿灯；新增 31 项单元测试纳入 build job 门禁；
审计 P2 中的工程建议（错误类型化 thiserror、stdout/stderr 分流、
DRY 合并、MSRV/license 字段）未纳入本轮（非漏洞项，随后续任务演进）。

### Phase S — 杀软误报治理（2026-09-28 维护者指令：解决卡巴斯基报毒）

- [x] **任务 32**：Kaspersky 等启发式引擎误报治理（产物元数据 + 上报路径）。
      背景：发布物为"单文件 + strip + 未签名"小 exe，行为面天然贴近启发式
      关注区（跨进程窗口 AUMID 改写 / 全局 SetWinEventHook / HKCU Run 自启 /
      任务栏磁贴固定，均为公开文档 API），Kaspersky 会对此类"无版本信息 +
      无清单 + 无签名"组合给出非特征性 generic 误报（UDS / PDM / Heur 类）。
      落地（CI 编译验证，遵守禁止本地编译红线；每项独立提交）：
      ① `build.rs`（winresource 0.1.31，default-features=false 仅引入
      version_check 一个构建依赖，lockfile 增量最小）构建期嵌入
      VERSIONINFO（描述/版本/版权/项目主页）与应用清单（asInvoker +
      Win10/11 supportedOS）——纯资源注入、零运行时行为变化，非 Windows
      目标直接返回；MSVC 目标经宿主 Windows SDK 的 rc.exe 编译 .res
      （windows-latest 自带，无需安装）；
      ② README 新增 "Antivirus false positives" 章节：误报成因、
      SHA256SUMS + build-provenance 验真口径、Kaspersky OpenTip
      （https://opentip.kaspersky.com/ ）误报上报路径；
      ③ 资源体积增量约 1-2 KB（150 KB 实测基线，预算 0.5 MB 线内；
      正式发布时由 release 工作流回填实测值）。遗留：代码签名证书可
      根治此类告警（暂无赞助）；Kaspersky 库内白名单需维护者经
      OpenTip 提交确认，属仓库外流程。
      修复轮（run 36372461818 → 36373209674，两轮均败于同因）：winresource
      自动生成的 RC（强制 `#pragma code_page(65001)`；清单先走 RC 字符串
      块、再走转义路径文件直嵌）两轮均使加载器报 SxS 14001（side-by-side
      configuration is incorrect，三 job 同因失败；lockfile 门禁两轮均
      绿——手工锁文件与 cargo generate-lockfile 输出一致）。终案对齐
      alacritty 成熟方案：`set_resource_file` 整体自管 RC——`tbg-lite.rc`
      入库（纯 ASCII、无 code-page pragma、BEGIN/END 块 + 相对路径引用
      清单），winresource 仅负责定位 rc.exe 与挂接 .res；清单同步把
      supportedOS 属性名修正为微软规范 `Id`（原误写 Guid）。教训记入
      build.rs 头注。

### Phase A — 注入版双轨（2026-09-28 维护者指令：生成注入/非注入两个 exe）

- [x] **任务 33**（治理先行）：路线 A 重启的决策记录（§0-6）+ 红线改写（§2 /
  AGENTS.md 同步）+ 立项评审三项结论落档（§5 重写 + §6-1 GPL 复核）。
      注入版边界同时定型：`inj/` 工作区成员独立成包；不提供 `install`
      自启（与 B+ 差异化，AV 行为面收敛）；无还原表（不写真实属性，
      摘钩即回原状）；与 tbg-lite watch 互斥运行（README 口径）。
- [x] **任务 34**：`inj/tbg-proto`（双进程共享内存协议 crate）+
      `inj/tbg-hook`（cdylib：DllMain 空载 + 导出 init/stop + IAT 重定向
      `SHGetPropertyStoreForWindow` + COM 委托 IPropertyStore（PKEY_
      AppUserModel_ID 改写）+ 应用窗口过滤复刻 + 统计上报）。工作区化
      Cargo.toml + 锁文件同笔更新 + CI `--workspace` 化。
      完成：18954d0 + 修复 3 轮（bad593e / ffd9dbc / 9fe3374，run
      36513267646 四门禁全绿）。修复教训（无本地编译器时代的高价值
      核验法）：windows 0.58 以 crates.io 官方源码核对调用面——E_\*/
      S_OK 在 `Win32::Foundation`（非 core）、PROPERTYKEY 在
      PropertiesSystem、WriteProcessMemory 在 Diagnostics::Debug、
      FreeLibrary 在 Foundation、MapViewOfFile/UnmapViewOfFile 返回
      视图结构体、OpenFileMappingW 首参裸 u32、IID 关联常量需
      `Interface` trait 入作用域；IPropertyStore vtable 次序实为
      IUnknown(3) + GetCount/GetAt/GetValue/SetValue/Commit——首版漏
      GetCount/GetAt 两槽（编译可过但运行时错位跳转），经生成源码
      对照补全。
- [x] **任务 35**：`inj/tbg-inject` 注入宿主（explorer PID 定位 + 远程
      LoadLibraryW/init/stop 三段式 + 共享内存宿主侧 + `inject`/`stop`/
      `status` CLI + 双语交互菜单）。完成：3ceb036 + 同上 3 轮修复；
      基址定位走"本地 RVA + 远程 Toolhelp 模块基址"规避 32 位线程
      退出码截断。
- [x] **任务 36**：CI 双产物流水线——build/lockfile/runtime-smoke/
      phase0b 四门禁 workspace 化；runtime-smoke 新增 Phase INJ（注入版
      端到端 27 断言：基线合并 → 注入/状态/摘钩/复原 → explorer 重启
      重注入 → 早期注入决定性实验）；dev/release 双 zip（tbg-lite 单
      exe / tbg-inject exe+dll）+ 双 attestation。完成：929fc87 +
      行为修复 8 轮（87a57d4 … 14ab280，详见 §5 已知限制⑤的证据链）。
      修复链产出：API Set 导入名修复、单侧小写匹配根因修复、DLL 持
      共享节句柄（名字随句柄释放而非随对象死亡）、GPA 层常开、
      IPropertyStoreCache 代理扩展、proto v3 代理行为插桩、早期注入
      决定性实验。
      **修复轮 9（2026-09-30 真机反馈，DLL 无法卸载）**：旧版宿主
      `call_remote_export` 对每次导出调用（含 stop）都先发一次远程
      `LoadLibraryW`——stop 时引用计数先 +1、`FreeLibraryAndExitThread`
      只 -1，DLL 永久驻留 explorer（文件锁死、"stop: ok" 假象）；CI 旧
      断言只查退出码不查模块表，故漏网。三处修复：① 装载条件化（快照
      探测已装载实例即复用，绝不叠加引用）；② `tbg_hook_stop` 语义
      重写（任何调用都以 FreeLibraryAndExitThread 收尾：ACTIVE=完整
      拆钩 / 残留实例=仅卸载自愈 / INITING+STOPPING=并发保护），并补
      旧版缺失的节视图 UnmapViewOfFile（每周期漏一个无名视图）；③
      宿主 do_stop 残留实例自愈路径 + 卸载真实性验证（轮询模块表，
      旧版 DLL 无法自卸载时如实报错并指引重启 explorer）。CI 盲区
      双补：Win11 腿（runtime-smoke.ps1）stop 后断言 DLL 离开模块表
      + 二次 stop 必须报 not injected；新增经典任务栏腿
      `runtime-smoke-inj`（windows-2022，continue-on-error 观察模式）
      ——路线 A 唯一可能端到端生效的环境（经典任务栏分组 AUMID 预期
      仍走属性存储），断言 aumid-served≥1 / 注入后按钮分离 / 真实
      卸载。
- [x] **任务 37**：双版本文档补全——README 注入版章节（用法/风险/杀软
      预期/验真）、BENCHMARK 注入版行（CI 回填）、AGENTS 目录导览与构建
      命令同步、plan §3/§4 回填关闭。完成：本轮一并提交（README/AGENTS/
      plan + Phase INJ 断言语义对齐已知限制⑤）。

## §4 验收与测试（常态化）

| 层面 | 方法 |
|---|---|
| 编译门禁 | CI `build`（windows-latest，`cargo build --release --locked` + `cargo test --locked`，任务 22b/23） |
| 双线路行为回归 | CI `runtime-smoke`（131 断言，含启动扫存量、交互菜单、自启 Phase I、pin Phase P、固定/取消固定 Phase T、磁贴联动 Phase L、常驻加固 Phase X 与注入版 Phase INJ 端到端 27 断言）+ `phase0b-acceptance`（30 断言），每次 push |
| 固定磁贴联动 | CI 断言（任务 18，Phase L：.lnk AUMID == 运行窗口 AUMID + UIA 合并按钮，已绿） |
| 真机清单 | 竞态感知率、覆盖矩阵全量、长时回写、视觉细节、explorer 重启（验收报告 §5） |
| 内存/体积 | 验收报告口径；发布前回填 `BENCHMARK.md`（任务 21） |

## §5 注入版路线 A（Phase A，2026-09-28 重启实现——原"不实现仅存档"作废）

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
  兼作未来 Windows 若改走文档化调用时的金丝雀。Win10 表现未测
  （CI 仅 Win11）。
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
