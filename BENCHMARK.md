# tbg-lite 实测基准（任务 21 回填）

> 口径：GitHub Actions windows-latest（Windows Server 2025，Win11 24H2 内核）
> 真实交互式会话；release 构建（`cargo build --release --locked`，体积导向
> profile：opt-level="z" + LTO + strip）。对照 plan v1 §6 预算表（存档于
> git 历史 commit 8e09b0e）。
>
> 维护者指令（2026-09-23）：GitHub CI 测试等同真机测试。

## 1. 体积

| 项 | 实测 | plan v1 §6 预算 | 判定 |
|---|---|---|---|
| 单文件发布 zip（exe） | **153,637 字节（150 KB）**（v0.1.0，SHA256 3E700805…D3F94E，[Releases](https://github.com/xixka/taskbar-grouping/releases)） | B+ 路线 <0.5 MB 目标 | **达标**（约为预算 1/3.4） |

> release 工作流（`.github/workflows/ci.yml` 的 `release` job，tag `v*` 触发）
> 在发布说明中自动写入当次构建的 `size_bytes` 与 `sha256`，并附
> build-provenance attestation（审计 SEC-04）。v0.1.0 实测：zip 153,637
> 字节；attest 步骤绿灯（release run 35815197242，5/5 job 成功）。

## 2. 常驻内存（watch 运行中）

| 项 | 实测 | 验收线 | 来源 |
|---|---|---|---|
| 工作集 Working Set | 9.15 MB | <10 MB | run 35679966357 Phase C（门禁断言） |
| 私有提交 Private Bytes | 1.48 MB | — | 同上 |

测量场景：watch（线路一）运行中 + 3 个 notepad 窗口事件处理后静置 10 s。
每次 push 由 `phase0b-acceptance` job 的内存门禁断言回归。

## 3. 行为压测（50 窗口，双线路）

| 指标 | 线路一（ungroup） | 线路二（group） | 来源 |
|---|---|---|---|
| 50 窗口改写 | 50/50 | 50/50 | run 35679966357 Phase A/B（门禁） |
| 漏检 missed | 0 | 0 | 同上 |
| 应用回写 reverted | 0 | 0 | 同上 |
| 写失败 | 0 | 0 | 同上 |
| AUMID 写入时延 | 0.1–2.5 ms/窗 | 同量级 | watch 统计（write 列） |
| 还原复原 | 50/50 回原生 AUMID | 50/50 回映射原值（表清空） | 同上 |
| UIA 任务栏层 | 50 独立按钮 + 溢出菜单正常 | 单组按钮（`Notepad - 50 running windows`） | 同上（UIA 探针） |

## 4. 多应用覆盖（摘要）

用户应用 7/7 = 100%（notepad / mspaint / cmd / Windows Terminal /
Windows PowerShell 控制台 / regedit / Edge 短时）；已知限制：Explorer
文件夹窗口会被 shell 回写（自管 AUMID，plan v2 §2）。完整矩阵与裁决见
[`docs/coverage-matrix.md`](docs/coverage-matrix.md)。

## 5. 常驻加固（任务 20）

| 指标 | 实测 | 来源 |
|---|---|---|
| explorer 重启检测 | 2 s 级轮询 PID，重启即全量重扫 | runtime-smoke Phase X（门禁） |
| 钩子存续（重启后新窗口） | 重启后新开窗口仍被标记 | 同上 |
| 异常熔断 | 连续 3 次短命异常退出 → 自动注销自启 | 同上 |
| 环形日志 | 256 KiB 上限，超限截半 | 同上（X1 门禁） |
