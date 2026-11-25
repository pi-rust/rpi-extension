# Shared Host control client（阶段性）

这个模块面向共享进程 Host，不是现有 `host::HostSession`（native RPC 子进程 owner）。
它不启动 `rpi`、不持有 Agent、不读取 transcript、不自动重试，也不隐式回退 ChildAgent。

**core feature worktree 已接通前台 Host 和 wire v1.5 认证 IPC：status/list/inspect、
profiles、Agent 级 submit/receipt/result，以及完整原意图的 administrative receipt/create/
restore/cancel/close/stop。能力与固定 owner policy 均需允许，不因枚举声明而授予权限。
私有 startup/dynamic profiles 仍由 core 绑定；新建 Session 使用持 writer lease 的原子
exclusive-create，不覆盖既有历史/孤儿 progress。可选 [`rpi-host`](../../../rpi-host/README.md)
管理 DLL 已创建，复用本客户端，不重做 Agent/Session；已通过真实 CLI 加载 DLL、实际
Host 和 HTTP Provider 的单条贯通链路。mini、审批、默认 CLI Host 接管与整组恢复仍未
接通，本客户端及该扩展不冒充这些能力已完成。**

## 调用边界

- 通过 `ConnectionConfig` 显式提供私有 Unix socket、Host ID、认证 token 和整体 deadline。
  非 Unix 明确返回 UnsupportedPlatform，不走另一套进程实现。
- 握手检查协议 major、Host ID、epoch、能力和预算；未知 capability 不授予权限。
- 最大帧 1 MiB、页最多 128、协商 in-flight 最多 64。每客户端当前只串行一个交换；
  core 的 server 当前协商 1，默认 8 个连接任务；实际能力以 Welcome 为准，不能把
  客户端接受的 wire 上限当成运行时并发数。token 允许整个 catalog 查询及对已加载
  Agent 触发预授权 Provider/工具执行，不是逐 Agent grant。
- 中途取消会丢弃 socket，不能继续读残帧。`detach` 只丢连接，不关闭远端 Agent。
- query 失败、发送前拒绝和 mutation outcome unknown 分开报告。
  `OutcomeUnknown` 带原 request ID，要求显式查 receipt，不得盲目重试。
  core 已实现 Agent 级 `receipt` 查询（`receipts` 能力）；返回 receipt 前会重证严格
  history 同步，失败返回 PersistenceUncertain。查询不追加 transcript、不重放执行；
  收据仅证明输入保管，不证明执行完成或工具副作用 exactly-once。
- `result` 使用独立 `results` capability，按原 Agent/request → 原生 run → 稳定结果引用
  返回有界最终文本/outcome。UTF-8 byte 分页每页 4–16384 bytes，非零 offset 需原 run_id。
  旧 v1.4 peer 无能力时 NotSent(UnsupportedCapability)，不发命令；该查询的 request_id
  不使它成为 mutation，丢回复仍是 QueryFailed。不能把 receipt.finished=true 当成成功。
  详情见[管理工具结果报告](../../../rpi-host/docs/result-interface.md)。
- token 的 Debug 隐去内容，客户端错误不带原始 peer 帧、路径、token 或 OS 错误。
  不要记录序列化的握手帧。父路径依赖受信本机文件系统，不隔离恶意同 UID 进程。

没有插件 ABI、Harness、Axum、ACP 或前端依赖；可选 `rpi-host` 包在本模块之上经认证
IPC 提供管理工具，不在这里重实现 Agent 或原生 Session 权威。

## 合同同步

`protocol.rs`、`client.rs`、`client_tests.rs`、`fixtures/control-v1.json` 与 core 的
`crates/rpi-cli/src/host/` 对应文件逐字一致。不使用跨仓库未发布 path dependency。
变更 wire major/能力/错误语义时须两侧同时更新 fixture 和测试。

在本 extension worktree 运行：

```bash
CARGO_TARGET_DIR="$PWD/target" cargo test -p rpi-extcore --lib shared_host:: --locked --offline -j 2
CARGO_TARGET_DIR="$PWD/target" cargo test -p rpi-extcore --locked --offline -j 2
```

本轮控制合同/客户端 **22 passed**。此前完整 extcore 回归 **371 库 + 11 resource recovery
passed，5 ignored**：其中原有 4 项需要显式 native/DLL，新增 1 项需要下述 binary，均不算通过。
本轮没有重跑上述完整回归。合同单测仍使用合成 peer；此前另行显式运行的跨仓库必需产物
测试 **1 passed, 0 ignored**，验证真实 rpi daemon 的认证/查询/capability/detach/重启 epoch 与正常退出。

```bash
RPI_SHARED_HOST_BINARY=/Users/bigfish/work/rpi-projects/pi-rust-wt-host/target/debug/rpi \
CARGO_TARGET_DIR="$PWD/target" cargo test -p rpi-extcore --test shared_host_daemon \
  --locked --offline -j 2 -- --include-ignored
```

没有显式二进制时该 ignored 测试若被启用会失败，不跳过、不找 PATH fallback。只有测试
fixture 启动一个前台 daemon；客户端库不启动进程，更不会每 Agent 创建 RPC 子进程。
上面的 shared_host_daemon 用例不包含管理 DLL/HTTP Provider 执行，不能称为完整 Host E2E。
新增管理 DLL 的单条真实贯通验证与边界见 [`rpi-host`](../../../rpi-host/README.md)；生产服务未部署、安装或替换。
