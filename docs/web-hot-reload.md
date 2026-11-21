# Local Web hot-reload environment (macOS host)

`https://work-dev.bigfisharc.cn` (HMR) serves an **isolated development
worktree**; `https://work.bigfisharc.cn` (production) serves the `main`
checkout. Keeping them apart means unmerged work can never reach the production
page, and the main checkout stays clean for other sessions and for merging.

## Checkouts

| role | path | branch | served by |
| --- | --- | --- | --- |
| dev | `/Users/bigfish/work/rpi-projects/rpi-extension-dev` (git worktree of this repository) | `dev/web-hmr` | Vite HMR on `:18087` |
| prod | `/Users/bigfish/work/rpi-projects/rpi-extension` | `main` | `rpi --webserver` on `:18088`, bundle from `packages/rpi-web/assets` |

The backend (its `project`, sessions and both API routes) always belongs to the
main checkout, so a dev-page session reads the same project sessions as
production unless you pick the worktree explicitly in the sidebar.

Create or repair the worktree:

```bash
cd /Users/bigfish/work/rpi-projects/rpi-extension
git worktree add -b dev/web-hmr ../rpi-extension-dev origin/main
cd ../rpi-extension-dev/packages/rpi-web/frontend && npm ci
```

The Vite service (`cn.bigfish.rpi-web-dev`) runs from the worktree; update its
`ProgramArguments` and `WorkingDirectory` only if the worktree moves.

## Topology

```text
browser ──https──> remote Caddy (139.196.85.175, container app-caddy)
                     ├─ work-dev.bigfisharc.cn
                     │    ├─ /api/*  ──> 127.0.0.1:28088 ─┐
                     │    └─ /*      ──> 127.0.0.1:28087 ─┤
                     └─ work.bigfisharc.cn                 │
                          └─ /*  ──> 127.0.0.1:28088 ──────┤
                                                          │
                          SSH reverse tunnel (launchd cn.bigfish.rpi-tunnel)
                            remote :28088 -> mac :18088      │
                            remote :28087 -> mac :18087      │
                                                          v
macOS host
  :18088  rpi --webserver (extension rpi-web, launchd cn.bigfish.rpi-web)
            serves the built bundle from packages/rpi-web/assets (web.json "assets")
  :18087  Vite dev server (launchd cn.bigfish.rpi-web-dev)
            serves packages/rpi-web/frontend/src with HMR
```

The Vite config (`packages/rpi-web/frontend/vite.config.ts`) pins
`port: 18087`, `allowedHosts: ["work-dev.bigfisharc.cn"]`, the `wss` HMR
endpoint on port 443, and proxies `/api` to `127.0.0.1:18088` for direct
(non-Caddy) access.

## Services

| launchd label | program | port | log |
| --- | --- | --- | --- |
| `cn.bigfish.rpi-web` | `python3 ~/.rpi/mac-web/run-web.py` → `rpi --local-only --extension … --webserver=~/.rpi/mac-web/web.json` | 18088 | `~/.rpi/mac-web/logs/cn.bigfish.rpi-web.log` |
| `cn.bigfish.rpi-web-dev` | `node …/rpi-extension-dev/packages/rpi-web/frontend/node_modules/vite/bin/vite.js --host 0.0.0.0 --port 18087 --strictPort` | 18087 | `~/.rpi/mac-web/logs/cn.bigfish.rpi-web-dev.log` |
| `cn.bigfish.rpi-tunnel` | `ssh -N -R 28088:127.0.0.1:18088 -R 28087:127.0.0.1:18087 root@139.196.85.175` | — | `~/.rpi/mac-web/logs/cn.bigfish.rpi-tunnel.log` |

`~/.rpi/mac-web/web.json` is the service configuration: `project` points at the
main checkout, `assets` at its `packages/rpi-web/assets`, sessions/registry live
under `~/.rpi/mac-web` and survive restarts. The dev worktree is intentionally
absent from this file — it only feeds Vite.

## Daily workflow

