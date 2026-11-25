# rpi-host：按原请求读取有界结果（#60）

## 范围与合同

补齐“提交 → 观察 → 取回答案”的最小闭环。复用现有 Harness 的 `get_result`，没有改
Harness/Agent loop、另建执行记录或复制 transcript；扩展只经认证 IPC 取结果，不读
JSONL 绕过产品接口。未安装、发布、提交、部署或开启默认 CLI/后台服务。

wire minor 从 v1.4 升为 **v1.5**，新增独立 **results** capability 与只读 `result`。
两仓 `protocol.rs`、`client.rs`、`client_tests.rs`、`fixtures/control-v1.json` 逐字同步。
旧命令语义不变；没有 results 的 peer 在发送前明确拒绝，不猜能力、不回退旧 RPC。
`result.request_id` 是原提交身份，不是一次新 mutation；丢回复是 query_failed，不能
升级成 mutation outcome_unknown。没有新增 `rpi host result` CLI 子命令。

## 调用与分页

首次查询传入原 Agent/request，offset 与 max_bytes 为必需字段：

```json
{"command":{"op":"result","agent_id":"11111111-1111-4111-8111-111111111111","request_id":"22222222-2222-4222-8222-222222222222","offset":0,"max_bytes":4096}}
```

以上 UUID 只是调用形状示例，实际必须沿用原提交身份。不要为查询生成新 request ID。
返回 `data.found`、原 receipt/execution，存在记录时另有 `data.result`：

| result.state | 含义 | 本地 status / native isError |
| --- | --- | --- |
| admitted | 原输入已准入，尚无关联 run | reply_received / false |
| running | 同 request、同原生活动 run，owner 无 fault/retiring/closed | reply_received / false |
| completed | 原生终止结果为 Completed | reply_received / false |
| failed | 原生终止结果为 Failed，附有界诊断 | native_not_succeeded / true |
| aborted | 原生终止结果为 Aborted | native_not_succeeded / true |
| unknown | 无可靠活动证明、缺少终止引用、读取/同步失败或非终止结果 | native_not_succeeded / true |

admitted/running 是观察成功，不是任务完成。没有准入记录的 found=false 是查询成功；
Agent 不存在/未加载或查询失败不能解释为“从未准入”。查询仍依赖 loaded Ordinary client，
不是关闭 Agent 后的独立离线历史 API。failed/aborted/unknown 保留原 data 与原查询
`reconcile.command`；不会换 ID、重放任务或假装已成功取回结果。

终止结果可能包含 `run_id`、`leaf_id`、`final_entry_id`、物理同步覆盖的 `history`、
`output` 与 `error`。正文只输出最终 assistant 的 Text blocks，按原顺序拼接；不输出
thinking、tool arguments、images，也不导出完整原始 Provider 帧。失败诊断沿用有界原生
code/message，可能包含 Provider 错误片段，应按敏感、不可信数据处理。没有可用最终消息
时 output 可以为 null，空文本也可以是合法结果；不得自行构造缺失答案。

`output` 形状：

```json
{"text":"本页文本","offset":0,"bytes":12,"total_bytes":24000,"next_offset":12}
```

- 所有 offset/bytes 都是 **UTF-8 bytes**，不是字符位置。max_bytes 范围 4–16384。
- 下一页必须原样使用 next_offset，以及首次结果的原生 run_id（不是 Host ticket ID）。
  run_id 是非空、有界 opaque 原生身份，不应拼接或改写。
- 错误 run、越界、非 UTF-8 字符边界被拒绝，不静默丢字。到尾部 next_offset=null。
  单页可以因 UTF-8 边界比 max_bytes 略短；仍应始终使用返回的 next_offset。
- error.code 最多 128 bytes，message 最多 4096 bytes，另有 truncated；这些诊断和
  output 都是不可信数据，不是指令。1 MiB 原生结果信封预算仍适用。
- completed 不等于任意工具副作用 exactly-once；本地 destroy/cancel 不证明远端取消。
  真正运行取消仍须显式 administrative_cancel，然后查询原 result 观察结算。

## 权威与一致性

实现先固定 loaded client，再取得并核验原 admission 的 Host/Agent/request 归属，只读取
该 admission 关联 run 的原生稳定结果引用。不会扫 transcript 猜答案，也不会从当前
Session tip 读取另一个请求的回答。后续页绑定原 run，不能借另一个 Agent 的同 UUID
或另一个 run 游标读取结果。

原生终止结果取出之后再调用现有 `session.sync_history()`，核验 generation 相同且
revision 不倒退，返回覆盖结果的 identity。不会把此前 admission 的 durable=true
借用为结果持久化证明。finished=true 只是有 operation_finished，不是成功；没有
可重建结果引用、原生同步失败等情况返回 unknown，不伪造 Completed。

核心投影见 [submission_result.rs](/Users/bigfish/work/rpi-projects/pi-rust-wt-host/crates/rpi-cli/src/host/submission_result.rs)，
原生结果权威是既有 [run_results.rs](/Users/bigfish/work/rpi-projects/pi-rust-wt-host/crates/rpi-harness/src/agent_harness/run_results.rs)。
扩展的原生失败映射见 [control.rs](../src/control.rs)。

