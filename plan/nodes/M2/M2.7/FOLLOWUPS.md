# M2.7 FOLLOWUPS — review-gate MINORs deliberately left in `M2.7`

The four gate **blockers** (fd leak in `recv_fd`, growing destroyed-world registry, supervisor left alive on a
readiness timeout, silently-empty poisoned `ServiceLogs::snapshot`) are fixed and covered by tests — see
`RESULT.md` § "Gate hardening". The items below are **MINOR** and were consciously deferred; none of them changes
behaviour today.

## 1. Dedupe `nsenter` entry construction (`crates/slipfork-engine/src/reflink_netns.rs`)

`entry_prefix()` already builds `--target/--user/--preserve-credentials/--mount/--net/--pid/--wd/--`, and
`entry_command()`, `exec_in_world()` and `spawn_service()` each then assemble a near-identical `Command`
(`env_clear` + `envs` + `SLIPFORK_WORLD_STATE` + piped stdio). `IncusEngine::entry_command()` builds its
`incus exec` prefix inline too.

**Fix:** one private `fn enter_world(&self, init_pid, cwd, state_dir, env, argv) -> Command` used by all three,
plus a shared `WorldEntryCommand` constructor. Cosmetic; deferred to keep this node's diff reviewable.

## 2. Merge `spawn_reader` / `spawn_log_reader` (`crates/slipfork-engine/src/reflink_netns.rs`)

Both drain a `Read` in 4 KiB chunks; one pushes `StreamChunk`s into an `mpsc::Sender` (exec streaming), the other
into `ServiceLogs`. A single reader parameterised over a `FnMut(StreamChunk)` sink would remove the duplicate loop.

**Fix:** `fn spawn_stream_reader<R: Read + Send + 'static>(source: R, kind: StreamSource, mut sink: impl FnMut(StreamChunk) + Send + 'static)`
and adapt both call sites. Behaviour-neutral.

## 3. `ARCHITECTURE.md` unsafe-table accuracy (outside `M2.7` paths)

- The `crates/slipfork-engine` row/§3.3 lists `current_username` / `getpwuid_r(geteuid())` as living in
  `linux_cow.rs`; **no such function exists anywhere in the workspace** (the developer's account name is
  resolved once by the CLI composition root via `UserIdentity`, not by the engine).
- This node added `kill(2)` (`linux_cow::signal_process`, `SIGTERM`/`SIGKILL` for bounded service shutdown and
  world-init teardown), which the table should name alongside `ioctl(FICLONE)`, `current_uid_gid`, and the
  `SCM_RIGHTS` pair.

**Fix:** edit `ARCHITECTURE.md` §3.3 + the crate table (and `ADR-0008` §7) — owned by the conductor/architect,
not by M2.7's path lock.

## 4. `H-011` kernel rationale restated four times (engine docs)

The `setns(CLONE_NEWNET) == EPERM` / `setns(CLONE_NEWUSER) == EINVAL` justification appears in the module docs of
`src/lib.rs`, `src/reflink_netns.rs`, `src/bin/slipfork-world-init.rs` and `src/world_init.rs`.

**Fix:** keep one canonical paragraph in `lib.rs` and link to it (`ARCHITECTURE.md` §3.3 remains the normative
prose). Pure documentation churn; deferred so the code diff stays reviewable.

## 5. `ServiceLogs::follow` lifetime & bounded reads (for `M2.16` / `slipfork logs`)

`follow()` replays the retained window and then blocks until the log closes (service stopped/exited/world
destroyed). `slipfork logs` (no `--follow`) wants `snapshot()` plus the diagnostic `poisoned()`; `--follow`
wants `follow()`. No engine change needed — just make the CLI choose the right accessor and surface
`poisoned()` in `--json`.

## 6. Readiness timeout is a constant (`SERVICE_READY_TIMEOUT = 20 s`)

If per-component readiness budgets become a `snow.yml` concern in `M2.16`, add them to `ServiceSpec`
(`readiness_timeout: Duration`) instead of hiding a knob on the engine constructor.

## 7. `freeze_world` after `destroy_world` returns `WorldGone`

Round-2 hardening made a destroyed world uniformly `WorldGone` (registry entry removed) and reserved
`WorldNotFreezable` for already-sealed worlds. Any downstream node that matched on `WorldNotFreezable` for the
destroyed case (`M2.16`'s `down`/`prune` error mapping) should match `WorldGone` instead.
