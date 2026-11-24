# rpi-host：原生工具错误状态修复

> 本报告冻结的是 **#59 当时的范围、产物与证据**，以下 18 单测/8 native 用例、
> v1.4 与“结果/运行中取消未覆盖”均指该轮。后续 #60 已补结果接口与运行中取消验收，
> 见[结果接口报告](result-interface.md)；不改写本报告的历史 artifact、事件或内存结论。

## 范围

修复功能 review 的第二项 P1：业务 `details.error=true` 没有传播到原生
`tool_execution_end.isError`。仅改管理扩展的结果编码、注册 schema、包内测试和文档；
**没有改 core、SDK、wire v1.4，没有重跑内存，也没有安装、提交或部署。**

结果/outcome 读取缺失仍未修复：`submit` 是 admission，`receipt.execution.finished`
只表示存在结束记录，不提供最终回答，也不能区分成功、失败、取消。

## 实现与边界

- [注册 schema](../src/schema.json) 声明已有 `x-rpi-result-envelope: 1`。
  [poll / 编码器](../src/lib.rs) 仍用 SDK 的 `StepResult::done`，但返回已有 v1 信封，
  由支持该信封的宿主转换为正确的成功或结构化错误；不新建 SDK API 或错误平台。
- `error=false` 才映射为原生工具成功；其他本地 error 形状保守为错误。
  正常大小的 `content` JSON 与 `details` 完整相等，不丢原请求身份、原始 Host data、
  `outcome_unknown` 分类或 `reconcile.command`。
- text/structured framing 合计不能超过宿主现有 1 MiB 上限。wire frame 合法并不代表
  双份 text/details 信封也合法。超限不让 decoder 丢结构化信息、不静默截断成成功：
  - query 返回小型 `query_failed`；不返回原 data，尽量使用更小分页；
  - mutation 返回小型 `outcome_unknown`，保留原 request ID、原 Agent/历史 epoch/
    scope/完整行政意图对应的只读对账命令；不回显 prompt，不重放工作。
- `reply_received` 仍只表示收到认证回复，不升级为生命周期或任务执行成功。
  本地 cancel/destroy 仍只关闭交换、join 线程，不冒充远端任务取消。
- 加载 DLL 的本地 CLI 必须支持 v1 结果信封。schema 标记不是能力握手，也不是旧 CLI
  兼容性保证；这与远端 wire capabilities 不同。只验证本报告明确指定的核心构建。

## 验收

### 包级检查

- **18 个单测通过，0 失败/忽略**；包含成功/全部业务失败分类、完整 details 与 text
  等价、错误形状 fail-closed、两种超限处理、真实 ABI 本地失败，以及原有真实 FFI
  mutation cancel 后 unknown/原身份对账/destroy join。
- 本包 `cargo fmt --check` 通过。
- 本包严格 `cargo clippy --all-targets --no-deps -- -D warnings
  -A clippy::not_unsafe_ptr_arg_deref` 通过。唯一显式例外沿用 SDK export 宏所需的
  raw-pointer lint，不扩大到依赖，也没有修旧包 warning。
- 原有 native smoke 重跑：基本 **7** 次工具调用、行政 **21** 次调用覆盖全部 **12**
  类命令；各只有 **1** 次被管理 Agent 的真实 Provider 执行。

### 真实 CLI → DLL → native 工具事件

[回归脚本](../tests/native_errors.py) 不使用 PATH/安装版 binary fallback，不运行 benchmark。
它只复用测量脚本中的进程/framing helper，显式选择以下 8 个独立用例。
[原始事件与 artifact hash](error-envelope-results.json) 同时校验管理 Provider 收到的 JSON、
RPC 原生 `isError` 和保留下来的 `details`，不是仅检查扩展的内部返回值。

| 用例 | 远端 | details.status | native isError |
| --- | --- | --- | --- |
| status 成功 | 实际空 Host | reply_received | false |
| 向不存在 Agent 提交 | 实际空 Host | rejected | true |
| 缺少操作者配置 | 无连接 | not_sent | true |
| query 回复前断连 | 认证 fault peer | query_failed | true |
| mutation 发出后丢回复 | 认证 fault peer | outcome_unknown | true |
| uncertain 行政回执投影 | 认证 fault peer | native_not_succeeded | true |
| query 信封超限 | 认证 fault peer | query_failed | true |
| mutation 信封超限 | 认证 fault peer | outcome_unknown | true |

丢回复与 mutation 超限均断言原 request/Agent ID、可直接回传的 receipt 命令不变；
uncertain 投影保留完整行政 request，包括历史 epoch、scope、revision/action。
超限不返回大 data；所有事件不含配置 token。fault peer 只接受一个原始请求并检查
没有第二个连接，不能把 fault peer 的投影测试称作真实 Host journal 的 uncertain 故障验收。

全部 fixture CLI/Host 正常退出；RPC 读线程、peer、HTTP serving/handler 线程均回收/join。
没有启动 daemon、注册服务或碰生产数据。运行中远端 Agent 取消仍未由管理 DLL 验收；
行政 smoke 的 `signalled=false` 仍只是已完成输入的取消。

## 产物与不变性

- **沿用核心 binary SHA-256**：
  `81501327df8cd705d389001b609bbd5eb28a3c5ed8132a971273e0cd985ae816`
- **本次新 release DLL SHA-256**：
  `f3f3d455c14fdf49374ca9a8f3c34e4f4116deb8f30c99ec49e084e247a086eb`
- 原测量前快照中的 **854 个核心输入逐一未变**。两仓四份 wire 合同逐字一致。
  未重跑完整 core/SDK/extcore/workspace，不能称 core 全绿；原有 #56 等 WIP 保留。
- 原 [20-Agent 数据](memory-20-results.json) 仍绑定旧 DLL：
  `13f31ba9092ff26f158d2656895f1ed7f7b69c5279d295b4cdf94f0bd54fd6be`。
  历史错误事件仍是 `isError=false`，不改写历史。测量脚本 SHA 仍为
  `912e94f5bd9bf09c488247c740f90e7d41201cfb4a9bcb59ce912a49990f510a`。
  **本次修复没有产生新 DLL 的 20-Agent 内存结论。**

下一项是按原 Agent/request/run 身份关联、有界、只读的原生结果/outcome 接口，
完成“提交 → 观察 → 取回答案”；不读文件绕过 IPC，不复制 transcript 权威，
不把完整 Host 默认接管/资源装配重新作为扩展前置。
