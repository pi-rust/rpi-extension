# Core Clippy follow-up（#61）

## 结论与范围

本轮按授权清理原有 **55 项 rpi-cli core Clippy**，包括必要的 TUI/pet 定点修改。严格库级检查已清零，无新增 `allow` / `expect`。仅 core CLI 的 **34 个 Rust 文件**发生本轮源码修改；两树原有 WIP 均保留，没有扩展 Host 功能、提交、安装或发布。

**不能称全库测试或 workspace 全绿**：正确 Cargo 环境下，全库单测为 **1529 passed / 4 failed / 3 ignored**。四项签名夹具失败保留，未通过跳过用例或放宽签名检查掩盖。

本记录与 [验证 JSON](clippy-followup-validation.json) 是新产物证据；[结果接口原验收](result-interface.md)、原 JSON、错误信封与 20-Agent 内存证据均不改写，未重跑内存 benchmark。

## 实现收口

- 机械修复：等价迭代/Option 简写、初始化表达式、诊断文档与回调类型推断；三个锁文件的 `OpenOptions` 显式 `truncate(false)`，保持原不截断语义。
- 具名分组：`ProviderModelConfig`、`RpcOptions`、`InteractiveStartup`、`EditorUi`、`SessionLane`、`ViewPresentation`，同步实际和测试调用。保留原表达式、Provider 凭据可选语义、显式 lane、OAuth `flow_id` 竞态保护和 Windows 大 TUI future 的 `Box::pin`。
- 大 enum：`OrdinaryOutput::Run` / `Compaction` 及 durable human `View::Outcome` 使用 Box；不改 Agent loop、原生 admission/run/result 权威或 IPC 合同。
- RPC 内部 variant 改为 `ExecuteExtension`，显式 `serde(rename = "execute_command")` 保持 wire 名字；原解码测试同步。

## 验证

| 检查 | 本轮结果 |
| --- | --- |
| `cargo clippy -p rpi-cli --lib --no-deps --locked --offline -j 2 -- -D warnings` | 通过，0 诊断，0 新豁免 |
| scoped rustfmt / 双树 `git diff --check` | 34 文件格式与两树空白检查通过 |
| `cargo build -p rpi-cli --bin rpi --locked --offline -j 2` | 通过，重建 dev/debug CLI |
| core 全库单测 | **1529 / 4 / 3**，无过滤；不是全绿 |
| 上述全库内的 Host / TUI / 配置 / 安装 | **284 / 389 / 37 / 39** 通过；TUI 另 1 项原 ignored |
| 上述全库内的 modes / native durable / RPC | **79 / 62 / 81** 通过 |
| 管理包 / SDK `shared_host` 单测 | **19 / 24** 通过 |
| SDK 真实 daemon fixture | **1 passed / 0 ignored**，显式 `RPI_SHARED_HOST_BINARY`，只证明读侧认证 IPC |
| 实际 CLI → 管理 DLL → Host → HTTP 结果闭环 | **9 场景 / 31 工具调用 / 6 Provider 请求** |
| 错误回归 | **10 用例**通过；真实 Host 与认证故障 peer 的证据分开 |
| 基本 / 行政 smoke | **7 / 21** 次工具调用，各 **1** 次 Provider 请求 |

结果闭环覆盖 running、UTF-8 多页、错误 run/offset、重复不重放、同 request 跨 Agent 隔离、后续 Session tip 不混答、Provider failed、运行取消后 aborted、崩溃重载 unknown 且不重放。分页轮询次数会随时序变化，本轮实际计数为 31，不沿用旧报告的 33。所有 fixture 自有资源已回收；崩溃后的私有 stale socket 清理不是产品自动恢复功能。

本地构建/测试环境使用冻结的 `CARGO_HOME`、各自工作树 `CARGO_TARGET_DIR=$PWD/target` 与 **`TMPDIR=/private/tmp`**。首次超时尝试的混合日志、一次缺失 Cargo cwd/TMPDIR 的直接测试二进制试跑均不作验收证据；最终独立 `core-clippy-55-unit-final.log` 是正确 Cargo 命令的完整结果。日志位置及哈希见验证 JSON。

## 保留的四项测试失败

均位于本轮未修改的 `prebuilt_extensions.rs`：

- `prebuilt_extensions::tests::all_six_distributions_install_and_record_exact_provenance`
- `prebuilt_extensions::tests::downloaded_manifest_and_archive_install_into_an_isolated_profile`
- `prebuilt_extensions::tests::legacy_archives_install_only_the_library_and_keep_their_records_unchanged`
- `prebuilt_extensions::tests::prebuilt_packages_upgrade_without_force_and_same_version_is_unchanged`

失败为 macOS `/usr/bin/codesign` 拒绝测试构造的 **128 字节 Mach-O 头**：`object file format unrecognized, invalid, or unsuitable`。已核验该测试文件及 `install::resign_staged_native` 函数字节与冻结输入一致，并在自有临时目录对 aarch64/x86_64 同夹具分别执行原签名命令，均独立复现 exit 1。

**未执行完整 before-source 单测基线，因此不冒称完成了整版基线对照。** 本轮不修这些额外夹具问题，不绕过内核签名边界，也没有新增 skip。三个 ignored 用例名单见验证 JSON。

## 产物与保全

- 新 CLI：`/Users/bigfish/work/rpi-projects/pi-rust-wt-host/target/debug/rpi`（dev/debug）\
  SHA-256：`48835ce2ac25d300ceafc67bf448b8c1cf70760325580ba98b341ff9817b592a`
- 实测管理 DLL：`/Users/bigfish/work/rpi-projects/rpi-extension-wt-host/target/release/librpi_host.dylib`（既有 release DLL，**未重建**）\
  SHA-256：`b21e7dfe0b7626014d9d39021fbf753570e2d3e24a3bc56b4b8c7cd075b2d477`
- core 冻结输入 **1144** 文件，其中 **1110** 原样、34 白名单变更；extension **821/821** 既有输入原样。本轮 extension 仅新增本报告与验证 JSON，不覆盖任何旧文件。
- 两仓四合同（`protocol.rs`、`client.rs`、`client_tests.rs`、`fixtures/control-v1.json`）逐字一致、哈希不变；每个修改源文件的前后哈希见验证 JSON。

本轮不是 `--all-targets` / 全 workspace / MSRV 专用工具链 / 跨平台验收；没有完成 #56、生产 Host 插件装配、默认 Host、自动整组恢复或 daemon 后续工程。未知工具副作用仍不承诺 exactly-once，取消夹具不代表任意工具后代都已结算。
