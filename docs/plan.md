# taskbar-grouping 独立 Rust 版本（小型 exe）可行性评估与实施计划

> 评估对象：[Windhawk mod: taskbar-grouping](https://windhawk.net/mods/taskbar-grouping)（"Disable grouping on the taskbar"）
> 参考仓库：[gianluca-schwekendiek/taskbar-grouping](https://github.com/gianluca-schwekendiek/taskbar-grouping)（TaskbarFolders）、[DenisGeide/ShelfyGAI](https://github.com/DenisGeide/ShelfyGAI)
> 日期：2026-09-21
> 结论速览：**技术上可行**，Rust 工具链齐全；完整复刻属"高难度、需持续维护"路线；两个参考仓库**都刻意避开了注入路线**，只能提供部分外围设计参考。建议先做 Phase 0 PoC 再决定是否全量投入。
> 补充调研（2026-09-21 15:18）：GitHub 检索 `taskbar-grouping` 全部 11 项结果逐项核对完毕，详见**附录 B**。关键新发现：免注入修改运行中窗口分组的**公开文档 API**（`SHGetPropertyStoreForWindow` + `PKEY_AppUserModel_ID`）有活跃开源实锤（aumid-stopgap-tools，Win11 实测可用），据此新增**路线 B+（属性存储 AUMID 监听，零注入）**作为低成本 MVP 候选，与路线 A 双轨 PoC 后裁决。

---

## 1. 结论（TL;DR）

| 问题 | 答案 |
|---|---|
| 能否用 Rust 做独立小 exe 复刻该 mod？ | **能**。mod 本体是 ~2000 行 C++，逻辑可移植；Rust 有等价基础设施（inline hook、PDB 解析、DLL 注入均有成熟 crate） |
| 体积能否做小？ | **能**。估算 hook DLL 0.3–0.8 MB + 宿主 exe 0.8–2 MB（见 §6 预算表，均为估算，Phase 0 实测） |
| 内存能否做小？ | **能**。常驻开销可压缩到"explorer 进程内 +约 1–2 MB，宿主进程 1.5–4 MB（可退出）"，远低于 Windhawk 全家桶（主程序 UI + 注入引擎，数十 MB 量级，以实测为准） |
| 主要代价是什么？ | ① 需要自己复刻 Windhawk 的"符号下载 + inline hook + 注入"三层基础设施；② 微软每次功能更新都可能改 explorer 内部类，**维护责任从 Windhawk 社区转移到自己身上**；③ 注入 explorer 有崩溃带崩任务栏的风险；④ mod 源码为 **GPL-3.0**，移植版属于衍生作品，分发需开源 |
| 两个参考仓库有没有参考价值？ | **TaskbarFolders：部分有价值**（外置 AUMID 分组方案、COM/快捷方式交互模式、架构文档）；**ShelfyGAI：价值有限**（窗口样式整理思路、恢复状态持久化、安全护栏设计）。**两者都没有可复用的注入/hook 代码**，都明确声明"不 hook Explorer" |
| GitHub 全景（`taskbar-grouping`，11 项，附录 B） | **无人在开源世界复刻过注入路线**（除 Windhawk mod 本体外零实现）；但发现免注入实锤：`SHGetPropertyStoreForWindow`+`PKEY_AppUserModel_ID`（公开文档 API）可外部改运行中窗口的分组归属——`tksh164/aumid-stopgap-tools`（27★，MIT，Win11 最新版实测）与 `mihaicuibus/taskbar-grouping`（Win7 时代 JNI 样例）先后验证。据此新增**路线 B+**：事件驱动改写 AUMID，零注入实现"取消分组"，见 §4 与附录 B |

---

## 2. 调研结论

### 2.1 Windhawk taskbar-grouping 模块本体

**基本信息**（来源：模块页面 + `ramensoftware/windhawk-mods` 仓库 `mods/taskbar-grouping.wh.cpp`，v1.3.10，作者 m417z，GPL-3.0）：

- 注入目标：**仅 `explorer.exe`**（x86-64），支持 Win10 64-bit / Win11（含 24H2+ 分支）/ Win11 上经 ExplorerPatcher 还原的旧任务栏。
- 源码规模：约 2000 行 C++，单文件。

**实现机制拆解**（这是评估的核心）：

1. **符号钩子（约 32 个）**：钩住 explorer 内部**未导出**的 C++ 方法——`CTaskGroup`、`CTaskBand`、`CTaskListWnd`、`CTaskBtnGroup` 的 `GetAppID / SetAppID / GetFlags / DoesWindowMatch / _HandleItemResolved / _MatchWindow / GetLauncherName / TaskDestroyed / _CreateTBGroup` 等。这些函数没有导出表条目，地址只能靠 **PDB 符号**解析——Windhawk 引擎自动从微软符号服务器（msdl.microsoft.com）下载 PDB、缓存、按"未修饰签名"匹配地址。
2. **核心技巧**：窗口被任务栏"解析"（`_HandleItemResolved`）时，给它的 AppUserModelID 追加后缀 `~Wh~w<HWND十六进制>`，让每个窗口落到独立的任务栏组 = **取消分组**；自定义分组则把成员的 AppID 改写成 `Windhawk_Group_<n>` 前缀。
3. **纠偏钩子**：AppID 被改后，图标、跳转列表、启动、固定、排序全会出问题，于是再挂一串"翻译层"钩子把带后缀的 AppID 映射回原 AppID（`GetAppID / GetShortcutIDList / GetIconResource / Launch / _GetJumpViewParams` 等）。
4. **导出 API 钩子（4 个）**：`kernelbase!LoadLibraryExW`（侦测 ExplorerPatcher 加载）、`kernelbase!CompareStringOrdinal`（让 AppID 匹配忽略后缀，用线程 ID 门控限定作用域）、`comctl32!DPA_InsertPtr / DPA_DeletePtr`（控制任务栏按钮插入位置，实现"同类挨着放"与固定项交换）。
5. **多版本适配**：符号名/签名在 Win10、Win11、Win11 24H2、旧任务栏四个分支各不相同，源码用 `optional` 钩子 + 版本探测处理；ExplorerPatcher 路径直接用它导出的修饰名 `GetProcAddress`。

> 含义：**mod 本体只占工作量的 40% 左右，另外 60% 是 Windhawk 替你做掉的基础设施**——注入、符号下载/缓存/匹配、hook 引擎、设置热更新、进程重启再注入。独立 Rust 版必须自己实现这 60%。

### 2.2 参考仓库一：gianluca-schwekendiek/taskbar-grouping（TaskbarFolders）

（注：你提供的 URL 用户名拼写有误，实际为 `gianluca-schwekendiek`，已按该仓库评估。）

- **是什么**：C# 12 / .NET 8 / WPF 的"macOS 风格任务栏应用文件夹"——把若干 app 收进一个可固定的磁贴，点击弹出图标扇出面板。
- **技术路线**：**完全不 hook、不注入**。每个组生成一个 `.lnk`（独立 AppUserModelID `TaskbarFolders.Group.<id>`，经 `IPropertyStore` 打入）+ 共享 `Launcher.exe --group-id` + 合成图标；依赖 Windows 把不同 AUMID 当作不同应用，从而并排为独立磁贴。
- **对 Rust 版的参考价值**：
  - ✅ **"自定义分组"功能的免注入实现范本**：AUMID + `IShellLinkW` + `IPropertyStore` + `TaskbarManager` 请求固定的完整模式（C# interop 可直接翻译成 `windows-rs` 调用）；
  - ✅ 架构文档（ADR：为什么每组一个 .lnk、为什么共享 launcher）可作设计输入；
  - ❌ 它**做不了"取消分组/每窗口一按钮"**——运行中窗口的分组归属由 explorer 内部决定，外部程序无法干预（这正是 Windhawk mod 用注入的原因）；
  - ❌ 技术栈（WPF/.NET 自包含）与"小体积低内存"目标背道而驰，工程上仅借设计不借实现。

### 2.3 参考仓库二：DenisGeide/ShelfyGAI

- **是什么**：Python 编写的窗口整理器（隐藏/置顶/分组/恢复），PyInstaller 打包。
- **技术路线**：**同样不注入**。用 `SetWindowPos` + 窗口扩展样式（摘 `WS_EX_APPWINDOW`、加 `WS_EX_TOOLWINDOW`）把窗口从任务栏/Alt+Tab 隐藏，自己画 overlay hub 呈现"分组"；恢复时写回原样式。README 明确写道："原生 Windows 任务栏分组未实现，因为需要不安全的 shell 级修改"。
- **对 Rust 版的参考价值**：
  - ✅ **反向佐证**：第三方作者调研后得出同样结论——不碰 explorer 内部就做不了真正的任务栏分组，支持"完整复刻必须走注入路线"的判断；
  - ✅ 安全护栏设计值得抄：隐藏前保存原始样式、`recovery.json` 持久化恢复状态、退出时自动还原、忽略陈旧 HWND——这些模式对 hook DLL 的"退出时卸载钩子并恢复原状"同样适用；
  - ❌ 无注入/hook 代码可参考；其"隐藏按钮"只是变相整理，不是分组控制。

### 2.4 顺带确认的相关事实

- 同作者的 **7+ Taskbar Tweaker** 是同类前辈：闭源、Win10、同样注入 explorer（用硬编码偏移而非符号）——证明"脱离 Windhawk 独立分发该功能"在产品上成立过，但其不支持 Win11。
- mod 仅注入 explorer 一个进程，且所有钩子都在 explorer 的 UI 线程上跑（C++ 源码大量用 thread-id 门控），Rust 移植时必须保持同样的线程纪律。

---

## 3. 可行性评估

### 3.1 Rust 技术栈覆盖度

| 需要的能力 | Windhawk 里的对应物 | Rust 方案 | 成熟度 |
|---|---|---|---|
| x64 inline hook | `Wh_SetFunctionHook`（Minhook 系） | `retour` crate（静态/动态 detour），或 `minhook` 绑定 | 高，久经使用 |
| DLL 注入 | Windhawk 注入引擎（进程创建期 + 已存在进程） | 手写 `OpenProcess + VirtualAllocEx + WriteProcessMemory + CreateRemoteThread + LoadLibraryW`（~100 行，`windows` crate） | 高，教科书级 |
| PDB 解析 | Windhawk 符号引擎 | `pdb` crate（纯 Rust）或 `symbolic-debuginfo`（含 MSVC 反修饰）；匹配可直接用修饰名（mod 的 ExplorerPatcher 分支已示范该做法） | 中高 |
| 拿到 PDB 的 GUID/Age（下载 URL 参数） | 同上 | 读 PE 调试目录 CodeView 记录：`object` crate | 高 |
| 符号下载 | 引擎内置（msdl.microsoft.com，symsrv 协议，本地缓存） | WinHTTP（走 `windows` crate，零体积代价）请求 `https://msdl.microsoft.com/download/symbols/<pdb>/<GUID><age>/<pdb>` | 高（纯 HTTP 逻辑） |
| explorer 重启后自动再注入 | Windhawk 常驻服务 | 宿主 exe 轻量轮询 explorer PID 变化（或 WTS 会话通知），变则重新注入 | 高 |
| 设置热更新 | Windhawk 设置 UI | `config.toml` + 文件监视（`notify` crate 或 FindFirstChangeNotification），改完广播给 DLL 重载 | 高 |

**覆盖度结论：无空白。** 每一层都有可用的 Rust 库或可手写的成熟模式。

### 3.2 功能保真度矩阵（关键决策依据）

| mod 功能 | 免注入可实现？ | 说明 |
|---|---|---|
| 取消分组（每个窗口一个按钮） | ⚠️ 实验性可达（路线 B+） | 最初判断为 ❌（分组决策在 explorer 内部，外部无入口）。**2026-09-21 更正**：附录 B 调研发现 `SHGetPropertyStoreForWindow` + `PKEY_AppUserModel_ID` 是公开文档 API，外部进程可直接改写运行中窗口的分组归属（aumid-stopgap-tools 在 Win11 实测）。做法：监听新顶层窗口 → 给其 AUMID 追加每窗口后缀。限制：新窗口存在"先按原组显示、属性生效后重排"的竞态（偶发按钮跳动）；UWP/Chromium 等自管 AUMID 的应用可能回写覆盖。保真度 80–90%，以 Phase 0b 实测定夺 |
| 自定义分组（指定程序合成一组，含运行中窗口） | ⚠️ 实验性可达（路线 B+） | 同上机制：给组内窗口统一改写为共享 AUMID 前缀；固定/启动行为依赖配套 `.lnk`（aumid-stopgap-tools 与 TaskbarFolders 的共同解法） |
| 自定义分组（仅固定磁贴层面，TaskbarFolders 式） | ✅ | .lnk + AUMID + 合成图标，TaskbarFolders 已验证可行 |
| 排除列表 / 反向模式 | ⚠️ 路线 B+ 同机制可达 | 属于 AUMID 改写规则的一部分，效果同样受竞态限制 |
| 固定项模式（replace / keepInPlace） | ❌（路线 B+ 只能近似） | 涉及 `DPA_InsertPtr/DeletePtr` 与固定项交换逻辑，B+ 只能靠 .lnk 固定顺序近似 |
| 退出/禁用时完全还原 | ✅（两条路线都须实现） | 路线 A：unhook + explorer 自动重排；路线 B+：清空自定义 AUMID（恢复原值）+ explorer 自动重排 |

> 修正后的结论：**"完全保真 + 零闪烁 + 全应用覆盖"仍必须走路线 A（注入 + 符号 hook）**；但"低成本、零注入、覆盖大多数 Win32 应用"的路线 B+（属性存储 AUMID 监听）是此前未识别的中间态，值得与路线 A 并行 PoC 比较后择优。

### 3.3 风险清单

| 风险 | 等级 | 缓解 |
|---|---|---|
| hook 代码 bug 直接带崩 explorer（任务栏消失） | 高 | ① Phase 0 先在测试机上验证单钩子稳定性；② 每个钩子入口做防御性校验（参数、线程）；③ 保持"原函数直通"为默认失败路径；④ 提供 watchdog：宿主进程监测 explorer 崩溃循环时自动停用并还原 |
| Windows 功能更新改内部类/签名 | 高（长期） | 符号缺失时**优雅降级**（mod 源码大量钩子本就 optional）；把符号表外置为数据文件，热更新适配而无需重发 exe；明确只承诺 N-1 个主流版本（如 Win11 24H2/25H2 + Win10 22H2） |
| 杀软/SmartScreen 拦截（向 explorer 注入未签名 DLL） | 中高 | 代码签名证书（预算项）；或文档引导加白；注入手法用最标准的 LoadLibrary 路径降低启发式误报 |
| GPL-3.0 传染 | 中 | mod 源码 GPL-3.0 → 移植版必须同样 GPL-3.0 开源（个人自用无碍，公开分发需遵守）。若不可接受，只能做 clean-room 行为级重写，工作量和法律审查成本上升；本计划默认**接受 GPL-3.0** |
| 32 个钩子的语义移植错误（尤其纠偏层） | 中 | 逐钩子带翻译注释移植；建立"功能验收清单"逐项过（见 §8） |
| PDB 下载依赖微软符号服务器 | 低 | 本地缓存 `%LOCALAPPDATA%`；离线时用缓存；可选参数指向内部符号服务器 |

---

## 4. 方案选型

**推荐：双轨 Phase 0 PoC（0a 注入路线 + 0b 属性存储路线，约 1–2 周），用实验裁决主线；默认预期主线为路线 A，路线 B+ 为低成本替代。**

- **路线 A — 注入复刻（高保真主线）**：宿主 `tbg-host.exe`（注入器 + 符号下载器 + watchdog，用完可退或常驻 ~2 MB）+ `tbg-hook.dll`（移植 mod 全部逻辑）。功能 100% 对齐，体积/内存目标可达成，代价是 §3.3 的风险与长期维护。
- **路线 B+ — 属性存储 AUMID 监听（新，零注入，附录 B 证据支撑）**：单一 `tbg-lite.exe`，`SetWinEventHook` 监听新顶层窗口 → `SHGetPropertyStoreForWindow` 按 `config.toml` 规则改写 `PKEY_AppUserModel_ID`（每窗口后缀 = 取消分组；共享前缀 = 自定义组；配套生成 `.lnk` 保障固定/启动/图标）。**只用公开文档 API：无注入、无符号依赖、无崩溃传导、Windows 更新几乎不影响**（API 自 Win7 稳定至今）。预计体积 <0.5 MB、常驻 <10 MB（可降到 ~2 MB 级别取决于事件吞吐）。已知短板：新窗口竞态（偶发按钮跳动/延迟归组）、自管 AUMID 应用（UWP/部分 Chromium 系）可能覆盖、固定项交互弱于 A。
- **路线 B（原免注入混合）— 降级为 B+ 的外围组件**：TaskbarFolders 式磁贴与 ShelfyGAI 式整理可按需叠加在 B+ 之上。
- **不建议**：直接改造/精简 Windhawk 本体（闭源引擎）、或用 C++ 抄 7+ Taskbar Tweaker 的硬编码偏移路线（Win11 无符号支持根本走不通，符号路线正是 mod 相对 7TT 的进步点）。

**裁决标准（Phase 0 结束时）**：若 0b 在目标机器上对用户常用应用集竞态不可感知且覆盖 ≥90% → 先发布 B+ 满足"小体积低内存"诉求，A 转为可选升级；若 0b 竞态/覆盖不可接受 → 全力走 A。

---

## 5. 目标架构（路线 A）

```
taskbar-grouping-rs/
├── tbg-host.exe            # 宿主：注入、符号下载、PDB 缓存、explorer 重启监视、配置热更新
├── tbg-hook.dll            # cdylib：32 个符号钩子 + 4 个导出 API 钩子 + 配置状态机（mod 逻辑的 Rust 移植）
├── config.toml             # pinnedItemsMode / placeUngroupedItemsTogether / useWindowIcons /
│                           # customGroups[] / excludedPrograms[] / groupingMode / oldTaskbarOnWin11
├── symbols.toml            # 外置符号表：每个 Windows 版本分支的 (签名/修饰名 → 钩子槽位)，支持热更新适配
└── %LOCALAPPDATA%\tbg\
    ├── pdb-cache\          # 从 msdl.microsoft.com 下载的 explorer.exe.pdb 等
    └── tbg.log             # 环形日志（默认关闭，排障开启）
```

**运行时序**：

1. host 启动 → 读取 `config.toml` → 读 explorer PE 调试目录得到 PDB GUID/Age → 命中缓存或 WinHTTP 下载 → `pdb`/`symbolic` 解析出 32 个符号地址（连同 `symbols.toml` 的版本分支选择）→ 生成"钩子安装清单"；
2. 注入 `tbg-hook.dll`（标准 LoadLibrary 远程线程），DLL `DllMain` 后由 host 通过共享内存/命名管道传入安装清单与配置；
3. DLL 内用 `retour` 安装钩子（全部 try 失败即直通原函数，不中断）；此后 host 可退出或以 ~2 MB 常驻监视 explorer PID 变化；
4. explorer 重启 → host 检测到新 PID → 重新走 1–3；
5. 配置变更 → host 写共享内存 → DLL 在安全时机（消息空闲钩子/下个钩子调用）原子换配置。

**关键实现纪律**（从 C++ 源码继承）：

- 所有钩子跑在 explorer UI 线程 → 钩子内**禁止阻塞、禁止跨线程锁**；沿用源码的 thread-id 门控模式（`compareStringOrdinalHookThreadId` 等全局原子量）；
- `static` 状态用 `static` + `AtomicU32`/`OnceLock`，避免 Rust 全局分配器在钩子热路径上抖动；
- 卸载顺序：先摘全部钩子（`retour` 的 unhook 需等待在飞调用）→ 释放 COM → `FreeLibraryAndExitThread`。

---

## 6. 体积与内存预算（估算，Phase 0 实测校准）

| 组件 | 预算（估算） | 手段 |
|---|---|---|
| `tbg-hook.dll` | 0.3–0.8 MB | `cdylib`，`opt-level="z"` + `lto="fat"` + `codegen-units=1` + `panic="abort"` + `strip`，`windows` crate 按特性裁剪，`retour` 是唯一大依赖 |
| `tbg-host.exe` | 0.8–2 MB | 同上；TLS 走 WinHTTP（避免 rustls ~1.5 MB）；PDB 解析选 `pdb` crate（比 `symbolic` 轻） |
| 磁盘总量（含空配置） | **< 3 MB**（Stretch 目标 1.5 MB） | — |
| DLL 注入后 explorer 私有内存增量 | ~1–2 MB（代码页 + 少量状态） | 对照法：注入前后 `explorer.exe` 提交大小差 |
| host 常驻（watchdog 模式） | 1.5–4 MB 工作集 | 单线程 + 轮询（2 s 一次 PID 查询，无 WMI） |
| host 退出模式总常驻 | **≈ 仅 explorer 内 +1–2 MB** | 代价：失去 explorer 重启自动恢复，需登录脚本/计划任务拉起 |

**与 Windhawk 对比口径**（诚实声明：以下为量级估计，`plan` 附实测方法，不作为承诺数字）：Windhawk 开销 = `windhawk.exe` 主程序（UI、引擎、编译服务）+ 注入到各目标进程的引擎 DLL；用户主观感受"几十 MB 量级"。独立版的本质优势是：**没有常驻 UI/编译器、只注入 explorer 一个进程、host 可退出**。

**实测方法（Phase 0 验收用）**：任务管理器"提交大小"与"工作集"、`RAMMap` 查私有内存、注入前后 explorer 差值取 5 次均值；体积用 Release 产物直测。

---

## 7. 实施计划（阶段与验收）

### Phase 0 — 双轨 PoC 门禁（1–2 周，可终止点）

目标：用最小代价验证高风险点，**0a 三项全绿且 0b 表现差 → 走 A；0b 达标 → 先发 B+；双双失败 → 退路线 B**。

**0a 注入路线（原计划）**：

1. **符号链路 PoC**：host 下载本机 explorer PDB → 解析出 `CTaskGroup::GetAppID` 等任一符号地址 → 与 `x64dbg`/WinDbg 人工核对一致。
2. **注入 + 单钩子 PoC**：注入 DLL，仅 hook `kernelbase!CompareStringOrdinal`（导出函数，最安全），带线程门控地打印日志，explorer 稳定运行 24 h 无崩溃。
3. **单内部钩子 PoC**：hook `CTaskGroup::GetAppID` 直通返回，验证 24 h 稳定 + unhook 干净。
4. **体积/内存基线**：测出两个组件的真实体积与内存增量，回填 §6 预算表。

**0b 属性存储路线（新增，预计 3–5 个工作日）**：

5. **API 语义 PoC**（~200 行 Rust）：手工对若干窗口调 `SHGetPropertyStoreForWindow` + `PKEY_AppUserModel_ID`：① 每窗口加后缀能否立即拆成独立按钮；② 属性写入后 explorer 是否即时重排（记录延迟毫秒数）；③ UWP/Chrome/Explorer 多窗口/记事本等 6 类应用逐一记录"生效/覆盖/无效"。
6. **事件驱动 PoC**（~300 行 Rust）：`SetWinEventHook`（EVENT_OBJECT_SHOW/CREATE，WINEVENT_SKIPOWNPROCESS）→ 自动改写新窗口 AUMID；高频连开 50 窗口压测，统计：竞态次数（先并组后跳变）、漏检数、CPU/内存占用。
7. **还原 PoC**：清除自定义 AUMID 后按钮是否完全恢复原生行为。
8. **双线路 CLI 切换**（2026-09-21 增补）：`watch --strategy ungroup|group`——线路一为既有"每窗口后缀"（取消分组，条目 6），线路二把全部新候选窗口统一改写为共享 AUMID `TBG.Group.<name>`（自定义分组，§4 路线 B+"共享前缀"分支）；两线路标记互斥、`--dry-run` 通用。线路二的原值落盘 exe 同目录 `tbg-restore.tsv`（附录 B.3 的 recovery 模式），`restore` 据此复原；无映射条目的孤儿窗口只报告、不动（防 HWND 复用误还原）。
9. **CI 运行时冒烟**（2026-09-21 增补）：GitHub Actions `runtime-smoke` job（windows-latest）在真实 Windows 会话内拉起 GUI 窗口（notepad 多开），对两条线路分别断言——线路一全部窗口带互异后缀、线路二全部窗口 AUMID 精确相等、`restore` 后逐窗复原；采集 watch/restore/inspect 日志与桌面截图（含 explorer/任务栏可行性探针）为工件。回答"Phase 0b 行为验收能否搬上 CI"（此前判断"必须真机人工"，windows-latest Runner 本身即真机，可先验证 API 行为层）。

**验收**：0b 的量化裁决线——竞态可感知率 <5%、应用覆盖 ≥90%、常驻内存 <10 MB 即视为达标。

### Phase 1 — 核心 MVP：取消分组（3–5 周）

范围（对齐 mod 默认行为，仅当前主版本 Windows）：

- [ ] 移植 `ProcessResolvedWindow`：AppID 后缀方案（`~Wh~w<HWND>`）；
- [ ] 钩子：`_HandleItemResolved / v_WndProc / _MatchWindow / GetNumItems / GetAppID / SetAppID / GetFlags / UpdateFlags / CompareStringOrdinal / DPA_InsertPtr / DPA_DeletePtr`（最小集）；
- [ ] `config.toml` 基本项 + host 常驻 watchdog + explorer 重启再注入；
- [ ] 卸载/禁用完全还原（unhook 后任务栏恢复正常分组）。

**验收**：目标 Windows 版本上，多开 Notepad/Explorer/浏览器 → 每窗口独立按钮；开 关循环 20 次无崩溃；睡眠/休眠/显示器热插拔/DPI 变化后仍正常。

### Phase 2 — 完整功能对齐（3–4 周）

- [ ] 纠偏钩子全量移植（图标/跳转列表/启动/固定：`GetShortcutIDList / GetIconResource / Launch / GetLauncherName / _GetJumpViewParams / ShowJumpView / _UpdateItemIcon / TaskDestroyed` 等）；
- [ ] `customGroups / excludedPrograms / groupingMode(inverse) / pinnedItemsMode / placeUngroupedItemsTogether / useWindowIcons` 全部设置项；
- [ ] 多版本分支：Win10 22H2 / Win11 / Win11 24H2（`symbols.toml` 数据驱动，缺失即降级）；
- [ ] （可选）ExplorerPatcher 旧任务栏分支（修饰名 GetProcAddress 路径）。

**验收**：按 mod README 的行为清单逐项对照（自定义组名显示在任务栏、排除程序保持原生分组、中键/Shift+点击新开实例等）。

### Phase 3 — 打磨与分发（1–2 周）

- [ ] 环形日志 + 掉线自恢复 + 崩溃熔断（连续 2 次 explorer 崩溃即自动停用）；
- [ ] `--install/--uninstall/--status/--inject-once` CLI；开机自启（HKCU Run，无需管理员）；
- [ ] GPL-3.0 合规（LICENSE + 源码公开）；代码签名决策（是否购证书）；
- [ ] README：与 Windhawk 共存的注意事项（同装时需禁用原 mod，避免双重钩子冲突）。

### 持续运营（非一次性）

- 每次 Windows 功能更新：跑一遍验收清单，更新 `symbols.toml`（预计每次 0.5–1 人日，这是该项目最大的长期成本）。

---

## 8. 测试与验证计划

| 层面 | 方法 |
|---|---|
| 运行时行为（B+ 双线路 AUMID） | CI `runtime-smoke` job（任务 9）：windows-latest 真实会话拉起 notepad 多开，断言线路一互异后缀 / 线路二精确共享值 / `restore` 复原与映射表清理；截图与日志工件（`ci/runtime-smoke.ps1`） |
| 符号解析正确性 | host 输出地址 ↔ WinDbg `x explorer!CTaskGroup::GetAppID` 人工比对（每分支抽查 5 个） |
| 钩子稳定性 | 24 h 冒烟（Phase 0）→ 每次 Release 前的回归清单：多开/关闭/固定/取消固定/重启 explorer/注销重登/缩放变化 |
| 行为保真 | 用 mod 官方 README 与设置项语义写成的 checklist 逐项打勾（约 25 条） |
| 内存/体积 | §6 的测量协议，结果写入 `BENCHMARK.md` |
| 卸载干净度 | Process Explorer 查残留钩子/模块；重启后 explorer 行为与 vanilla 一致 |
| 与杀软共存 | Defender/主流 AV 白名单引导文档 + （若购证书）签名后的误报回归 |

---

## 9. 开放问题（需要决策）

1. **GPL-3.0 接受度**：移植版将开源分发，是否可接受？（影响是否走 clean-room 路线）
2. **目标 Windows 版本范围**：是否需要 Win10 22H2，还是仅 Win11 当前分支？（Win10 支持约 +30% 符号表工作量）
3. **host 常驻 or 退出**：1.5–4 MB 换"explorer 重启自动恢复"，选哪种为默认？
4. **是否需要 GUI**：默认按"TOML + CLI 无 UI"设计（体积最优）；若要托盘图标，推荐 Phase 3 用纯 Win32 托盘（+~100 KB）而非 GUI 框架。
5. **代码签名预算**：无签名则 SmartScreen/AV 误报是现实阻力，是否购入 OV/EV 证书？

---

## 附 1：本计划的事实来源

- Windhawk 模块页面与源码：`windhawk.net/mods/taskbar-grouping`、`ramensoftware/windhawk-mods`（GPL-3.0，v1.3.10，2000 行 C++，32 个符号钩子 + 4 个导出 API 钩子，仅注入 explorer.exe）；
- TaskbarFolders：`gianluca-schwekendiek/taskbar-grouping`（MIT，.NET 8/WPF，AUMID+.lnk 免注入方案）；
- ShelfyGAI：`DenisGeide/ShelfyGAI`（MIT，Python，窗口样式整理 + 恢复状态持久化，明确声明不做 shell 注入）；
- aumid-stopgap-tools：`tksh164/aumid-stopgap-tools`（MIT，C++，`SHGetPropertyStoreForWindow`+`PKEY_AppUserModel_ID` 免注入改分组，Win11 实测，27★，核心代码 ~184 KB）；
- mihaicuibus/taskbar-grouping（Java+JNI，Win7 时代样例，核心代码仅 821 字节即完成外部 AUMID 分组）；
- 体积/内存数字均为工程估算，以 Phase 0 实测为准。

---

## 附 2（附录 B）：GitHub 相似项目全景（2026-09-21 检索 `taskbar-grouping`，共 11 项）

检索方式：GitHub Repository Search `q=taskbar-grouping`（total_count=11，全量核对，无遗漏）；逐项抓取 README 与代码树。

### B.1 全景评估表

| # | 仓库 | 语言 | ★ | 技术路线 | 对本项目的参考价值 |
|---|---|---|---|---|---|
| 1 | [tksh164/aumid-stopgap-tools](https://github.com/tksh164/aumid-stopgap-tools) | C++ | 27 | **免注入·属性存储 AUMID**：`mklnkwaumid` 生成带定制 AUMID 的 `.lnk`；`runwaumid` 启动目标进程，待窗口出现后用 `SHGetPropertyStoreForWindow` + `PKEY_AppUserModel_ID` 改写其 AUMID，使窗口归入固定磁贴组（演示：两个 Obsidian vault 独立图标并排） | ★★★★★ **路线 B+ 的直接实锤**：公开文档 API 在 Win11 最新版实测可用；README 诚实标注 CAUTION（"外部改 AUMID 非常规用法"）；MIT 许可、代码量小（23 文件），Rust 移植无障碍；含 ProcessIdWindowFinder/WindowTitleWindowFinder 等待窗口出现的实用模式 |
| 2 | [mihaicuibus/taskbar-grouping](https://github.com/mihaicuibus/taskbar-grouping) | Java+JNI | 2 | **免注入·同一 API**：JNI 调 `SHGetPropertyStoreForWindow` 写 `PKEY_AppUserModel_ID`，把 Swing 多窗口拆到不同任务栏按钮（README 自述面向 Win7+，2013 年代风格；仓库 2022 年最后更新） | ★★★ **历史佐证**：该 API 自 Win7 起即可外部设置运行中窗口分组；核心 JNI 代码仅 821 字节，把机制展示得一清二楚（对照读物） |
| 3 | [gianluca-schwekendiek/taskbar-grouping](https://github.com/gianluca-schwekendiek/taskbar-grouping)（TaskbarFolders） | C# | 0 | 免注入·AUMID + `.lnk` + 共享 Launcher + 合成图标 + 弹出扇出面板（主计划 §2.2 已详评） | ★★★ 固定磁贴式自定义分组的完整范本；ADR 文档可作设计输入 |
| 4 | [DenisGeide/ShelfyGAI](https://github.com/DenisGeide/ShelfyGAI) | Python | 4 | 免注入·窗口样式隐藏（WS_EX_APPWINDOW/TOOLWINDOW）+ overlay hub（主计划 §2.3 已详评） | ★★ 安全护栏/恢复状态持久化设计；佐证"不注入做不了真分组"的原判断 |
| 5 | [pulni4kiya/windows-taskbar-replacement](https://github.com/pulni4kiya/windows-taskbar-replacement)（TaskbarNicifier） | C# | 0 | **自绘覆盖式**：WPF 窗口盖在系统任务栏中段之上，自渲染带分组/固定的按钮，管理真窗口（EnumWindows、虚拟桌面过滤、全屏检测、OverlaySettings） | ★★ "第三条路线：覆盖式"的样例——与真任务栏（拖放固定、跳转列表、缩略图预览）有本质交互差距，但窗口枚举/全屏检测/设置持久化代码可参考 |
| 6 | [aratakotaki/taskbar-grouping](https://github.com/aratakotaki/taskbar-grouping) | Tauri/TS | 0 | **早期脚手架**（日文 README）：Tauri 2 × React × TS 的"任务栏分组管理"应用计划，面向 UWP 应用与 Web URL 的固定分组；但"動作確認"章节仍停留在 Tauri 模板 greet 演示，UWP/URL 管理均列于"今後の拡張予定"（未实现），2026-03 后无提交 | ✗ 无可参考实现；仅佐证"快捷方式管理器"这一产品方向有人想要 |
| 7 | [wanghao9103/deskflow-desktop-manager](https://github.com/wanghao9103/deskflow-desktop-manager) | Tauri | 0 | 自绘桌面管理器：桌面图标分组 + 自绘"主题任务栏"组件（2026-08 更新） | ★ 非系统任务栏集成，纯自绘 UI；仅"桌面分组收纳"的交互参考 |
| 8 | [413hq/SmoothFolder](https://github.com/413hq/SmoothFolder) | C# | 1 | 桌面 iOS 风格玻璃文件夹（3×3 活预览、拖放收纳；明确不替换 shell/任务栏），2026-09 更新 | ★ 桌面层而非任务栏层；其"排除 Alt+Tab/任务栏自身窗口"的细节处理可借鉴 |
| 9 | [morrisscd/TaskbarIconGrouping](https://github.com/morrisscd/TaskbarIconGrouping) | C++ | 1 | 2019 年 Win7 MSDN 缩略图工具栏样例（无 README，12 KB） | ✗ 与分组 hook 无关 |
| 10 | [satodu/kde-webapp-gen](https://github.com/satodu/kde-webapp-gen) | Python | 11 | KDE Plasma 浏览器 webapp 生成器 | ✗ 平台不符 |
| 11 | [riisager/chrome-profile-badger](https://github.com/riisager/chrome-profile-badger) | Python | 3 | Linux/Mint 的 Chrome profile 启动器 | ✗ 平台不符（思路与 AUMID 剖面图标相同，可作对照阅读） |

### B.2 关键发现（对主计划的实质影响）

1. **注入路线在开源世界零先例**：11 个结果无一复刻 explorer 内部 hook 路线（Windhawk mod 本体是唯一开源参考）。→ 路线 A 的"以 GPL mod 为唯一蓝本、独力维护"的判断不变。
2. **免注入路线有实锤（重要更正）**：`SHGetPropertyStoreForWindow` + `PKEY_AppUserModel_ID` 是微软公开文档 API（Shell 属性存储），允许外部进程改写**运行中窗口**的分组归属。两个独立项目（早期 Win7 时代的 JNI 样例、活跃维护且 Win11 实测的 aumid-stopgap-tools）先后验证。此前主计划 §3.2 将"免注入实现取消分组"判为 ❌，**证据不足，已更正为 ⚠️ 实验性可达**。
3. **三条免注入技术路线齐了**：① 属性存储 AUMID 改写（aumid-stopgap-tools/mihaicuibus）→ 真正的运行中窗口分组控制；② `.lnk`+AUMID 固定磁贴（TaskbarFolders/aratakotaki）→ 自定义分组；③ 覆盖式自绘（TaskbarNicifier/deskflow）→ 完全接管外观但牺牲系统交互。路线 B+ 取 ①+② 组合。
4. **无人用 Rust 做过**：11 项中 C++ ×2、C# ×3、Java/Python/TS/Tauri 若干——**Rust 版本仍是空白**，本项目的差异化定位成立；同时说明没有现成 Rust 代码可抄，全部为自研。

### B.3 各仓库代码层面的可复用资产（路线 B+ 视角）

| 资产 | 来源 | 移植方式 |
|---|---|---|
| "等待窗口出现→写属性"循环（PID/标题匹配） | aumid-stopgap-tools `runwaumid`（C++） | 直译 Rust：`EnumWindows` + 轮询 + 超时；改用 `SetWinEventHook` 事件驱动更优雅 |
| `.lnk` 内嵌 AUMID（IPropertyStore + PKEY_AppUserModel_ID + IShellLinkW） | aumid-stopgap-tools `mklnkwaumid`、TaskbarFolders | `windows` crate 的 `IShellLinkW`/`IPropertyStore` COM 调用，~150 行 |
| 合并图标生成 + 弹出面板 | TaskbarFolders | WPF 实现不移植；弹出面板如需要可用纯 Win32（+100–200 KB）或舍弃（B+ 核心=分组，不是文件夹 UI） |
| 全屏检测 / 虚拟桌面过滤 / 设置持久化 | TaskbarNicifier、ShelfyGAI | 小模块直译；ShelfyGAI 的 recovery.json 模式用于 B+ 的"退出还原原 AUMID" |

### B.4 结论

- **对"能不能做小 exe"的回答强化了**：路线 B+ 只依赖公开 API，一个 <0.5 MB 的单文件 `tbg-lite.exe` 即可覆盖"取消分组 + 自定义分组"的 80–90% 场景，零崩溃传导、零 Windows 更新维护——这是比原预期（必须注入）更乐观的新选项。
- **对"要不要注入"的回答不变但有了退路**：追求 100% 保真（零竞态、UWP/Chromium 全覆盖、固定项精细交互）仍需路线 A；双轨 Phase 0 用数据裁决（见 §4、§7）。
- 检索的局限声明：仅按用户要求检索 `taskbar-grouping` 一个关键词；相关但改名的项目（如 7+ Taskbar Tweaker 系、ExplorerPatcher 的 never-combine 功能、StartAllBack 等闭源方案）未在检索范围内，其中 ExplorerPatcher/StartAllBack 属"还原旧任务栏自带不合并"的商业可行替代，在路线决策时应作为环境事实知晓（它们与本项目互斥而非参考）。
