# 20-Agent 内存实测：共享 Host 与独立 RPC 进程

> **历史快照：** 数字与原始负向事件绑定修复前 DLL（SHA-256 `13f31ba9…54fd6be`）。
> 后续扩展错误传播已修复，见[修复报告](error-envelope.md)；未用新 DLL 重测内存，
> 不改写本报告原始测量数字、artifact hash 或旧 `isError=false` 事件。

## 结论

在本机匹配的小文本负载下，共享 Host 有明显内存优势：

- 20 个原生 Provider 请求同时等待时，**共享 Host backend 10.06 MiB，20 个独立 RPC
  backend 合计 100.74 MiB**；footprint 降低 **90.0%**，约为原来的十分之一。
- 将真实加载管理 DLL、已完成 20 次 submit 工具往返的管理 CLI 也计入，shared 总计
  **20.99 MiB**，相对上述 RPC backend 仍少 **79.2%**。
- **这不是功能等价证明。** 当时存在结果/outcome 读取缺失、外层工具失败标记错误等，
  其当前状态见[功能 review](functional-review.md)。尤其不能把它与同进程 ChildAgent 的内存差异混淆。

## 方法与口径

- macOS ARM64，本机 16 GiB RAM、12 个逻辑 CPU；不设置 `TOKIO_WORKER_THREADS`，两路径
  均使用实际 CLI 的默认 runtime 配置。系统上其他用户进程不纳入测量，也不操作它们。
- 两路径使用**同一个 release rpi binary**，管理扩展也是 release。构建全部完成后才测量，
  不将编译占用或 debug 预检数据混入正式结果。
- 三轮成对测量，顺序 `shared/RPC`、`RPC/shared`、`shared/RPC`。各阶段采 7 次、间隔
  100 ms，表中取各轮中位数的中位数。每 case 独立创建 20 个 Agent/Session，无历史复用。
- 相同 cwd、Provider、native 模型、空工具集、无用户扩展/context/skills/模板发现。
  输入为 4096 个 ASCII `x` 加固定说明，完整 prompt **4152 UTF-8 bytes**；同样的短回答。
- HTTP fixture 收到并完整读取 **20 个实际原生请求**后才打开测量 barrier。两路径全部
  120 个正式测量请求的完整 Provider JSON（含 system prompt）SHA-256 **完全相同**。
  请求一直持有至 barrier 采样完毕，然后统一放行；不是顺序运行，也不是只启动空进程。
- 新路径的全部 20 次 submit 均经过 **真实 CLI → release 管理 DLL 的 host 工具 →
  认证 IPC → 实际 Host → Provider**。最终回答的验证使用 fixture 自有 JSONL，
  不冒称产品已经有结果读取接口。
- 指标为 `libproc` `RUSAGE_INFO_V0.phys_footprint` 的进程 ledger 合计。RSS 另列，
  **不能把 RSS 相加称为物理内存**；footprint ledger 合计也不是全系统物理页去重后的精确并集。
- backend 主表比较 1 个 Host 进程中的 20 个被管理 Agent，与 20 个独立 RPC rpi 进程。
  Python 控制器、HTTP fixture 和实际 Web/UI 前端均不计入。另列 shared 的管理 CLI 全量与
  Host+管理 CLI 同批采样合计，是额外计入管理成本的保守参考，不是完整应用同构比较。

## 正式 footprint 结果

单位 MiB（1 MiB = 1048576 bytes）。降低比例相对于同阶段 20-RPC backend。

| 阶段 | 20 个独立 RPC backend | 共享 Host backend | backend 降低 | shared 管理 CLI | Host + 管理 CLI | 含管理者降低 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 初始化、全部空闲 | 88.65 | 5.70 | 93.6% | 4.55 | 10.24 | 88.5% |
| 20 个 Provider 请求同时等待 | 100.74 | 10.06 | 90.0% | 10.89 | 20.99 | 79.2% |
| 全部完成后保留 Agent/Session | 106.18 | 12.91 | 87.8% | 10.89 | 23.77 | 77.6% |

合计列来自两个 PID 的同批原始采样，不是机械相加另外两列的中位数。
管理 CLI 是完整进程开销，包含自己的 Harness、host tool schema、上下文和 20 次工具往返，
**不是 DLL 文件大小或 DLL 单独的增量**。

三轮各 case 中位数的范围：

| 阶段 | RPC backend | Host backend | Host + 管理 CLI |
| --- | ---: | ---: | ---: |
| 空闲 | 88.651–88.666 | 5.657–5.704 | 10.220–10.251 |
| Provider barrier | 100.651–100.901 | 10.063–10.126 | 20.939–21.017 |
| 完成后保留 | 106.041–106.276 | 12.719–12.907 | 23.610–23.860 |

