# rpi-extension

**English** | [简体中文](README.zh-CN.md)

Rust-native extension packages for the `rpi` agent, built against the stable
`rpi-plugin-sdk` ABI from `pi-rust`.

This repository imports the `pi-rust/rpi-package` main tree and is the crates.io
publisher for these extensions, exactly like the upstream repository. Release
plz bumps versions, updates changelogs and the catalog, then publishes to
crates.io and creates per-package tags in **Actions → Version and publish
crates** (dispatch `main`). The `mode` input defaults to `dry-run`, which
validates the whole path without uploading; choose `publish` to release. It
requires the `CARGO_REGISTRY_TOKEN` secret. Prebuilt archives and
`rpi-extension.json` manifests are then attached to
[Releases](https://github.com/pi-rust/rpi-extension/releases) for six target
platforms. See [the release guide](docs/github-releases.md).

The workspace includes tools, workflow extensions, provider integrations and
client adapters. Each package has its own version and setup guide. The
[package catalog](catalog/packages.json) records installation commands and
versioned prebuilt manifests for catalog extensions. See the
[tool guidance contract](docs/tool-guidance.md) for model-facing descriptions,
parameter schemas, and regression checks.

## Packages

| Package | Tools and capabilities |
| --- | --- |
| [rpi-mcp-adapter](packages/rpi-mcp-adapter/README.md) | Discover/call tools and manage resources with `mcp_list`, `mcp_call`, `mcp_resource`; raw diagnostics with `mcp_request` |
| [rpi-mcp-server](packages/rpi-mcp-server/README.md) | Publish the agent as an MCP server with `--mcp`, or run the standalone `rpi-mcp-bridge` |
| [rpi-subagents](packages/rpi-subagents/README.md) | Delegate work with `delegate_task` |
| [rpi-codemode](packages/rpi-codemode/README.md) | Compose native tools with bounded JavaScript, lossless business JSON and explicit exact native-result selection within inline limits; published/prebuilt extension, requires the companion ToolProgram v1 capabilities |
| [rpi-orchestrator](packages/rpi-orchestrator/README.md) | Bounded child agents and task graphs with `subagent`, `plan` and `dispatch` |
| [rpi-lens](packages/rpi-lens/README.md) | Inspect code with `code_lens` |
| [rpi-codegraph](packages/rpi-codegraph/README.md) | Query indexed symbols, source, and relationships with `codegraph_*` (automatically initializes/syncs the index) |
| [rpi-todo](packages/rpi-todo/README.md) | Track tasks with `todo` |
| [rpi-background-tasks](packages/rpi-background-tasks/README.md) | Run long commands with `background` and inspect them with `jobs` |
| [rpi-goal](packages/rpi-goal/README.md) | Maintain a persistent project objective with `goal` |
| [rpi-optchat](packages/rpi-optchat/README.md) | Recall this session's chat memory with `zoom`, `search` and `/optchat` |
| [rpi-plan-mode](packages/rpi-plan-mode/README.md) | Plan work with `/plan`, `plan_mode_start` and `plan_mode_complete` |
| [rpi-btw](packages/rpi-btw/README.md) | Read-only side conversations with `/btw`; requires rpi 0.6.0 or newer |
| [rpi-ask-user](packages/rpi-ask-user/README.md) | Ask interactive questions with `ask_user` |
| [rpi-permissions](packages/rpi-permissions/README.md) | Manage tool policies with `permissions` |
| [rpi-permission-gate](packages/rpi-permission-gate/README.md) | Confirm `bash`, `write` and `edit` through host UI dialogs |
| [rpi-websearch](packages/rpi-websearch/README.md) | Search the web with `websearch` |
| [rpi-webfetch](packages/rpi-webfetch/README.md) | Fetch web content with `webfetch` |
| [rpi-im-message](packages/rpi-im-message/README.md) | Run a headless Feishu/Lark service |
| [rpi-voice](packages/rpi-voice/README.md) | Dictation and spoken replies with `/voice`; offline STT enabled by default |
| [rpi-run-stats](packages/rpi-run-stats/README.md) | Display live usage, timing and cost in the TUI |
| [rpi-langfuse](packages/rpi-langfuse/README.md) | Export traces and manage scores and prompts with Langfuse |
| [rpi-providers](packages/rpi-providers/README.md) | Add DeepSeek, OpenRouter, llama.cpp and Proxy providers |
| [rpi-server](packages/rpi-server/README.md) | Serve TCP JSONL or WebSocket requests and streaming subscriptions with `--server` (`--server-ws`) |
| [rpi-acp](packages/rpi-acp/README.md) | Connect ACP clients, including Zed and Open WebUI Computer |
| [rpi-ui-adapter](packages/rpi-ui-adapter/README.md) | Connect Open WebUI directly through `--webui=openwebui` |
| [rpi-node-core](packages/rpi-node-core/README.md) | Protocol-neutral node identity, discovery and capability contracts |
| [rpi-node](packages/rpi-node/README.md) | Native `rpi --node` lifecycle and open protocol/discovery registries |
| [rpi-edit-diagnostics](packages/rpi-edit-diagnostics/README.md) | Replace `edit` with exact matching and compiler-style failure reports |

Durable workflow state is owned by the native host; packages do not provide a second durable runtime.

Extensions inspired by Pi packages are independent Rust implementations, not
ports of upstream source code, except the candidate search in
`rpi-edit-diagnostics`, which is ported from pi-semantic-edit (MIT; see its
`NOTICE`).

`rpi-voice` embeds sherpa-onnx + SenseVoice for offline speech recognition by
default. OpenAI-compatible STT is also available; see the
[voice guide](packages/rpi-voice/README.md) for model setup, build options and
speech output configuration.

`rpi-run-stats` adds a live monitoring row below the usage footer: turns, LLM calls, tool executions,
output tok/s, time to first token, response time, rolling 60-second TPM,
input/output/cache tokens, errors and USD cost. Use `/stats on`, `/stats off`
or `/stats` to toggle it, or press F8. The row uses compact monochrome symbols
in three groups and retains measured timing between requests. USD accumulates model usage costs calculated by the host from
model pricing. It uses the host's generic `UiComponent` text interface.
See [rpi-run-stats](packages/rpi-run-stats/README.md) for measurement definitions.

Code Mode's **first full-parity increment is implemented, not the full plan**.
The extension requires exact version-1 `resultEnvelope`, `hostAccounting`,
`parentProgress` and `losslessValues` capabilities and fails closed on older
hosts. Local evidence: **72 package tests passed**, strict all-target package
Clippy passed, **1109 host tests passed / 6 ignored / 0 failed**, host formatting
passed, and **35 real Windows host + DLL + fixture + gate smoke cases passed**.
Final results and binary hashes are at
`pi-rust-codemode/target/codemode-full-parity-smoke-5/`; this is source-build
qualification, not deployment or a new CI-green claim. Large values
beyond 1 MiB, complete child history/progress/recovery and Web/ACP child
cards/reload remain outstanding; self/ancestor tools remain excluded.
See the [parity status matrix](docs/codemode-full-parity-matrix.md).

## Provider integrations

`rpi-providers` is one independently loaded package for DeepSeek, OpenRouter,
llama.cpp and Proxy. It keeps a single native plugin entry and separate
provider IDs, model catalogs and authentication policies. Install or upgrade
one package; maintain vendor implementations in separate modules.

```bash
rpi install rpi-providers --github pi-rust/rpi-extension
```

Starting with 0.1.2, this package is distributed as prebuilt archives through
GitHub Releases. It uses `rpi-plugin-sdk` 0.6.3 with the optional `provider`
feature and matching `rpi-ai` 0.6.3; use the rpi 0.6.3 host for this package.
See [the package guide](packages/rpi-providers/README.md) for compatibility,
source linking, migration and tests.

## Web workspace (rpi-web)

`rpi-web` provides project-based conversations in a browser, with streaming replies, tool interactions, Markdown rendering, file previews, and Git diffs. The interface supports English and Simplified Chinese. It supports a native extension entry point and a standalone program, sharing the same Web core.

The per-extension archive for a published version is on this repository's
[Releases](https://github.com/pi-rust/rpi-extension/releases?q=rpi-web-v). The
portable Web bundle with the extracted `assets/` directory is produced by the
manual `release-web.yml` workflow; run it from `main` before following the
standalone start commands. Install the [rpi 0.7.0 host](https://github.com/bigfish1913/pi-rust/releases/tag/v0.7.0) and configure model credentials for the service process before starting:

```bash
# Run from the extracted Web directory with an existing project directory.
./rpi-web --project /path/to/project --rpi-bin /path/to/rpi --assets ./assets --port 3030
```

Open [http://127.0.0.1:3030/](http://127.0.0.1:3030/), select a project, and create a session. On Windows, use `rpi-web.exe` and `rpi.exe`.

The [Web user guide](packages/rpi-web/README.md) covers Windows / Linux / macOS setup, project creation, model credentials, password login, remote access, persistence, and upgrades. Web 0.1.7 uses `rpi --webserver` to start the native extension with embedded frontend resources; `--web` remains a compatibility alias. Install it with `rpi install rpi-web --github pi-rust/rpi-extension`, or from a checkout using `rpi install rpi-web --path packages/rpi-web`.

Web 0.1.1 supports `.rpi/web.json` startup settings. Run `rpi-web --init` once in a project directory, configure the host and assets paths, then use `rpi-web` for subsequent starts. Global defaults can be stored in `~/.rpi/web.json`, and CLI arguments provide temporary overrides. Users of Web 0.1.0 must upgrade to use configuration files.

## Shared backend and HTTP API

`rpi-extcore` now holds the common ACP/Web native RPC, run, session, history, event and workspace capabilities. `rpi-httpcore` adapts those services to the existing HTTP/SSE API and provides an API-only executable. `rpi-web` adds the embedded browser frontend; `rpi-acp` maps the common runtime to ACP.

```bash
cargo run --locked -p rpi-httpcore -- --project /path/to/project --rpi-bin /path/to/rpi --port 3030
```

`rpi-http` packages the same API-only service as a standard native plugin
(`rpi --http`) through the `rpi-httpcore` library. It only maps the existing
plugin lifecycle onto `rpi-httpcore`; it starts a separate core `Service` and
does not share the live Agent session that loaded the extension. The
`rpi-http-service` executable ships the same API-only service without the plugin
host.

`rpi-extcore` is also published as a reusable crates.io library. Applications
can depend on the compatible minor line with `rpi-extcore = "0.1"`; it requires
Rust 1.78+ and a compatible native `rpi --mode rpc` executable at runtime. See
[rpi-extcore](packages/rpi-extcore/README.md), [rpi-httpcore](packages/rpi-httpcore/README.md) and the [architecture contract](docs/architecture/extension-core.md). The new packages are ordinary Rust libraries; existing extension installation and startup commands remain the same.

For frontend work, the local hot-reload topology (development worktree, production checkout, ports, launchd services and the deploy helper) is documented in [the Web hot-reload guide](docs/web-hot-reload.md).

For frontend work, the local hot-reload topology (dev HMR page, production page, ports, launchd services and the deploy helper) is documented in [the Web hot-reload guide](docs/web-hot-reload.md).

## MCP server

`rpi-mcp-server` is the reverse direction of `rpi-mcp-adapter`: it publishes the
agent to MCP clients instead of consuming MCP servers.

```text
MCP client -> MCP over stdin/stdout -> rpi-mcp-bridge -> rpi --mode rpc
             (or `rpi --mcp`, which reuses the host's `ownsStdio` ABI)
```

The package owns the MCP JSON-RPC framing, tool catalog, bounded session
resources (`rpi://session/transcript`, `rpi://session/state`) with
`resources/subscribe` updates, `tools/call` concurrency and cancellation
mapping. Native session lifecycle, prompt
acceptance, settlement and child cleanup stay in `rpi-extcore`. The standalone
bridge is the recommended integration because it owns its own stdin/stdout and
therefore needs no host stdio arbitration. The same bridge can serve MCP
Streamable HTTP (`--http <addr>`, `POST /mcp`) for clients that cannot spawn a
process, plus a private WebSocket channel (`GET /mcp/ws`) for our own clients.

`rpi install rpi-mcp-server` installs the extension (`rpi --mcp`). The
recommended entry point, the `rpi-mcp-bridge` executable, ships as a standalone
per-platform archive next to the ACP bridge:

```bash
task pack-mcp   # dist/rpi-mcp-<platform>.tar.gz with the binary at the root
```

See the [MCP integration guide](docs/mcp-server.md) for transport choice, client
configuration, trust boundaries and troubleshooting, and the
[MCP server package README](packages/rpi-mcp-server/README.md) for the exact tool
arguments and limits.

## Native ACP integration

`rpi-acp` contains the ACP protocol, session history, streaming, question handling
and Zed registration. The extension and standalone `rpi-acp-bridge` use the same
implementation within this package.

```text
Computer / Zed -> ACP over stdin/stdout -> rpi --acp -> rpi --mode rpc
                                                       (one child per session)
```

Install the extension with the `rpi` host:

```bash
rpi install rpi-acp
```

For **Open WebUI Computer**, deploy Computer separately and configure its native
ACP agent to launch the `rpi` executable with `--acp`. The process must have
access to the installed extension, provider credentials, project directory and
writable session storage. Tools run in the workspace sent by the client's
`session/new` request. Container deployments must make that same path available
to the agent process.

Set `RPI_ACP_NO_AUTO_REGISTER_ZED=1` in Computer's agent environment to disable
automatic Zed registration. Set `RPI_ACP_SESSION_DIR` to an absolute directory
when session history needs a persistent container volume. See the
[ACP package README](packages/rpi-acp/README.md#configure-computer-native-acp)
for the connection requirements and client capabilities.

For **Zed**, run `rpi --install-zed` to register the agent or `rpi --print-zed`
to print the settings snippet. Registration preserves comments and creates a
backup when updating an existing file.

The standalone bridge owns stdin/stdout and starts `rpi --mode rpc` itself.
Build its archive with `task pack-acp`; use `RPI_BIN` to select the backend
executable. Its packaging and proposed ACP Registry entry are documented in
[registry/README.md](registry/README.md).

## UI adapters

`rpi-ui-adapter` adds `rpi --webui=openwebui` to connect an existing Open WebUI
instance directly to rpi ACP, with streaming, progress, tool details and cancellation.
Each UI has its own project configuration in `.rpi/webui/<UI>.json`;
`--webui` only selects the adapter.
It automatically installs a managed Pipe; Computer is not required. rpi and
the project can remain outside the UI container. See the
[Chinese setup guide](packages/rpi-ui-adapter/README.md) for configuration examples,
container networking and session behavior.

## Build and install

Source builds use stable Rust and Cargo. Task is required for the `task`
commands; Python 3 is required for the ACP integration tests. The `rpi` host
must already be installed and configured to run the extensions.

```powershell
cargo build -p rpi-providers --locked
cargo test --workspace --locked
task install
```

Build `rpi-providers` before running its integration tests, which load the
actual native library.

`task install` builds the workspace and copies catalog extensions to
`~/.rpi/agent/extensions`, or `$RPI_CODING_AGENT_DIR/extensions` when configured.
The CLI discovers the default directory automatically; an
explicit directory also works:

```powershell
rpi --extensions-dir "$HOME/.rpi/agent/extensions"
```

These are `cdylib` extensions, so plain `cargo install` is not the right
consumer installation mechanism. The cross-platform consumer command is built
into the `rpi` host:

```powershell
rpi install rpi-todo --github pi-rust/rpi-extension
rpi install rpi-todo --github pi-rust/rpi-extension --version 0.3.4
```

For GitHub prebuilt installation, select this repository explicitly, because
the crates.io metadata of these packages still points at the upstream
repository:

```powershell
rpi install rpi-ui-adapter --github pi-rust/rpi-extension
rpi install rpi-providers --github pi-rust/rpi-extension
rpi update
```

Use `rpi uninstall <package>` to remove a package installed through the host.
Check each package guide for host requirements and source-install alternatives.

A self-contained platform bundle (extension libraries, catalog and the
`rpi-package-install` binary) is produced by the manual `release-assets.yml` and
`release-unix.yml` entry points and attached to this repository's Releases. No
shell script is involved:

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

Developers can instead clone this repository and run `task install`. Most
extensions use the published `rpi-plugin-sdk` 0.3.20 pinned in the workspace;
`rpi-providers` independently pins the published SDK and core dependencies to
0.6.3, enabling `provider` only for that package. A sibling `pi-rust` checkout
is not needed for the workspace build. Linux builds with voice support also need native audio
and TLS development libraries (`libasound2-dev`, `libssl-dev` and `pkg-config`
on Ubuntu).

To install an already-downloaded archive without unpacking it by hand:

```bash
task install-bundle ARCHIVE=dist/rpi-packages-windows-x86_64.zip
```

The optional `task smoke` maintainer check exercises the host loader and does
require a sibling `pi-rust` checkout.

To test ACP unit tests and deterministic streaming scenarios, run:

```bash
task test-acp
```

Optional real-host lifecycle checks also run when `RPI_TEST_HOST` and
`RPI_TEST_PLUGIN` point to the `rpi` executable and built ACP library.

## Headless Feishu/Lark service

`rpi-im-message` runs the Feishu/Lark long-connection server with:

```powershell
rpi --im-message-server --im-profile feishu-main
```

The profile is optional when `defaultProfile` is set in `.rpi/im.json` (or
`~/.rpi/agent/im.json`). Set the app secret through the profile's environment
variable before starting the process.

## Releases and prebuilt extensions

This repository publishes to crates.io. **Actions → Version and publish crates**
(`workflow_dispatch` on `main`) runs release-plz against the published registry
baseline: it bumps versions for unpublished changes, regenerates changelogs,
synchronizes `catalog/packages.json`, tests the workspace, dry-runs
`cargo publish --workspace`, merges the prepared metadata through a version pull
request, publishes the crates and creates the `<package>-v<version>` tags and
GitHub Releases. The workflow then builds the cross-platform assets.

- The `mode` input defaults to `dry-run`, which performs every step except
  `release-plz release`, so the pipeline is verifiable without uploading.
- Publishing needs the `CARGO_REGISTRY_TOKEN` repository secret with permission
  for every crate in the workspace.
- Versions already on crates.io are skipped; only unpublished changes release.
- Package tags are created by the release and are immutable.

**Actions → Publish GitHub extensions** remains available as a GitHub-only
backfill that needs no crates.io token. It validates that each selected catalog
package declares `repository = https://github.com/pi-rust/rpi-extension`, checks
all version tags before mutating anything, uploads validated archives before the
`rpi-extension.json` discovery manifest, and publishes drafts only after every
supported platform succeeds. Do not run both entries for the same versions.

See the [release workflow and distribution protocol](docs/prebuilt-distribution.md)
for the publishing rules and the host-side install protocol, the
[GitHub release guide](docs/github-releases.md) for the backfill path, optional
credentials and Android signing, and the [JSON schema](docs/rpi-extension.schema.json)
for the discovery manifest.

Release tooling checks run on every change to `.github/scripts`,
`catalog/packages.json` or the release workflows:

```bash
python3 -m unittest discover -s .github/scripts -p 'test_*.py' -v
python3 .github/scripts/github-release.py plan --repository pi-rust/rpi-extension
```

The `plan` command prints the selection and source commit without creating tags
or releases. `task publish-dry-run` remains available for a local preflight.
`task pack` also creates a directly installable platform bundle under `dist/`.
