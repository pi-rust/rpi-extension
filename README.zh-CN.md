# rpi-extension

[English](README.md) | **简体中文**

基于 `pi-rust` 稳定版 `rpi-plugin-sdk` ABI 的 `rpi` 代理扩展包，使用 Rust 原生实现。

本仓库导入了 `pi-rust/rpi-package` 的 main 分支代码树，并作为这些扩展的 crates.io
发布源，行为与上游仓库一致。release-plz 会升版本、更新 changelog 与目录，然后向
crates.io 发布并创建各包标签，入口是
**Actions → Version and publish crates**（在 `main` 上手动触发）。`mode` 输入默认
`dry-run`，只验证全流程而不上传；选 `publish` 才真正发布，并且需要
`CARGO_REGISTRY_TOKEN` 密钥。预编译归档与 `rpi-extension.json` 清单随后附到
[Releases](https://github.com/pi-rust/rpi-extension/releases)，覆盖六个目标平台。
详见[发布指南](docs/github-releases.md)。

工作区包含工具、工作流扩展、模型提供方集成与客户端适配器。每个包都有独立版本
和安装指南。[包目录](catalog/packages.json)记录了各扩展的安装命令和带版本的预编译
清单。面向模型的工具描述、参数 schema 与回归检查见
[工具指引契约](docs/tool-guidance.md)。

## 扩展包列表

| 包 | 工具与能力 |
| --- | --- |
| [rpi-mcp-adapter](packages/rpi-mcp-adapter/README.md) | 通过 `mcp_list`、`mcp_call`、`mcp_resource` 发现/调用工具并管理资源；`mcp_request` 提供原始诊断 |
| [rpi-mcp-server](packages/rpi-mcp-server/README.md) | 用 `--mcp` 将代理发布为 MCP 服务器，或运行独立的 `rpi-mcp-bridge` |
| [rpi-subagents](packages/rpi-subagents/README.md) | 用 `delegate_task` 委派任务 |
| [rpi-codemode](packages/rpi-codemode/README.md) | 用受限 JavaScript 组合原生工具，支持无损业务 JSON 和显式精确原生结果选择（内联限额内）；已发布/预编译扩展，需要配套的 ToolProgram v1 能力 |
| [rpi-orchestrator](packages/rpi-orchestrator/README.md) | 通过 `subagent`、`plan`、`dispatch` 实现有边界的子代理与任务图 |
| [rpi-lens](packages/rpi-lens/README.md) | 用 `code_lens` 检查代码 |
| [rpi-codegraph](packages/rpi-codegraph/README.md) | 用 `codegraph_*` 查询索引化的符号、源码与关系（自动初始化并同步索引） |
| [rpi-todo](packages/rpi-todo/README.md) | 用 `todo` 跟踪任务 |
| [rpi-background-tasks](packages/rpi-background-tasks/README.md) | 用 `background` 运行长命令，用 `jobs` 查看状态 |
| [rpi-goal](packages/rpi-goal/README.md) | 用 `goal` 维护持久化的项目目标 |
| [rpi-optchat](packages/rpi-optchat/README.md) | 用 `zoom`、`search` 和 `/optchat` 回忆本次会话的聊天记忆 |
| [rpi-plan-mode](packages/rpi-plan-mode/README.md) | 用 `/plan`、`plan_mode_start`、`plan_mode_complete` 规划工作 |
| [rpi-btw](packages/rpi-btw/README.md) | 用 `/btw` 开启只读的旁支对话；需要 rpi 0.6.0 或更新版本 |
| [rpi-ask-user](packages/rpi-ask-user/README.md) | 用 `ask_user` 发起交互式提问 |
| [rpi-permissions](packages/rpi-permissions/README.md) | 用 `permissions` 管理工具策略 |
| [rpi-permission-gate](packages/rpi-permission-gate/README.md) | 通过宿主 UI 对话框确认 `bash`、`write`、`edit` 调用 |
| [rpi-websearch](packages/rpi-websearch/README.md) | 用 `websearch` 进行网络搜索 |
| [rpi-webfetch](packages/rpi-webfetch/README.md) | 用 `webfetch` 抓取网页内容 |
| [rpi-im-message](packages/rpi-im-message/README.md) | 运行无头飞书/Lark 服务 |
| [rpi-voice](packages/rpi-voice/README.md) | 用 `/voice` 做语音听写与朗读回复；默认启用离线语音识别 |
| [rpi-run-stats](packages/rpi-run-stats/README.md) | 在 TUI 中实时显示用量、耗时与成本 |
| [rpi-langfuse](packages/rpi-langfuse/README.md) | 用 Langfuse 导出 trace 并管理 score 与 prompt |
| [rpi-providers](packages/rpi-providers/README.md) | 新增 DeepSeek、OpenRouter、llama.cpp 与 Proxy 模型提供方 |
| [rpi-server](packages/rpi-server/README.md) | 用 `--server`（`--server-ws`）提供 TCP JSONL 或 WebSocket 请求与流式订阅服务 |
| [rpi-acp](packages/rpi-acp/README.md) | 接入 ACP 客户端，包括 Zed 与 Open WebUI Computer |
| [rpi-ui-adapter](packages/rpi-ui-adapter/README.md) | 通过 `--webui=openwebui` 直接对接 Open WebUI |
| [rpi-node-core](packages/rpi-node-core/README.md) | 协议中立的节点身份、发现与能力契约 |
| [rpi-node](packages/rpi-node/README.md) | 原生 `rpi --node` 生命周期与开放协议/发现注册表 |
| [rpi-edit-diagnostics](packages/rpi-edit-diagnostics/README.md) | 用精确匹配与编译器风格失败报告替换 `edit` |

持久化工作流状态由原生宿主负责；各扩展不提供第二套持久化运行时。

受 Pi 包启发的扩展是独立的 Rust 实现，并非上游源码的移植；唯一例外是
`rpi-edit-diagnostics` 中的候选搜索，它移植自 pi-semantic-edit（MIT，见其
`NOTICE`）。

`rpi-voice` 默认内置 sherpa-onnx + SenseVoice 做离线语音识别，也支持
OpenAI 兼容的语音转写；模型准备、构建选项与语音输出配置见
[语音指南](packages/rpi-voice/README.md)。

`rpi-run-stats` 会在用量页脚下方增加一行实时监控：轮次、LLM 调用次数、工具执行次数、
输出 tok/s、首 token 时间、响应时间、60 秒滚动 TPM、输入/输出/缓存 token、错误数与美元成本。
用 `/stats on`、`/stats off` 或 `/stats` 切换，也可按 F8。该行以三组紧凑的单色符号显示，
并在请求之间保留已测量的耗时。美元成本按宿主根据模型定价计算出的用量累计。它使用宿主通用的
`UiComponent` 文本接口。指标定义见 [rpi-run-stats](packages/rpi-run-stats/README.md)。

Code Mode 的**首个全量对齐增量已实现，但尚未完成全部计划**。该扩展要求精确的
version-1 `resultEnvelope`、`hostAccounting`、`parentProgress` 与 `losslessValues`
能力，在旧宿主上会直接失败关闭。本地证据：**72 个包测试通过**、严格的
all-target 包 Clippy 通过、**1109 个宿主测试通过 / 6 忽略 / 0 失败**、宿主格式化
通过、**35 个真实 Windows 宿主 + DLL + fixture + gate 冒烟用例通过**。最终结果与
二进制哈希位于 `pi-rust-codemode/target/codemode-full-parity-smoke-5/`；这是源码
构建层面的验证，不代表部署，也不构成新的 CI 通过声明。超过 1 MiB 的大值、完整的
子代理历史/进度/恢复以及 Web/ACP 子卡片与重载仍未完成；自身与祖先工具仍被排除。
见[对齐状态矩阵](docs/codemode-full-parity-matrix.md)。

## 模型提供方集成

`rpi-providers` 是一个独立加载的包，覆盖 DeepSeek、OpenRouter、llama.cpp 与
Proxy。它只有一个原生插件入口，内部按提供方 ID、模型目录与鉴权策略分开维护。
安装或升级只需一个包；各厂商实现放在独立模块中。

```bash
rpi install rpi-providers --github pi-rust/rpi-extension
```

自 0.1.2 起，该包通过 GitHub Releases 以预编译归档分发。它使用带可选 `provider`
特性的 `rpi-plugin-sdk` 0.6.3 以及配套的 `rpi-ai` 0.6.3；该包需要 rpi 0.6.3 宿主。
兼容性、源码链接、迁移与测试见[包指南](packages/rpi-providers/README.md)。

## Web 工作台（rpi-web）

`rpi-web` 在浏览器中提供基于项目的对话，支持流式回复、工具交互、Markdown 渲染、
文件预览与 Git diff。界面支持英文与简体中文。它同时提供原生扩展入口与独立程序，
两者共享同一套 Web 核心。

已发布版本的扩展归档位于本仓库
[Releases](https://github.com/pi-rust/rpi-extension/releases?q=rpi-web-v)。带完整
`assets/` 目录的可移植 Web 包由手动 `release-web.yml` 工作流生成，按下方独立启动
命令操作前请先在 `main` 上运行该工作流。启动前需安装
[rpi 0.7.0 宿主](https://github.com/bigfish1913/pi-rust/releases/tag/v0.7.0)，并为服务
进程配置模型凭据：

```bash
# 在解压后的 Web 目录中运行，并指定一个已存在的项目目录。
./rpi-web --project /path/to/project --rpi-bin /path/to/rpi --assets ./assets --port 3030
```

打开 [http://127.0.0.1:3030/](http://127.0.0.1:3030/)，选择项目并创建会话。Windows 下
使用 `rpi-web.exe` 与 `rpi.exe`。

[Web 使用指南](packages/rpi-web/README.md)涵盖 Windows / Linux / macOS 安装、项目创建、
模型凭据、密码登录、远程访问、持久化与升级。Web 0.1.7 通过 `rpi --webserver` 启动带
内嵌前端资源的原生扩展，`--web` 仍作为兼容别名保留。安装命令为
`rpi install rpi-web --github pi-rust/rpi-extension`，在代码检出目录中也可使用
`rpi install rpi-web --path packages/rpi-web`。

Web 0.1.1 支持 `.rpi/web.json` 启动配置。在项目目录中运行一次 `rpi-web --init`，配置
宿主与资源路径，之后直接用 `rpi-web` 启动。全局默认值可写入 `~/.rpi/web.json`，
命令行参数可临时覆盖。仍在使用 Web 0.1.0 的用户需要升级后才能使用配置文件。

## 共享后端与 HTTP API

`rpi-extcore` 现在承载 ACP/Web 共用的原生 RPC、run、会话、历史、事件与工作区能力。
`rpi-httpcore` 把这些服务适配到既有的 HTTP/SSE API，并提供仅 API 的可执行程序。
`rpi-web` 在其上增加内嵌浏览器前端；`rpi-acp` 把通用运行时映射到 ACP。

```bash
cargo run --locked -p rpi-httpcore -- --project /path/to/project --rpi-bin /path/to/rpi --port 3030
```

`rpi-http` 通过 `rpi-httpcore` 库把同一套仅 API 服务打包成标准原生插件（`rpi --http`）。
它只把既有插件生命周期映射到 `rpi-httpcore`：会启动独立的 core `Service`，不共享加载
该扩展的那个活动 Agent 会话。`rpi-http-service` 可执行程序在不带插件宿主的情况下提供
同样的仅 API 服务。

`rpi-extcore` 同时作为可复用的 crates.io 库发布。应用可通过兼容的小版本线依赖
`rpi-extcore = "0.1"`；它要求 Rust 1.78+，并在运行时需要兼容的原生
`rpi --mode rpc` 可执行程序。参见 [rpi-extcore](packages/rpi-extcore/README.md)、
[rpi-httpcore](packages/rpi-httpcore/README.md) 与
[架构契约](docs/architecture/extension-core.md)。这些新包就是普通 Rust 库；现有扩展的
安装与启动命令保持不变。

## MCP 服务器

`rpi-mcp-server` 是 `rpi-mcp-adapter` 的反向实现：它把代理发布给 MCP 客户端，而不是
消费 MCP 服务器。

```text
MCP 客户端 -> stdin/stdout 上的 MCP -> rpi-mcp-bridge -> rpi --mode rpc
             （或 `rpi --mcp`，复用宿主的 `ownsStdio` ABI）
```

该包负责 MCP JSON-RPC 分帧、工具目录、有边界的会话资源
（`rpi://session/transcript`、`rpi://session/state`）及其 `resources/subscribe` 更新、
`tools/call` 并发与取消映射。原生会话生命周期、提示接收、结算与子进程清理仍留在
`rpi-extcore`。推荐使用独立 bridge 集成，因为它自己持有 stdin/stdout，无需与宿主
仲裁 stdio。同一 bridge 还可为无法启动子进程的客户端提供 MCP Streamable HTTP
（`--http <addr>`，`POST /mcp`），并为自有客户端提供私有 WebSocket 通道
（`GET /mcp/ws`）。

`rpi install rpi-mcp-server` 安装该扩展（`rpi --mcp`）。推荐入口 `rpi-mcp-bridge`
可执行程序以独立分平台归档的形式与 ACP bridge 一起发布：

```bash
task pack-mcp   # 生成 dist/rpi-mcp-<platform>.tar.gz，二进制位于压缩包根目录
```

传输方式选择、客户端配置、信任边界与排错见
[MCP 集成指南](docs/mcp-server.md)；具体工具参数与限制见
[MCP server 包说明](packages/rpi-mcp-server/README.md)。

## 原生 ACP 集成

`rpi-acp` 包含 ACP 协议、会话历史、流式输出、提问处理与 Zed 注册。扩展与独立
`rpi-acp-bridge` 在本包内使用同一套实现。

```text
Computer / Zed -> stdin/stdout 上的 ACP -> rpi --acp -> rpi --mode rpc
                                                       （每个会话一个子进程）
```

用 `rpi` 宿主安装扩展：

```bash
rpi install rpi-acp
```

对接 **Open WebUI Computer** 时，需单独部署 Computer，并把其原生 ACP agent 配置为以
`--acp` 启动 `rpi` 可执行程序。该进程必须能访问已安装的扩展、模型凭据、项目目录与
可写的会话存储。工具在客户端 `session/new` 请求给出的工作区中运行；容器部署必须把
同一路径暴露给 agent 进程。

在 Computer 的 agent 环境中设置 `RPI_ACP_NO_AUTO_REGISTER_ZED=1` 可关闭自动注册 Zed。
当会话历史需要写入持久化容器卷时，把 `RPI_ACP_SESSION_DIR` 设为绝对目录。连接要求与
客户端能力见
[ACP 包说明](packages/rpi-acp/README.md#configure-computer-native-acp)。

对接 **Zed** 时，运行 `rpi --install-zed` 注册代理，或用 `rpi --print-zed` 打印配置片段。
注册过程会保留注释，并在更新已有文件时创建备份。

独立 bridge 自己持有 stdin/stdout，并自行启动 `rpi --mode rpc`。用 `task pack-acp` 构建
其归档；用 `RPI_BIN` 指定后端可执行程序。打包方式与建议的 ACP Registry 条目见
[registry/README.md](registry/README.md)。

## UI 适配器

`rpi-ui-adapter` 增加 `rpi --webui=openwebui`，把既有 Open WebUI 实例直接连接到 rpi ACP，
支持流式输出、进度、工具详情与取消。每个 UI 在 `.rpi/webui/<UI>.json` 中有独立项目配置；
`--webui` 只用于选择适配器。它会自动安装受管 Pipe，无需 Computer；rpi 与项目可以留在
UI 容器之外。配置示例、容器网络与会话行为见
[中文安装指南](packages/rpi-ui-adapter/README.md)。

## 构建与安装

源码构建使用稳定版 Rust 与 Cargo。`task` 命令需要 Task；ACP 集成测试需要 Python 3。
运行扩展前必须先安装并配置好 `rpi` 宿主。

```powershell
cargo build -p rpi-providers --locked
cargo test --workspace --locked
task install
```

运行 `rpi-providers` 的集成测试前需先构建该包，因为测试会加载真实的原生库。

`task install` 会构建工作区并把目录中的扩展复制到 `~/.rpi/agent/extensions`，若设置了
`$RPI_CODING_AGENT_DIR` 则复制到 `$RPI_CODING_AGENT_DIR/extensions`。CLI 会自动发现
默认目录，也可以显式指定：

```powershell
rpi --extensions-dir "$HOME/.rpi/agent/extensions"
```

这些是 `cdylib` 扩展，因此直接用 `cargo install` 并不是面向使用者的正确安装方式。
跨平台的使用者命令内置于 `rpi` 宿主：

```powershell
rpi install rpi-todo --github pi-rust/rpi-extension
rpi install rpi-todo --github pi-rust/rpi-extension --version 0.3.4
```

做 GitHub 预编译安装时必须显式选择本仓库，因为这些包在 crates.io 上的元数据仍指向上游仓库：

```powershell
rpi install rpi-ui-adapter --github pi-rust/rpi-extension
rpi install rpi-providers --github pi-rust/rpi-extension
rpi update
```

用 `rpi uninstall <package>` 卸载通过宿主安装的包。宿主版本要求与源码安装替代方案见各包说明。

自包含的平台包（扩展库、目录与 `rpi-package-install` 二进制）由手动
`release-assets.yml` 与 `release-unix.yml` 入口生成，并附在本仓库 Releases 中，
全程不依赖 shell 脚本：

```bash
# Linux / macOS
tar -xzf rpi-packages-linux-x86_64.tar.gz
cd rpi-packages-linux-x86_64
./rpi-package-install install --source-dir .
```

```powershell
# Windows
Expand-Archive .\rpi-packages-windows-x86_64.zip .
cd .\rpi-packages-windows-x86_64
.\rpi-package-install.exe install --source-dir .
```

开发者也可以克隆本仓库后运行 `task install`。多数扩展使用工作区固定的已发布
`rpi-plugin-sdk` 0.3.20；`rpi-providers` 单独锁定已发布的 SDK 与核心依赖为 0.6.3，
并且只为该包启用 `provider` 特性。构建工作区不需要同级 `pi-rust` 检出。带语音支持的
Linux 构建还需要系统音频与 TLS 开发库（Ubuntu 上为 `libasound2-dev`、`libssl-dev`
与 `pkg-config`）。

如需安装已下载的归档而不手工解压：

```bash
task install-bundle ARCHIVE=dist/rpi-packages-windows-x86_64.zip
```

可选的 `task smoke` 维护者检查会实际走一遍宿主加载，因此需要同级 `pi-rust` 检出。

运行 ACP 单元测试与确定性流式场景：

```bash
task test-acp
```

当 `RPI_TEST_HOST` 与 `RPI_TEST_PLUGIN` 分别指向 `rpi` 可执行程序与构建好的 ACP 库时，
可选的真实宿主生命周期检查也会一起运行。

## 无头飞书/Lark 服务

`rpi-im-message` 通过以下命令启动飞书/Lark 长连接服务：

```powershell
rpi --im-message-server --im-profile feishu-main
```

若 `.rpi/im.json`（或 `~/.rpi/agent/im.json`）中已设置 `defaultProfile`，profile 参数可省略。
启动进程前请通过该 profile 对应的环境变量提供应用密钥。

## 发布与预编译扩展

本仓库会向 crates.io 发布。在 `main` 上手动触发
**Actions → Version and publish crates** 时，release-plz 以 crates.io 已发布版本为基线：
为未发布的改动升版本、重写 changelog、同步 `catalog/packages.json`、运行工作区测试与
`cargo publish --workspace` 干跑，经版本 PR 合入元数据后发布 crate，并创建
`<package>-v<version>` 标签与 GitHub Release，随后构建跨平台产物。

- `mode` 输入默认 `dry-run`，除 `release-plz release` 外执行全部步骤，可在不上传的前提下
  验证整条链路。
- 真正发布需要 `CARGO_REGISTRY_TOKEN` 仓库密钥，并具备工作区内全部 crate 的发布权限。
- crates.io 上已存在的版本会被跳过，只有未发布的改动才会发版。
- 包标签由发布流程创建，且不可变更。

**Actions → Publish GitHub extensions** 作为不需要 crates.io 令牌的纯 GitHub 补发入口保留：
它校验被选包的 `repository` 元数据指向 `pi-rust/rpi-extension`，在创建任何对象前检查全部版本
标签，先上传校验后的归档再上传 `rpi-extension.json` 清单，并在所有平台成功后才公开草稿。
同一批版本不要同时跑两个入口。

发布规则与宿主侧安装协议见
[发布流程与分发协议](docs/prebuilt-distribution.md)；补发入口、可选凭据与 Android 签名见
[GitHub 发布指南](docs/github-releases.md)；发现清单的字段定义见
[JSON schema](docs/rpi-extension.schema.json)。

`.github/scripts`、`catalog/packages.json` 或发布相关工作流发生变化时会运行发布工具检查：

```bash
python3 -m unittest discover -s .github/scripts -p 'test_*.py' -v
python3 .github/scripts/github-release.py plan --repository pi-rust/rpi-extension
```

`plan` 命令只打印本次选择与源提交，不会创建标签或 Release。本地预检仍可使用
`task publish-dry-run`；`task pack` 也会在 `dist/` 下生成可直接安装的平台包。
