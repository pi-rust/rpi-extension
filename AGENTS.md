# rpi-package project guidance

This repository contains the shared native runtime and protocol adapters for rpi. Before changing architecture, session loading, transcripts, streaming, or adapter boundaries, read:

- `skills/rpi-architecture-boundaries/SKILL.md`
- `docs/architecture/extension-core.md`
- the relevant package README and tests

## Architecture rules

- Keep the dependency direction `rpi-web -> rpi-httpcore -> rpi-extcore -> native rpi --mode rpc` and `rpi-acp -> rpi-extcore`. The `rpi-http` extension crate only maps the plugin ABI (`rpi --http`) onto `rpi-httpcore`.
- Put protocol-neutral runtime semantics, native child lifecycle, request correlation, runs, cancellation, event replay, bounded transcript snapshots, history validation, project/workspace services, registry metadata, and timings in `rpi-extcore`.
- Keep HTTP extraction/serialization, status mapping, cookies/authentication, Host/Origin checks, SSE, and listener lifecycle in `rpi-httpcore`. Keep the plugin flag/lifecycle adaptation in `rpi-http`.
- Keep ACP JSON-RPC, capabilities, content conversion, stop reasons, tool/diff/plan formatting, ACP transport tasks, and Zed integration in `rpi-acp`.
- Keep React/Vite resources, UI row projection, Markdown/tool-card rendering, scrolling/virtualization, Web configuration, assets, and Web entrypoints in `rpi-web`.
- Native transcripts remain owned by the native host; core reads and validates them. Registry/timing files are supplemental metadata, not replacement transcripts.
- Do not put Axum, ACP, frontend, or plugin ABI types in extcore. Do not move `rowsFromMessages()` or presentation logic into extcore.
- Preserve `--webserver`, `--web`, `.rpi/web.json`, environment compatibility, explicit assets, `/api/v1`, HTTP/SSE, and ACP v1 behavior unless a change explicitly includes compatibility tests.

## Local Web frontend workflow

- This checkout is the single source for both Web environments; the pre-migration `rpi-package-dev` worktree no longer exists. Topology, ports and service commands are documented in [docs/web-hot-reload.md](docs/web-hot-reload.md).
- `https://work-dev.bigfisharc.cn` is the Vite dev server with HMR for `packages/rpi-web/frontend` (`launchd cn.bigfish.rpi-web-dev`, port 18087); `https://work.bigfisharc.cn` is the bundle in `packages/rpi-web/assets` served by `rpi --webserver` (`launchd cn.bigfish.rpi-web`, port 18088). Both arrive through the remote Caddy reverse proxy and the `cn.bigfish.rpi-tunnel` SSH tunnel.
- Edit frontend sources here and verify through the dev page and HMR, plus `npm run typecheck` and the relevant Playwright specs. `npm run build` publishes `packages/rpi-web/assets`; the service re-reads `index.html` per request, so a frontend-only change needs no restart.
- Frontend HMR never updates the shared Rust backend or the installed extension. Rust changes need `python3 ~/.rpi/mac-web/deploy-web.py` (build, install, idle check, restart, verify).
- Keep the production service and installed binaries unchanged unless a deploy is explicitly requested.

## Review and change discipline

- Inspect actual source, call sites, diffs, and tests; commit titles are not evidence.
- For shared behavior, define a neutral core API first, then add thin adapter mappings.
- Keep message/history reads bounded and resumable, and preserve the event sequence boundary so snapshot plus live replay cannot lose events.
- The resource creator owns cancellation, timeout, shutdown, and cleanup.
- Do not claim a runtime upgrade or performance fix until the running binary/extension source has been identified and the service has been restarted and measured.
- Architecture-only requests must not modify runtime implementation code. Documentation and tests are allowed when explicitly requested.

Run relevant checks after implementation, including package tests, frontend `npm run typecheck`, and native smoke/integration checks for snapshot, SSE, startup, authentication, ACP streaming, or lifecycle changes.
