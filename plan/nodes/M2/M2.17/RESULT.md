# RESULT M2.17 — CLI part 2: hook rescue UX, logs/shell/db/redis/forward, auth, support, git-remote-slipfork

Status: **blocked (upstream `M2.27` API)** — all five attempt-3 legality fixes are implemented and
were green before `M2.27` rewrote `WorldSpec`/`ServiceSpec`; the current tree needs `M2.29`'s
`WorldRootfs` composition-root injection before the crate can compile again. Every scoped
`crates/slipfork`, `crates/slipfork-edge`, and `crates/slipfork-dag` gate below is green on the
`M2.17` code against the `M2.16` engine API.

## What shipped

| Area | Behaviour |
|---|---|
| Hook failure UX | `BootEvent::HookFailed { id, argv, exit_code, world, log_path }`; `up`/`fork` stream the untruncated stdout/stderr, keep the failed world listed as `failed`, write `<state_dir>/<project>/worlds/<world>/logs/<component>.<phase>.log`, and print the three rescue lines (`hook run`, `logs`, `shell`, including `--no-tty` in non-interactive shells). |
| `hook run` | Interactive default (`stdin.is_terminal()` fail-closed naming `--no-tty`/`slipfork exec`) fetches `WorldExecTarget` from the daemon (`hook_exec_target`) and spawns the entry with `env_clear()` + `ExecEnv` + native `Stdio::inherit()`; `--no-tty` streams via `DaemonControl::hook_run` and propagates the exit code. |
| `logs` | `[--world] [<target>] [--follow] [--stderr] [--lines]` over `DaemonControl::logs`; targets are component services **and** retained hook ids; unknown targets list every candidate and exit non-zero. `--follow` replays retained output then streams. |
| `shell` | Requires a TTY (fails closed otherwise), resolves `$SHELL`, enters the world with native stdio. |
| `db` / `redis` | Resolve the sole declared `pg_databases.*` / `redis_clusters.*` when the positional id is omitted, then request `ServiceExecTargetRequest { cwd, world, service, argv }`. The daemon fails closed `ServiceNotRunning` naming the service and `slipfork up`; it never falls back to the app rootfs. A TTY is required only after the service target succeeds, so a stopped service is the primary diagnostic. |
| `forward` | `DaemonControl::forward` resolves resource/component targets, `WorldRegistry::forward` binds `127.0.0.1:0` listeners **inside `slipforkd`** and bridges them through `dial_world_tcp`; the CLI prints the JSON/human summary immediately and holds the control connection open until disconnect, which drops `ForwardBridge` and closes every host port. A dead world port closes only that accepted connection. |
| `auth login` | `[- | '<url>#handle=<64hex>']`; `-` reads from stdin. The daemon injects the persistent `FallbackDeviceKeySigner` + `OsCsprngEntropy` + `OsppTimeouts`, opens the approval page through the injected `BrowserOpener` (composition-root `xdg-open` adapter), redeems `PairingUrl` once, and prints the full `key_id`/`backend`/`account`. Handles are never logged, serialized in errors, or echoed. |
| `support [--json]` | Reads `SupportRegistry::embedded()` in the CLI; `--json` emits `id`, `category`, `tier`, present `evidence`, and the exact `required_evidence` array for every integration (`IntegrationCategory` gained `Serialize` with the existing `kebab-case` contract). |
| `git-remote-slipfork` | New binary (`src/bin/git-remote-slipfork.rs`) behind `slipfork_vcs::remote_helper`: `slipfork://<fork>` asks `slipforkd` for the live fork's `VcsWorkspace` refs (`DaemonControl::workspace_refs` -> injected `RepoRootResolver` + `VcsWorkspace::resolve`) and advertises/fetches `refs/heads/<fork>` and `refs/slipfork/<fork>/wip`. It shells out to no Git and refuses when the fork world or daemon is absent. |

## Contract additions (`crates/slipfork-edge/src/daemon/**`)

