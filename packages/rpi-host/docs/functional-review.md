# rpi-host 功能 review：管理闭环不等于 RPC 功能替代

## 范围与结论

最初 #58 只读 review 现有管理扩展和直接依赖的 Host 合同，并新增 20-Agent 测量夹具；
当时没有修改生产 Rust/core/wire，没有安装或部署。#59 在扩展内修复错误状态传播，
未改 core/SDK/wire v1.4；#60 定点增加 v1.5 `results` capability 与原生只读结果投影。
旧内存数据及旧负向探针原样保留，不归于后续 DLL。

**“提交 → 观察 → 分页取回答案”的最小闭环现已成立，但仍不是完整 RPC 替代。**
结果/outcome 与原生工具错误标记已有真实 DLL 验收；多模态、流式、资源装配等边界仍在。
不能用历史内存优势掩盖这些差异。

## 1. P1（已补齐最小闭环）：按原请求读取执行结果

**历史缺口（#58）：** `submit` 返回原生 admission receipt，证明输入已持久保管，不是完成输出。
`receipt` 的 `execution` 仅含：

```json
{"state":"started","runId":"...","finished":true}
```

`finished` 的实现只检查是否存在对应 `operation_finished` 记录；没有投影该记录的 outcome。
因此当时的工具不能取得最终回答，也不能凭这个布尔值区分成功、失败、取消。
`inspect` 返回 catalog/Agent 元数据，不能代替执行结果。当前也仍没有流式事件订阅。

证据：