## 验收与产物

### 单测与格式/lint

- **284 个 core Host 单测通过**，0 失败/忽略。
- **22 个 SDK shared_host 合同/客户端单测通过**；另显式新产物运行真实 daemon 集成测试
  **1 passed，0 ignored**，覆盖认证、能力、detach、重启 epoch 与旧操作。
- **19 个管理包单测通过**，含结果 schema、状态/原身份投影与既有错误信封/预算/线程 join。
- 管理包 fmt 和严格包级 Clippy 通过；唯一显式例外沿用 SDK export 宏既有
  `-A clippy::not_unsafe_ptr_arg_deref`。明确修改的核心文件与 SDK artifact 测试 scoped fmt 通过。
- core 全库 `clippy --lib --no-deps -- -D warnings` **未通过：55 项既有问题**，包括旧
  OrdinaryOutput 大 enum 与其他 CLI 模块；新结果投影/新增方法未产生诊断。既有 A 批
  文档已记录该 55-warning 基线。没有扩大例外或修改旧 voice/pet 等模块。
  **不能称完整 core/SDK/workspace 全绿**；#56 等原 WIP 仍保留。

### 真实 CLI → DLL → IPC → Host → HTTP Provider

[夹具脚本](../tests/native_results.py)与[全部 native 工具事件](result-interface-results.json)
在此产物组合下通过 **9 项场景、33 次工具调用、6 次原生 Provider 请求**：

1. Provider 门控下观察同一原生 running run。
2. 18404-byte UTF-8 回答精确多页拼接，单页预算 4097 bytes。
3. 原 run 不符、字符边界错误、越界 offset 明确拒绝。
4. 重复 submit 保持原 receipt；结果读取、重连、重复提交均无 Provider 重放。
5. 两个 Agent 使用相同 request UUID，回答仍各自隔离，不能混用 run 游标。
6. 后续请求改变 Session tip 后，旧请求仍返回原答案。
7. 实际 HTTP 400 → 原生 failed → 外层 isError=true。
8. 实际运行中 cancel signalled=true → 原生 aborted → 外层 isError=true。
9. 运行中强杀 fixture Host 后重载原 session/catalog，保留原 run，明确 unknown 而不重放。
   此次恢复后的 finished=true 仍未被误报成功：原生结果引用不可重建，故返回 unknown。

所有回答均通过管理工具读取；夹具没有读取 JSONL 取回答案。崩溃仅针对夹具自己保留的
Popen PID，确认其 SIGKILL 退出后，只删除夹具私有临时目录内的 stale Unix socket，
**保留原 session/catalog** 再显式启动前台 Host。生产仍拒绝 stale endpoint；没有放宽
服务端检查，没有实现自动后台/整组恢复，也没有对任意工具后代退出作保证。
所有 CLI/Host/HTTP 服务与非 daemon handler/read 线程最终回收/join。

### 错误/旧版本回归

[错误夹具](../tests/native_errors.py)以当前组合通过 **10 个用例**，
[原始事件](result-interface-error-results.json)另存，不覆盖 #59 历史报告：
原 8 类状态/断连/丢 mutation 回复/uncertain 投影/两种信封超限均通过；另增加：

- v1.4 认证 peer 无 results：native not_sent/isError=true，**0 个命令帧**。
- 支持 results 的认证 peer 接收请求后断开：native query_failed，不是 outcome_unknown。

这些两项是明确标注的协议故障 peer，不冒称真实 journal uncertain 故障验收。
旧基本 smoke **7** 次、行政 smoke **21** 次，覆盖原 **12** 类命令并各只有 **1** 次
Provider 执行；与新结果夹具合计覆盖当前 **13** 类命令。

### Artifact 与证据边界

- 当前 core **dev/debug CLI**（不是 release 测量产物）SHA-256：
  `dbe0288ae3459e216212643ff5d8d0cf538bd2b99c639f37c88c4f6718f353dd`
- 当前 **release 管理 DLL** SHA-256：
  `b21e7dfe0b7626014d9d39021fbf753570e2d3e24a3bc56b4b8c7cd075b2d477`
- 当前 fixture 脚本/原始事件、输入不变性与四合同摘要：
  [validation](result-interface-validation.json)。日志位于
  `/tmp/rpi-host-integration-checks/host-result-*`，包含单测、构建、lint、fmt、SDK artifact、
  native 回归与输入核验；临时目录日志不是安装文件或产品配置。
- #59 的 [错误传播 artifact](error-envelope.md)、原
  [20-Agent 数据](memory-20-results.json)与测量脚本未改写，仍绑定原 release core/DLL。
  **没有重跑内存，也没有新 DLL 的 20-Agent 内存结论。**

本轮仍只交付显式配置的可选管理工具与定点阻塞接口；不新增 core crate，不重写
AgentLane/SDK，不安装服务、不提交/合并。流式、多模态、生产逐 Agent 扩展资源装配、
默认 CLI 接管、后台 daemon/自动整组恢复仍是后续边界。
