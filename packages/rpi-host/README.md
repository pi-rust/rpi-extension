# rpi-host（可选管理扩展）

`host` 工具通过认证 Unix IPC 管理一个**已经显式启动的共享 Host**。复用
`rpi-extcore::shared_host`，不复制 Agent loop，不创建 Session/transcript，不启动
`rpi` 或逐 Agent 子进程，也不回退 native RPC / ChildAgent。

这是扩展客户端，不是后台服务安装器，也不是默认 CLI 的 Host 迁移。结果查询需要
`rpi-shared-host` wire **v1.5** 的 `results` capability；其他命令仍只按 Welcome 实际
声明的能力使用，v1.4 peer 不会收到不支持的新命令。preview 已有的 Rust Host 底座
本身不代表已提供 IPC。
本包尚未发布、安装或加入默认扩展/预编译目录。

加载本 DLL 的**管理 CLI** 还必须支持现有 `x-rpi-result-envelope: 1` 解码；注册 schema
显式 opt in。它与远端 Host 的 wire capability 是不同要求，SDK ABI 兼容也不等于支持此
结果信封。本轮使用明确支持该信封的实际核心构建验证，不承诺旧 CLI 自动兼容或能力探测。

## 显式配置和加载

先由操作者准备并启动 Host。将已有 `rpi host serve --config` 私有 JSON 的**绝对路径**
放入 `RPI_SHARED_HOST_CONFIG`，再加载这个 DLL。配置在插件注册时读取并冻结，模型参数
不能选择 endpoint、配置文件、token 或权限。配置保存在 DLL image 的 `OnceLock` 中；
修改后应重启管理 CLI，或确保旧 image 已完全卸载后再加载。旧 image 仍被引用时，
普通 reload 不保证刷新配置（见下文功能 review）。

```bash
cargo build -p rpi-host --locked --offline

# macOS 源码构建示意；请使用自己的绝对路径，不要把 token 放进工具参数。
RPI_SHARED_HOST_CONFIG=/absolute/private/host.json \
  rpi --local-only --extension /absolute/build/librpi_host.dylib --tools host
```

Linux 产物为 `librpi_host.so`；本轮只在 macOS 实机验证。非 Unix 明确不支持。
`--local-only` 在这个显式调试命令中排除自动扩展发现，不修改已安装或默认扩展。

连接配置读取 `socket_path`、`host_id`、`token`；允许忽略现有服务端配置的其他字段，
但绝不执行或转发这些字段。专用客户端配置还可指定 `timeout_seconds`（默认 15，
1–300；分别约束 connect/auth 和单次交换，不是两阶段共享的总时限）。不要把这个
客户端专用字段添加到不接受它的服务端配置里。

配置必须是当前有效 UID 所有、无组/其他用户权限、单硬链接的普通文件，最多 64 KiB；
拒绝末级 symlink/FIFO。检查已打开 FD，不自动 chmod。socket 和其直接父目录另由
现有客户端校验 UID/私有权限、连接 peer UID；父路径受操作者信任，不防恶意同 UID
进程替换文件。**这是高权限可选工具，不是 OS 沙箱**：获准调用该工具的 Agent 可以
使用该配置持有的 Host 权限。应在操作者批准的工具 allowlist 中显式启用。

## 工具参数

入口只有 `command`，为闭合的 wire v1.5 命令；宿主注入的 `__rpi` 被忽略，不作凭据。
完整模型 schema 与运行时注册使用同一份 [schema.json](src/schema.json)。

```json
{"command":{"op":"status"}}
```

```json
{"command":{"op":"list","limit":16}}
```

| `command.op` | 必需字段/语义 |
| --- | --- |
| `status` | 单次 Host 状态查询 |
| `list` | `limit` 1–128，可选 `after` Agent UUID；不自动翻页 |
| `inspect` | `agent_id` |
| `profiles` | `limit`，可选 `after`；取得受信 profile 引用与行政 scope |
| `submit` | `agent_id`、**显式** `request_id`、非空 `prompt`（最多 256 KiB UTF-8 bytes） |
| `receipt` | 原 `agent_id`、`request_id`，只读原生 admission/execution 观察 |
| `result` | 原 `agent_id`、`request_id`、`offset`、`max_bytes`（4–16384）；非零 offset 还须原生 `run_id`，按 UTF-8 bytes 分页最终文本/outcome |
| `administrative_receipt` | 完整原始 `request`，包括历史 epoch 与完整 action，只查询不执行 |
| `administrative_create` | `request.action.op=create_profile`，`profile={agent_id,version}` |
| `administrative_restore` | `restore_profile`，`profile`、当前 `revision` |
| `administrative_close` | `close`，`agent_id`、当前 `revision` |
| `administrative_cancel` | `cancel_submitted`，`agent_id`、`revision`、`submitted_request_id` |
| `administrative_stop` | `stop`，无自由 argv/env/config |