这次结果稳定，但没有测量高水位/全程 peak；不能把阶段采样当作峰值上界。

## 线程、FD 与辅助 RSS

取同样的三轮中位数口径。RPC 总量为 20 个 PID 合计。

| 阶段 | RPC 线程 / FD | Host 线程 / FD | Host+管理者线程 / FD | RPC RSS MiB | Host RSS MiB | Host+管理者 RSS MiB |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 空闲 | 320 / 240 | 15 / 34 | 31 / 47 | 346.03 | 20.13 | 37.42 |
| Provider barrier | 341 / 280 | 15 / 74 | 32 / 89 | 427.02 | 28.33 | 57.64 |
| 完成后保留 | 344 / 260 | 38 / 54 | 54 / 69 | 449.39 | 31.86 | 61.11 |

差异包含独立进程各自初始化的 runtime、线程及 CLI 状态，不能把全部节省都归因于某个
Agent 结构体。完成后的 Host 线程增加包含文件系统 blocking workers，本测量不再展开
核心线程池或资源生命周期改造。

## 功能/负向复验

正式 shared 三轮都在采样结束后追加一个符合 schema 的不存在 Agent 提交探针：
`details.error=true, status=rejected`，但 native `tool_execution_end.isError=false`。
这是**发现问题的证据**，不是“所有功能通过”的断言。

同一组最终 release 产物另通过现有 native smoke：

- 基本模式：7 次管理工具调用、1 次原生 Provider；
- 行政模式：21 次管理工具调用、覆盖全部 12 类命令、1 次原生 Provider；
- 行政 cancel 仍只验证已完成输入的 `signalled=false`，不冒称运行中取消验收。

正式六个 case、两次 smoke 的自建 CLI/Host 均正常退出，测试线程全部 join。
无部署、无安装、无系统服务，无用户 Session/默认扩展改动，也没有运行 600/3000。

## 产物、源码与失败历史

实际 release 产物 SHA-256：

```text
rpi
81501327df8cd705d389001b609bbd5eb28a3c5ed8132a971273e0cd985ae816
librpi_host.dylib
13f31ba9092ff26f158d2656895f1ed7f7b69c5279d295b4cdf94f0bd54fd6be
```

全部原生 Provider 请求 JSON 的 SHA-256：

```text
33276f42e9c024dcfd726467e93f578b2c0ca24cc230e457c4e32040254bf393
```

- [原始每 PID/每阶段采样、三轮结果及真实错误事件](memory-20-results.json)
- [可复现夹具](../tests/compare_20_memory.py)
- `/tmp/rpi-host-integration-checks/host-20-release{.json,.log}`
- `/tmp/rpi-host-integration-checks/host-20-{release-build,dll-release-build}.log`
- `/tmp/rpi-host-integration-checks/host-20-release-native{,-admin}.log`
- `/tmp/rpi-host-integration-checks/host-20-inputs-before.json` 在 release 构建进行中、测量前捕获。
  它不是干净 checkout 或全部编译输入的密码学构建来源证明；本轮未修改核心生产源码。

首轮 debug 夹具把 `.progress.jsonl` 也计入历史文件 glob，导致计数断言失败，相关自建进程
正常回收；已改为检查 20 个准确的权威 history 路径。失败日志保留为
`host-20-fixture-debug-first.log`，修正后的 debug 预检为 `host-20-fixture-debug-second.*`。
两者均未进入上述 release 表。

本轮没有重跑完整 core/SDK/extcore/workspace 回归；既有 #56 暂停补丁与 help 测试不一致
仍保留，不能因本次 release 构建和 smoke 通过就标成核心全绿。

## 复现

先使用允许的 worktree 构建显式 release 产物，再运行：

```bash
python3 packages/rpi-host/tests/compare_20_memory.py \
  --binary /absolute/release/rpi \
  --plugin /absolute/release/librpi_host.dylib \
  --rounds 3 --output /absolute/new-memory-results.json
```

脚本只支持有 `libproc` 的 macOS，固定 20 个被管理 Agent，不扩大到 600/3000；
拒绝覆盖已有证据输出。fixture 使用自己的临时目录、环境、Provider 和进程，结束后回收。

**适用范围仅为这次空工具、小文本、短历史的匹配负载。** 未测试生产插件、交互审批、
工具后台后代、长上下文/大输出、运行取消或恢复。完整功能/资源装配后必须按实际路径复测，
不能从这组数字外推生产内存上限或与同进程 ChildAgent 的优势。
