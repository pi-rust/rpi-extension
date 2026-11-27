# rpi-lens

A bounded diagnostics extension inspired by `pi-lens`. The `code_lens` tool
runs one allowlisted check: `git diff --check`, `cargo check`,
`cargo fmt -- --check`, or `cargo clippy -- -D warnings`. It does not run tests,
accept arbitrary commands/flags, apply formatting or fix diagnostics.

## `code_lens` input and result

All parameters are optional; `{}` selects `diff` with a 60-second timeout.
Unknown top-level keys (including host-injected `__rpi`) remain allowed.

| Parameter | Default | Contract |
| --- | --- | --- |
| `check` | `"diff"` | One of `diff`, `cargo`, `fmt`, `clippy`, mapped to the commands above. `diff` checks whitespace errors in unstaged tracked changes, not staged/untracked files or a full diff. |
| `cwd` | Host session workspace, with the process working directory as fallback | Existing directory appropriate for the command. Empty values use the default and relative values resolve from it. |
| `timeoutSeconds` | `60` | Integer from 1 to 120. Runtime clamps unsigned integers to that interval and falls back to 60 for other types/values. |

Requires the selected executable on PATH, a Git repository or Cargo project as
appropriate, and installed rustfmt/clippy components for those checks. Cargo
commands may execute build scripts, access the network and write build/cache
artifacts; these are not sandboxed read-only operations.

Success text is `check=<selector> exit=<code>` followed by stdout and then stderr
(not interleaved). Nonzero exit codes still return normal diagnostic output.
Spawn, wait, collection and timeout failures return tool errors. Output is
buffered in full and then capped at 40,000 bytes plus `\n[truncated]`; this is
not a bound on subprocess output production or memory use. Truncation preserves
whole UTF-8 characters.

## Execution limits

The 1–120-second limit covers waiting for the direct child. Timeout kills and
waits for that child only, not its descendants. Cancellation is checked before
execution, not while the synchronous command is running. Output is piped but
not drained until the wait completes, so a verbose child can fill a pipe and
time out. Output collection (including descendant-held pipes) is outside the
wait timeout: this is **not a hard end-to-end deadline** or process-tree cleanup
guarantee. No streaming or partial output is returned on timeout.