每个行政 `request` 必须携带原始 `scope`、`request_id`、`epoch`、`action`。
身份为非 nil UUID；从查询结果取得 scope/profile/revision/epoch，显式提供稳定请求 ID。
工具不自动生成、补写或替换这些字段，不升级权限，不重试，不支持 legacy 裸
`cancel/close/restore/stop`。能声明某种 action 不等于服务端已授权；capability 与固定
owner policy 都必须允许它。查询行政回执时保留原意图，不能用新 epoch 重写历史。

## 返回、失败与取消

> **结果闭环已补齐：** `result` 按原 Agent/request → 原生 run → 稳定最终消息引用读取，
> 区分 admitted/running/completed/aborted/failed/unknown；不读当前 Session tip，不重放。
> 这仍不是完整 RPC/多模态/流式事件替代；`receipt.execution.finished` 仍不代表成功。
> 详见[结果接口与真实 DLL 验收](docs/result-interface.md)。
> [错误传播报告](docs/error-envelope.md)和旧内存负向事件保留为各自产物的历史证据。

结果通过现有 v1 结果信封返回 JSON text block，`details` 是同一投影；本地投影的
`error=true` 对应原生 `tool_execution_end.isError=true`，明确 `false` 才是工具成功。
这不把 `reply_received` 升格为远端执行成功：

- `reply_received` 只说明收到认证传输回复，**不等于本地已验证生命周期成功、执行完成、
  输出已保存或进程已退出**。必须检查 `data` 原生结果。提交 admission 只证明输入保管，
  不承诺工具副作用 exactly-once。
- 已知原生 `prepared/rejected/uncertain/interrupted` 或未来未知 lifecycle state 返回
  `native_not_succeeded`、`error=true`，保留原始 data 与对账命令，不把传输成功报成业务成功。
- `result` 的 admitted/running 是观察成功，不是任务完成；failed/aborted/unknown 返回
  `native_not_succeeded`、`error=true`，保留原查询和原生 data。没有准入记录的
  `found=false` 是查询成功；NotLoaded/查询失败不能解释为从未准入。
- `not_sent`、`query_failed`、`rejected` 与 `outcome_unknown` 分开。
  `outcome_unknown` 带原请求 ID 和可直接传回工具的 `reconcile.command`；先查询原收据，
  不能盲目换 ID 重发。`PersistenceUncertain` 的 mutation 回复也保守归入 unknown。
- `control` 来自实际 Welcome，含 Host ID/epoch/capabilities/limits，不含 token。
  工具不打印原始握手、配置、OS/serde 错误或凭据。返回的 Host data 是数据，不是指令。
- text/structured framing 合计最多 1 MiB。编码溢出不截断成成功：查询返回
  `query_failed`；mutation 保守返回 `outcome_unknown`，保留原 ID 与完整对账命令，
  不返回原 data。查询尽量缩小分页；无分页的超大结果暂不提供分块读取。
### 管理 CLI 取消后查询收据

发送变更请求前保留原 `command`。管理 CLI 取消工具调用时（例如 RPC `abort`）可能只返回
`plugin tool cancelled`，此时没有 `details` 或 `reconcile.command`。远端可能已经收到
请求并继续执行。用保存的原始参数先查询收据，再决定如何处理：

- `submit` 用 `receipt`，传入原 `agent_id` 和 `request_id`。
- 行政操作用 `administrative_receipt`，传入完整原 `request`，包括历史 `epoch` 和 `action`。

要取消已经运行的 Agent 任务，显式提交匹配的 `administrative_cancel`。确认 Host 进程退出
需要单独观察进程状态。