- `DaemonErrorKind::ServiceNotRunning`.
- `ServiceExecTargetRequest` + `DaemonControl::service_exec_target` (default fail-closed method) and
  `DaemonControl::hook_exec_target` (default fail-closed method), with matching wire variants.
- `BootEvent::HookFailed`.
- `From<&EdgeError> for DaemonError` now preserves an already-typed `EdgeError::Control` kind.

`ExecTargetRequest.service: Option<String>` could **not** be added: the protected
`crates/slipfork-edge/tests/critical_edge.rs` constructs `ExecTargetRequest { cwd, world, argv }`
literally, and that file is outside this node's paths (`tools/critical_guard.py` refuses the edit).
The selector therefore lives on the sibling `ServiceExecTargetRequest` with identical semantics.
`M2.29` (which owns `crates/slipfork-edge/src/daemon.rs` and can coordinate the test update) should
fold it into `ExecTargetRequest` when it wires the container entry command.

## Follow-ups (recorded, not silently dropped)

1. **`WorldEngine::service_entry_command` (M2.28):** `service_exec_target` currently fails closed
   for every named service because no backing-service container exists yet. `M2.28`/`M2.29` must
   check the live service set and return the `crun exec` entry; never an app-rootfs entry.
2. **Fetch while a fork world is stopped:** `workspace_refs` only answers for a live fork world
   (the daemon owns the `VcsWorkspace`). A stopped/down fork fails closed naming `slipfork up`;
   reopening an existing workspace without mutating it remains follow-up work for `M2.12`/`M2.29`.
3. **`--host-caches` and container-aware `up`/`down` remain M2.29** as specified.
4. `forward` resource ports follow the current world-local coordinates (`5432`, `6379`, `9000`);
   multi-cluster port offsets arrive with the `M2.28` container service model.

## Attempt 3 legality fixes

1. **No Git side door in `git-remote-slipfork`:** the binary no longer runs `git rev-parse`.
   `DaemonControl::workspace_refs` returns the live fork's `WorkspaceRefs` + resolved OIDs from the
   daemon's injected `RepoRootResolver` and `VcsWorkspace::resolve`; a dead daemon or absent world
   produces a non-zero helper and **no advertised refs** (the test kills `slipforkd` and asserts
   `git ls-remote` is empty while the local fork ref still exists).
2. **Non-self-fulfilling `db`/`redis` test:** asserts the typed `ServiceNotRunning` refusal through
   `--json` *and* that neither `nsenter`, `rootfs`, nor the world worktree path appears, so the
   daemon cannot have called `entry_command` or entered the app rootfs.
3. **`BROWSER` is honored** by `SystemBrowserOpener` before `xdg-open` (environment read only at the
   composition root); the auth test injects a marker-writing `BROWSER` stub and asserts it ran
   without persisting the live pairing URL.
4. **SeaweedFS port** is documented as `ADR-0009` §2's fixed `-port=9000`; `snow.yml` has no
   per-bucket port to read.
5. **Dedupe:** `resolve_hook` is the single hook validation/argv path for both `hook_exec_target`
   and `hook_run` (including the empty-`run_command` guard); `exec` uses `spawn_entry`/`exit_code`;
   shared test harness/fixtures live in `crates/slipfork/tests/support/mod.rs`.

## Verification

All commands run from `/home/remy/offworld-nexus/slipfork` at `b21eda3` + this working tree.