- [HostAdmissionExecution](/Users/bigfish/work/rpi-projects/pi-rust-wt-host/crates/rpi-harness/src/agent_harness/host_admission.rs#L112)
- [finished 的结构性查询](/Users/bigfish/work/rpi-projects/pi-rust-wt-host/crates/rpi-harness/src/agent_harness/host_admission.rs#L534)
- [服务端 receipt 投影](/Users/bigfish/work/rpi-projects/pi-rust-wt-host/crates/rpi-cli/src/host/server.rs#L408)
- [扩展完整命令 schema](../src/schema.json)

20-Agent 夹具会在所有请求结束后检查 fixture 自己拥有的原生 JSONL，确认回答确实保存；
**这不是扩展具备结果读取功能的证明。** 测试读取文件不能成为产品绕过 IPC、复制 transcript
或另建执行权威的实现。

**当前补齐（#60）：** v1.5 `result` 复用原生 admission → run → Harness `get_result`
稳定引用，返回 admitted/running/completed/aborted/failed/unknown 与有界最终 assistant
文本；后续页固定原 run_id。终止结果读取后另做原生物理同步；finished=true、缺失引用、
无法确认活动状态或同步失败均不升级为成功。没有修改 Harness、复制 transcript 或新建执行权威。

[结果接口报告](result-interface.md)与[原始 native 事件](result-interface-results.json)记录
真实 CLI → DLL → 认证 IPC → Host → HTTP Provider 的 9 项场景：33 次工具调用、6 次
Provider 请求，含 UTF-8 多页、跨 Agent/原 run 隔离、旧答案不随当前 tip 改变、失败、
运行中取消、崩溃重载后的 unknown 与无重放。旧 peer 无 capability 不发送新命令；
结果查询丢回复仍为 query_failed。流式订阅仍保持后续，不作为最小接口的前置。

## 2. P1（已修复）：真实失败被外层标为工具成功

**历史复现（旧测量 DLL）：** 扩展 `poll` 对所有业务结果调用 `StepResult::done`，
只在 JSON text/details 中写 `error=true`；没有启用结构化结果信封。宿主 legacy decoder
不读取 details 内的 `error`。三轮真实 CLI → release DLL → 认证 IPC → Host 对不存在
Agent 的合法提交，都出现 `details.error=true/status=rejected`，而 native
`tool_execution_end.isError=false`。原始 `error_flag_probe.native_event` 仍保存在
[20-Agent 数据](memory-20-results.json)，没有改写历史事件。

**当前修复：** 注册 schema opt in 现有 `x-rpi-result-envelope: 1`，将本地投影的 error
映射到信封 `isError`，同时完整保留 content/details、unknown 分类与原意图对账命令。
信封超出 1 MiB 时返回显式错误；mutation 保留原身份的 conservative unknown，不丢对账
信息或静默截断成成功。没有修改 core/SDK/wire，也没有重跑内存。

验收在 #59 当时使用新 release DLL 的 **18 个包单测、8 个实际 native 状态用例**，以及原有基本 7 次
和行政 21 次/all 12 类命令 smoke；不是只检查 JSON 内的 `error`。
真实 Host 拒绝已确认 native `isError=true`；fault peer 的丢回复仍保留原 ID/对账，
uncertain 回执仅证明投影链路，不冒称真实 journal 的 uncertain 故障验收。

实现与证据见[错误传播修复报告](error-envelope.md)、[工具实现](../src/lib.rs)、
[注册 schema](../src/schema.json)、[真实 DLL 回归](../tests/native_errors.py)。
本地加载端必须支持现有 v1 结果信封；此标记不是动态能力握手，不承诺旧 CLI 自动兼容。

## 3. 其他明确差异、边界与未验收项

| 能力 | 当前状态 | 影响/分类 |
| --- | --- | --- |
| status/list/inspect/profiles | 已有，分页有界 | 不自动翻页；不是完整 Session 内容读取 |
| submit + durable receipt + 原 ID 去重 | 已有 | admission 不等于执行完成，也不保证未知工具副作用 exactly-once |
| create/restore/close/stop | 已有受控完整意图 | 必须固定 grants、epoch/revision/profile 引用；不是自由 argv/env/cwd 的 spawn |
| 按需 N | 可加载预声明的 N 个独立 profile | 一份受信 profile 固定一个 Agent/Session，不是可任意实例化的模板；不是“默认 CLI 已自动接管” |
| 最终文本、执行 outcome | 已有 v1.5 有界原请求结果读取 | 不是全多模态、流式事件或完整 RPC 替代 |
| 流式进度 | 尚无订阅接口 | 不阻挡已补齐的终止文本结果闭环 |
| 文本以外的输入 | 当前 submit 只接受文本 | 不等价于 RPC 的结构化文本/图片 prompt 能力 |
| 运行中取消 | #60 管理 DLL 已验收 signalled=true → 原生 aborted | 仅空工具 HTTP Provider fixture；不等于任意工具/OS 后代结算 |
| 本地工具 cancel/destroy | 关闭本地交换、join 请求线程 | 不是远端 Agent cancel；mutation 仍须按原身份对账 |
| Agent 关闭后的 admission 对账 | 查询依赖 loaded Ordinary client | 没有独立的离线历史查询入口；NotLoaded 不等于从未准入。行政 journal receipt 是另一种接口，不应混淆 |
| 管理工具并发 | 每 loaded DLL 最多 4 个本地请求 | 第五个会 `not_sent`，不自动排队/重试；这不是 Host 只能跑 4 个 Agent，本次真实 barrier 为 20 个 |
| 权限与配置 | 单个显式私有 endpoint，注册时冻结 | 高权限工具，caller 继承配置授予的 Host 能力；不是 OS 沙箱，也没有多 Host 动态选择 |
| 配置轮换/reload | `CONFIG` 为 image-global OnceLock | 不能保证同一 DLL image 仍被引用时再注册会重读配置；操作说明不应把普通 reload 当作无条件刷新保证。此为源码风险，未做真实 reload 复现 |
| Host 内逐 Agent 扩展/DLL、交互审批/定时器 | 尚未完整装配 | 管理 DLL 在调用方 CLI 中加载成功，不代表被管理 Agent 已加载生产插件 |
| 后台 daemon/整组恢复/默认 CLI Host | 尚未接通 | #60 仅显式前台 Host 崩溃后重载查询；需 fixture 自行清理已退出 PID 的 stale socket，不是产品自动整组恢复 |

Loaded-client 前置证据：
[HostService::admission](/Users/bigfish/work/rpi-projects/pi-rust-wt-host/crates/rpi-cli/src/host/service.rs#L402)。
冻结配置证据：[CONFIG](../src/lib.rs)。固定身份 profile 证据：
[TrustedProfileSet](/Users/bigfish/work/rpi-projects/pi-rust-wt-host/crates/rpi-cli/src/host/profile_set.rs)。

## 4. 内存比较必须限定的口径

- 比较对象是 **1 个共享 Host 中的 20 个被管理 Agent**，与 **20 个独立 native RPC rpi 进程**。
  **不是**旧 `rpi-subagents` 的同进程 ChildAgent vs Host 比较；现有 subagents 已明确不回退
  subprocess，不能把它错误描述为每 Agent 一个 RPC 进程。
- 相同显式 release binary、同一 cwd、20 份独立 Session、相同 Provider/模型/文本/空工具集。
  HTTP fixture 逐一校验 20 个完整请求，全部到齐后才采样，再统一放行；不是只启动 20 个空壳。
- 新路径的 20 次 submit 确实经过 CLI 加载管理 DLL、host 工具和 IPC；backend 主表不计管理者，
  另采样管理 CLI 与 Host+管理 CLI 合计，避免只展示省略管理成本的数字。
- 管理 CLI 的整个 footprint 不是“DLL 本体增量”；还包含其 Harness、模型工具往返与上下文。
  RPC 侧未计实际 Web/RPC 前端，shared 合计列是额外计入管理者的保守参考，不是完整应用同构比较。
- 本轮每个被管理 Agent 没有插件、审批、工具执行、长历史或大输出；不能据此宣称完整生产
  功能等价或任意负载都有相同比例的节省。也没有运行 600/3000。
- macOS 以 `libproc` `phys_footprint` 的进程 ledger 合计为主，RSS 辅助；这些不是全系统
  物理页去重后的精确内存并集。保留每 PID 原始采样、线程数和 FD。

## 5. 推进顺序

1. **已完成：** 扩展内修正错误状态传播，并新增真实 DLL 负向回归。
2. **已完成最小功能验收：** 定点补上原生有界结果/outcome，完成“提交 → 观察 → 取回答案”。
3. **已覆盖空工具 fixture：** 管理 DLL 运行中取消、查询断连与 unknown；更广的工具/资源结算仍未覆盖。
4. 资源/DLL 装配与薄 Host 后台恢复仍保持后续边界，不重新变成扩展交付的无限前置。

本 review 的置信度：结果的请求关联、错误标记、Loaded-client 前置为**高**（源码及真实探针）；
reload 风险为**中**（源码分析，未实测）；内存结论只适用于单独报告记录的本机匹配负载。