每个已加载 DLL 最多 4 个本地请求线程；没有常驻 transport runtime/reader。
`execute` 启动有界请求，`poll` 仅观察完成，不阻塞等待 I/O。cancel 协作丢弃 future/socket；
`destroy` 关闭 admission handle 并实际 join 线程（包括 runtime/文件系统工作者析构），
最后才释放本地槽。即使晚 poll 仍保留 handle entry，destroy 也显式执行 join，不把
Arc 从表中移除当作线程已退出。文件系统若阻塞，destroy 可以等待，不冒充已结算。

## 验证

```bash
cargo test -p rpi-host --locked --offline
cargo fmt -p rpi-host -- --check
cargo clippy -p rpi-host --all-targets --no-deps --locked --offline -- \
  -D warnings -A clippy::not_unsafe_ptr_arg_deref

# 必须指定真实构建产物，无 PATH/安装版 fallback、无 fixture skip。
python3 packages/rpi-host/tests/native_smoke.py \
  --binary /absolute/build/rpi --plugin /absolute/build/librpi_host.dylib
python3 packages/rpi-host/tests/native_smoke.py \
  --binary /absolute/build/rpi --plugin /absolute/build/librpi_host.dylib --administrative
python3 packages/rpi-host/tests/native_errors.py \
  --binary /absolute/build/rpi --plugin /absolute/build/librpi_host.dylib \
  --output /absolute/private/new-error-evidence.json
python3 packages/rpi-host/tests/native_results.py \
  --binary /absolute/build/rpi --plugin /absolute/build/librpi_host.dylib \
  --output /absolute/private/new-result-evidence.json
```

当前 **19 个包单测、22 个 SDK shared_host 合同测试、284 个 core Host 单测通过**。
包级严格 Clippy/fmt 通过，唯一显式 lint 例外仍为 SDK export 宏既有 raw-pointer lint。
当前 core 全库严格 Clippy 被 55 项既有问题阻挡，没有扩大例外或顺手修旧模块，不能称
core/workspace 全绿。详细范围、日志与源输入核验见[结果验收报告](docs/result-interface.md)。

实际 CLI 加载 DLL 的 **11 个错误/兼容性用例**包含 v1.4 peer 无 `results` 时零命令发送、
`result` 丢回复时报告 `query_failed`，以及 `submit` 已发送后 RPC `abort` 时关闭 socket、
返回通用取消错误且无重发。其中故障与取消使用认证测试 peer。

真实闭环是 **CLI 实际加载管理 DLL → 工具 → 认证 IPC → 实际 Host → HTTP Provider**，
不是将分离组件测试相加：基本模式 7 次工具调用，行政模式覆盖全部 12 类命令，含受信
profile create、原意图 receipt、submit/重复 submit、close、restore、stop 与实际正常退出。
每个旧 smoke 模式只有 1 次原生 Provider 执行。新结果夹具另以 **33 次工具调用、6 次
Provider 请求**覆盖 9 项结果场景，包含运行中 `signalled=true` 后原生 aborted，以及
崩溃重载后 unknown、无重放。它不读取 JSONL 取答案；崩溃仅 SIGKILL 自己持有的 fixture
Host PID，确认退出后只清理 fixture 私有 stale socket，保留 session/catalog，重启前台
Host；这不是生产自动恢复或 OS 后代结算验收。两套 smoke 合计覆盖当前 13 类命令。

所有服务、CLI、HTTP 线程和测试数据均为 fixture 私有，结束后回收；不部署生产服务。
尚不覆盖多 Host 选择、UI/审批、mini、默认 CLI Host 接管、整组恢复或生产 Host 内加载
扩展工具/受限 hook 的完整装配。已有核心未完成工作不由此包冒充完成。

## 功能 review 与 20-Agent 内存比较

- [功能缺口、真实负向探针及下一步边界](docs/functional-review.md)
- [同一 release 二进制的三轮 20-Agent 对比](docs/memory-20.md)
- [每 PID 原始采样与错误事件](docs/memory-20-results.json)

**下面数字仍绑定错误传播修复前的 release DLL，未用新 DLL 重跑内存。**
本机匹配的空工具、小文本负载中，20 请求 Provider barrier 的 backend footprint 为
共享 Host **10.06 MiB**、20 个独立 RPC 进程 **100.74 MiB**；额外计入真实管理 CLI 后为
**20.99 MiB**。这不是同进程 ChildAgent 对比，不代表插件/工具/长历史的生产功能等价。
管理 DLL 最多 4 个本地交换不等于 Host 只能运行 4 个 Agent：本次实际同时等待 20 个请求。