Develop in the worktree. The launchd Vite service already serves it with HMR on
`:18087`:

```bash
cd /Users/bigfish/work/rpi-projects/rpi-extension-dev/packages/rpi-web/frontend
npm run typecheck
npx playwright test --config=playwright.config.ts -g "<spec name>"
```

Edit files in the worktree; the dev page hot-updates. Commit, push the branch and
open the pull request from the worktree. Do not edit frontend sources in the main
checkout for everyday work: it is the production build source and other sessions
merge there.

Do not start a second `npm run dev`: the config pins `strictPort: 18087`, so it
fails while the launchd service owns the port. Stop the service
(`launchctl bootout gui/$(id -u)/cn.bigfish.rpi-web-dev`) only if you need a
foreground server.

Publishing a merged frontend change to production:

```bash
cd /Users/bigfish/work/rpi-projects/rpi-extension
git switch main && git pull --ff-only
python3 ~/.rpi/mac-web/deploy-web.py --frontend   # bundle into packages/rpi-web/assets
```

`rpi --webserver` re-reads `assets/index.html` on every `GET /`, so a
frontend-only change needs no restart.

Rust/backend or extension change (merge to `main` first — the deploy helper
builds the main checkout):

```bash
# frontend + release build + install + idle check + restart + verify
python3 ~/.rpi/mac-web/deploy-web.py
python3 ~/.rpi/mac-web/deploy-web.py --rust          # frontend assets already built
python3 ~/.rpi/mac-web/deploy-web.py --restart-only  # no build, just restart
python3 ~/.rpi/mac-web/deploy-web.py --frontend      # build bundle, no restart
# deliberate branch test only: build another checkout, e.g. the dev worktree
python3 ~/.rpi/mac-web/deploy-web.py --project ../rpi-extension-dev
```

`--project` makes the production service load an entry that is not on `main`;
use it to try a branch end to end, then redeploy from `main`.

`deploy-web.py` waits for sessions to go quiet before restarting (override with
`--force`), reinstalls `librpi_web_extension-v<Cargo version>.dylib` into
`~/.rpi/agent/extensions`, updates `native-packages.json`, and verifies the
served page carries the new version. Progress is logged to
`/tmp/rpi-web-deploy.log`.

`run-web.py` verifies the installed library (ARM64 Mach-O, code signature,
SHA-256) and keeps the verified copy in `~/.rpi/mac-web/extensions`; stale
copies are pruned.

## Verification

```bash
curl -sS -H 'Host: work.bigfisharc.cn'      http://127.0.0.1:18088/api/v1/health
curl -sS -H 'Host: work-dev.bigfisharc.cn'  http://127.0.0.1:18087/  | head -3   # @vite/client present
curl -sS https://work.bigfisharc.cn/        | grep -o 'index-[A-Za-z0-9_-]*\.js'
curl -sS https://work-dev.bigfisharc.cn/    | grep -o '@vite/client'
```

`packages/rpi-web/scripts/smoke-dev-web.sh` runs an isolated native host check
that never touches these services.

## Troubleshooting

- Service down: `launchctl print gui/$(id -u)/cn.bigfish.rpi-web[-dev] | head -20`
  and the log file above; `launchctl kickstart -k gui/$(id -u)/cn.bigfish.rpi-web`.
- `address already in use` on 18087/18088: something outside launchd owns the
  port; find it with `lsof -nP -iTCP:18087 -sTCP:LISTEN`.
- Public page stale: check the tunnel (`cn.bigfish.rpi-tunnel`) and that remote
  Caddy still routes `work-dev` `/api/*` to 28088 and everything else to 28087.
- HMR websocket fails: `vite.config.ts` `server.hmr.host` must stay
  `work-dev.bigfisharc.cn` with `protocol: "wss", clientPort: 443`; a new host
  also needs an entry in `server.allowedHosts` and `web.json` `allowedHosts`.
- `frontend not built` page: run `npm run build` in
  `packages/rpi-web/frontend` (assets are generated output, not tracked).