```text
$ cargo test -p slipfork --all-targets
running 16 tests ... test result: ok. 16 passed; 0 failed
running 14 tests ... test result: ok. 14 passed; 0 failed
running 6 tests  ... test result: ok. 6 passed; 0 failed
running 1 test   ... test result: ok. 1 passed; 0 failed

$ cargo test -p slipfork --test critical_cli -- --nocapture hook_failure
test hook_failure_keeps_the_world_and_prints_the_full_diagnostic ... ok
test result: ok. 1 passed; 0 failed; 13 filtered out
$ cargo test -p slipfork --test critical_cli -- --nocapture rescue_commands
test rescue_commands_re_run_and_inspect_the_failed_world ... ok
test result: ok. 1 passed; 0 failed; 13 filtered out
$ cargo test -p slipfork --test critical_cli -- --nocapture logs_follow
test logs_follow_streams_new_component_output ... ok
test result: ok. 1 passed; 0 failed; 13 filtered out
$ cargo test -p slipfork --test critical_cli -- --nocapture forward_bridge
test forward_bridge_streams_a_real_request_and_closes_on_disconnect ... ok
test result: ok. 1 passed; 0 failed; 13 filtered out
$ cargo test -p slipfork --test critical_cli -- --nocapture db_redis_exec
test db_redis_exec_fail_closed_on_a_stopped_service ... ok
test result: ok. 1 passed; 0 failed; 13 filtered out
$ cargo test -p slipfork --test critical_cli -- --nocapture auth_login
test auth_login_never_echoes_the_pairing_handle ... ok
test result: ok. 1 passed; 0 failed; 13 filtered out
$ cargo test -p slipfork --test critical_cli -- --nocapture support_json
test support_json_lists_required_evidence_for_every_integration ... ok
test result: ok. 1 passed; 0 failed; 13 filtered out
$ cargo test -p slipfork --test critical_vcs_remote -- --nocapture git_fetch
test git_fetch_lists_and_fetches_fork_and_wip_refs_through_the_real_binary ... ok
test result: ok. 1 passed; 0 failed

$ cargo clippy -p slipfork --all-targets --no-deps -- -D warnings
Finished `dev` profile ... (exit 0)
$ cargo fmt -p slipfork -p slipfork-edge -p slipfork-dag -- --check
(exit 0)
$ RUSTDOCFLAGS="-D warnings" cargo doc -p slipfork --no-deps
Finished ... Generated .../target/doc/slipfork/index.html (exit 0)
$ uv run --no-project tools/guard.py
guard: OK (340 file(s) checked)
$ uv run --no-project tools/plan.py validate
plan OK
$ uv run --no-project tools/check_docs_honesty.py
check_docs_honesty: OK (8 published doc(s) match real CLI subcommands)
$ uvx --offline zensical build --strict
No issues found
$ uv run --no-project tools/critical_guard.py --base b21eda3 --node M2.17
critical-guard: OK: authorized by M2.17   # before M2.27 modified critical_engine.rs
$ cargo test -p slipfork-edge
test result: ok. 3 passed; 0 failed
test result: ok. 31 passed; 0 failed
```

### Current workspace gate blocked by `M2.27`'s engine API migration (not this node's paths)

Attempt 3's code was fully green before `M2.27` landed its `WorldSpec`/`ServiceSpec` rewrite:
`cargo test -p slipfork --all-targets` passed (16 unit + 14 CLI + 6 slug + 1 VCS), all eight
acceptance filters passed, `cargo clippy -p slipfork --all-targets --no-deps -- -D warnings` was
clean, and `cargo fmt`/rustdoc for `slipfork`/`slipfork-edge`/`slipfork-dag` were clean.

The current tree no longer builds `crates/slipfork` because `WorldSpec` now requires a
`rootfs: WorldRootfs` (M2.27) and no composition-root `BaseImageBuilder` wiring exists yet — that is
exactly `M2.29`'s scope (`crates/slipfork/**`), which is still `ready`. The only remaining compiler
error in this node's files is:

```text
error[E0063]: missing field `rootfs` in initializer of `WorldSpec`
   --> crates/slipfork/src/daemon.rs:621:20
```

`M2.29` must inject the `WorldRootfs` produced by `DockerfileStageBuilder` into `WorldSpec` before
any workspace-wide test/clippy/fmt gate can run again. No fake/in-memory rootfs was substituted.
`ServiceSpec` was migrated to `ServiceRuntime::WorldProcess` so that only the rootfs value is
missing. `make check` additionally stops at `rs-fmt` on the concurrent
`crates/slipfork-engine/tests/critical_rootfs_live.rs`.
