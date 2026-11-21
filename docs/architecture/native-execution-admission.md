# Native execution admission (`correlated-v1`)

This is an internal native-host contract, not a replacement for the public API
capabilities in `rpi-extcore::protocol`. Core owns admission and state projection;
HTTP, UI and ACP adapters must not invent settlement rules.

## Native handshake and events

Use strict correlation only when the native ready frame advertises
`semantics.executionLifecycle: "correlated-v1"`. Old native hosts retain the
explicit legacy path. Deploy this extcore change together with the corresponding
pi-rust execution-lifecycle change: old extcore cannot safely consume added
autonomous settlements on its own.

| Identity | Meaning |
| --- | --- |
| `requestId` | Originating native RPC command; null for autonomous plugin work |
| `executionId` | Unique admission lease, including each suspended continuation |
| `runId` | Durable operation; can span multiple execution leases |
| `lane` | Only `main` participates in the session execution admission |

`execution_started` binds a lease to its owner. Its `runId` is null until native
recovery/poll lookup resolves the durable operation; do not confuse the
provisional admission identity with a durable run. Identity-bearing progress belongs
only to that owner. `execution_released` ends native lease ownership, not
necessarily command ownership: the command wrapper must finish cleanup and emit
its matching `agent_settled`. Autonomous work stays busy until its matching
settlement. An admitted successor remains busy when an older command settles.

`AgentEnd` is never authoritative. No timeout, dropped caller, unrelated event
or wildcard `runId` may clear a published correlated intent. `RunTracker`,
HostState and resource/project exclusion consume the same correlated admission
facts. Stale or duplicate settlement cannot overwrite terminal status or output.
Owner collisions, excessive unclosed execution identities and recovery-required
release fail closed. Recovery-required state lasts until host recreation.

A published, **unaccepted** `resume_pending` response with matching request and
command, `success: true` and `data.resumed: false`, is a proven no-op. Clear only
that local intent; never clear another native execution. This small no-op fix
also applies to legacy hosts.

Dialogs are closed by their native owners, not globally erased when an unrelated
execution settles. Session-only context messages are not RPC output of whichever
request happens to be active.

## Validation

```bash
cargo test -p rpi-extcore
# Build in the matching pi-rust checkout first:
# cargo build -p rpi-cli --example execution_admission_fixture
RPI_ADMISSION_NATIVE_FIXTURE="$PI_RUST/target/debug/examples/execution_admission_fixture" \
  cargo test -p rpi-extcore --test admission_native -- --ignored --nocapture
```

Supply an absolute, explicitly trusted executable path. The fixture runs the
real production dispatcher in a child with FauxProvider, starts autonomous work
through the plugin ActionHost's `send_message` path, and verifies no-op resume,
separate RPC/autonomous settlement and successful subsequent admission. It does
not load a DLL or use a real model, credentials or network endpoint. The test
shuts down/reaps its child even when its scenario fails or times out.

Native has additional regression tests for RunEnd callback drains, suspended
leases of the same durable run, lifecycle delivery loss and command-task loss.
The separate external goal DLL smoke remains opt-in and is not implied by this
fixture. These protocol regressions are confirmed independently; the original
user incident still needs its actual wire transcript for root-cause attribution.
