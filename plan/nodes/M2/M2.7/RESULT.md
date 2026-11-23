# M2.7 RESULT — DONE (attempt 2): persistent world init, `netns.sock` dialer, `:80` bridge, service supervision

**Status:** done, attempt 2/3. Base `1b4cf79` (+ `db1b400` H-011 spec ratification). All changes are confined to
`crates/slipfork-engine/**`; `crates/slipfork/src/main.rs` was **not** touched and still compiles (see follow-up 4).
Attempt 1's blocker (no daemon thread can `setns` into an unprivileged world `netns`) is resolved by the `H-011`
ruling: the world's own persistent PID 1 owns every namespace-scoped capability.

## What landed (per NODE.md invariant)

| # | Invariant | Implementation |
|---|---|---|
| 1 | Persistent per-world PID 1 + freeze ordering (`H-011` #2) | `src/bin/slipfork-world-init.rs` (`#![forbid(unsafe_code)]`) is spawned by `ReflinkNetnsEngine::boot_world` inside `unshare --map-user=$UID --map-group=$GID --keep-caps --mount --net --pid --fork --mount-proc <helper> <world-init.json>`. It brings `lo` up, writes `0` to `/proc/sys/net/ipv4/ip_unprivileged_port_start`, applies every `WorldSpec` mount once (`mount --bind`, `remount,bind,ro` for `ReadOnly`), binds its listeners, then writes `<world_dir>/init.ready`. `clone_world` discovers the PID-1 host pid from `/proc/<unshare>/task/<unshare>/children` (fail-closed if unreadable) and records it; `freeze_world` stops every live service **before** it sets `Freezing` and reflinks `state_dir`; `destroy_world` `SIGKILL`s PID 1 (the kernel destroys the whole PID namespace), reaps the launcher and waits until `/proc/<init_pid>` disappears. |
| 2 | `spawn_service` / `stop_service` + retained `ServiceLogs` | `spawn_service` starts the service through `nsenter --target <init_pid> --user --preserve-credentials --mount --net --pid --wd=<cwd> --`, layered over the world's `base_env ⊕ ServiceSpec.env` with only the inherited allowlist. stdout/stderr are captured per service into `crate::service::ServiceLogs` (size-capped 64 KiB ring, `snapshot()` + blocking `follow()` that replays then tails until close). `ServiceState { Running, Exited, Stopped }` is exposed on `ServiceHandle::state()`; a watcher thread flips `Exited` when the process dies on its own and **never restarts** it. `stop_service` resolves the forked in-world pid (`/proc/<nsenter>/children`, never the launcher parent), sends `SIGTERM`, waits `SERVICE_STOP_GRACE = 5 s`, then `SIGKILL` + `child.kill()`, is idempotent on repeat calls, and **preserves** the captured log. |
| 3 | Streaming `exec_in_world` + direct `entry_command` (`H-010` #1) | `exec_in_world` enters the same persistent init (`nsenter … --pid`) and streams interleaved `StreamChunk`s while returning the full, untruncated `ExecOutput`; the old throw-away `unshare` shell wrapper (`WORLD_INIT_SCRIPT`) is deleted. `entry_command` returns `{ program: "nsenter", args_prefix: [--target=<init_pid>, --user, --preserve-credentials, --mount, --net, --pid, --wd=<cwd>, --], cwd }`. |
| 4 | `dial_world_tcp` over `<world_dir>/netns.sock` (`H-011` #3) | The helper binds a `0600` pathname `AF_UNIX` socket `<world_dir>/netns.sock` (never abstract), reads `port: u16` (BE), connects `127.0.0.1:<port>` in-world and hands the connected stream back with one audited `sendmsg`/`recvmsg` `SCM_RIGHTS` pair in `linux_cow.rs` (`send_fd`/`recv_fd`, per-call `// SAFETY:` proofs, `MSG_CMSG_CLOEXEC`). `ReflinkNetnsEngine::dial_world_tcp` returns `TcpStream::from(OwnedFd)`; a refused in-world port is `EngineError::DialFailed { world, port, errno }`; dialing a destroyed world is `WorldGone`. |
| 5 | In-netns `$BASE_URL` `:80` bridge (`H-011` #4 Variant a) | With `WorldSpec.local_ingress = Some(LocalIngressBridge { port, host_ingress_sock })` the helper binds `127.0.0.1:<port>` (including `:80`) and splices each accepted stream bidirectionally with half-close propagation to the pathname `AF_UNIX` `<state_root>/http-ingress.sock`. `WorldSpec::validate` rejects an ingress socket path beyond `UNIX_SOCKET_PATH_MAX = 107` with `InvalidWorldSpec`; `boot_world` rejects an overlong `<world_dir>/netns.sock` with `WorldInitFailed` instead of dying inside the helper. |
| 6 | One `unsafe` module, no engine-kind ladders | `linux_cow.rs` remains the only module with `unsafe`: `ioctl(FICLONE)`, `current_uid_gid`, `signal_process` (SIGTERM/SIGKILL for bounded shutdown) and the single `send_fd`/`recv_fd` `SCM_RIGHTS` pair. `pty.rs` does not exist. `IncusEngine` implements the new trait methods honestly: `entry_command` = `incus exec <instance> --cwd <cwd> --`, while `spawn_service`/`stop_service`/`service_logs`/`dial_world_tcp` fail closed with `EngineError::Unsupported { engine: "incus", .. }` until the live adapter lands in `M2.21` — never emulated. |

Deleted in the same sweep (zero pre-`1.0` cruft): `WorldSpec::local_ingress_port` → `local_ingress:
Option<LocalIngressBridge>` (the `ARCHITECTURE.md` §3.3 shape), the `WORLD_INIT_SCRIPT` shell string, the
per-exec `unshare` scope, and the unused `WorldRecord::mounts`.

## Gate hardening (round 2, four items — no redesign)

| # | Finding | Fix |
|---|---|---|
| 1 | `recv_fd` overwrote `received_fd` per CMSG header (a peer could leak descriptors) | `recv_fd` now walks **every** `SCM_RIGHTS` slot (count derived from `cmsg_len`), collects `OwnedFd`s, accepts exactly one, and rejects anything else with `io::ErrorKind::InvalidData` *after* dropping the collected descriptors. Tests: `recv_fd_rejects_extra_descriptors_without_leaking_them` (2-fd `sendmsg`, asserts `InvalidData` and `/proc/self/fd` count does not grow) and `recv_fd_accepts_exactly_one_descriptor`. |
| 2 | destroyed `WorldRecord`s were never removed (unbounded registry in a long-lived daemon) | `destroy_world` drops the map entry after the world dir is removed, for **both** engines. A destroyed world is now uniformly `EngineError::WorldGone` (post-destroy `freeze_world` included); `WorldNotFreezable` is reserved for already-sealed worlds. `freeze_world`'s late race path also removes a half-claimed snapshot when the record vanished. Contract + conformance suite updated. |
| 3 | a readiness timeout returned `Err` but left the supervisor alive and the record `Running` (a retry hit `ServiceAlreadyRunning`) | new `abort_service` helper `SIGKILL`s the in-world pid + supervisor, reaps it, marks the record `Stopped` and closes the retained log before the typed timeout error is returned; the world-level error path aborts too. Test: `readiness_timeout_stops_the_supervisor_and_releases_the_service_id` (timeout → log closed → same id retried successfully). |
| 4 | `ServiceLogs::snapshot()` used `unwrap_or_default()` on a poisoned lock (silently empty logs) | every log lock now recovers the guard (`PoisonError::into_inner`) so a panicking engine thread can never erase a failed service's output, and `ServiceLogs::poisoned()` (also in `Debug`) exposes the diagnostic. Unit test `poisoned_log_still_serves_captured_output_and_reports_the_diagnostic` proves output captured before *and after* a poison is still returned and `follow()` drains it. |

`make check` was red during the first hardening pass only because of the then-uncommitted `M2.11`
`crates/slipfork-vcs/tests/support/mod.rs` edit; it now exits `0` (see below).

## DI / composition notes

- `ReflinkNetnsEngine::with_world_init_program(state_root, uid, gid, program)` is the injection seam for the
  helper path (tests use `env!("CARGO_BIN_EXE_slipfork-world-init")`; the daemon will use it when the helper is
  not co-located). `ReflinkNetnsEngine::new(..)` keeps its M2.6 three-argument signature and resolves the
  helper next to the running engine executable — that is why `crates/slipfork/src/main.rs` needed no edit.
- The helper's argv is one `0600` JSON config path (`WorldInitConfig`, no secrets — mounts, socket, bridge);
  `ExecEnv` secrets keep their sanctioned `Command::envs(..)` boundary and never travel over a socket.
- No `match engine_kind`, no new globals: per-world state stays in the engine registry, and the helper reads only
  its own argv/config (it is its own process entrypoint).

## Measured host evidence (this machine, kernel facts first, then the shipped tests)

Pre-implementation probes (real processes, no mocks):

```text
# /tmp/opencode/m27/probe2.py — persistent unshare world + python in-world agent
holder(host pid)= 2209397
init_pid= 2209398 ns: {'user': 'user:[4026532885]', 'mnt': 'mnt:[4026532887]',
                       'net': 'net:[4026532889]', 'pid': 'pid:[4026532888]'}
host ns: {'user': 'user:[4026532460]', 'net': 'net:[4026532467]'}
netns.sock stat: 0o600
dial status= b'\x00' fds= 1                     # SCM_RIGHTS descriptor received
dialed stream bytes= b'from-world'              # the handed-off TcpStream is live
world_server exit= 0
entry: 0 pidns-init=13570588 uid=1000 gid=1000  # repeated nsenter --target <init_pid>, stable
in-netns :80 -> 0 b'HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\nhost-ok'   # :80 splice to host AF_UNIX

# /tmp/opencode/m27/probe3.py — nsenter --pid fork semantics
quick: 0 inner-pid=3                            # $$ inside the world pid namespace
propagated exit: 7                              # nsenter mirrors the forked child's exit status
host children of nsenter: ['2209859']           # the service pid to SIGTERM
nsenter rc after SIGTERM to inner: -15          # launcher exits once the forked child dies

# /tmp/opencode/m27/probe4.py — teardown
init alive False inner alive False nsenter poll -9   # SIGKILL to PID 1 destroys the namespace
```

Shipped critical tests (`cargo test -p slipfork-engine --test critical_engine`, real FICLONE + real
namespaces, zero mocks):

- `persistent_init_pid_stays_stable_across_calls_and_is_reaped_on_destroy`: `/proc/1/cmdline` inside the world
  is `slipfork-world-init`, `/proc/<init_pid>/ns/pid` inode is identical across 4 `exec_in_world` +
  `entry_command` rounds, and `/proc/<init_pid>` is gone after `destroy_world` (`WorldGone` afterwards).
- `dial_world_tcp_reaches_in_world_listener_via_netns_sock_scm_rights`: `spawn_service` readiness itself dials
  `:18080`; a direct dial writes `ping` and reads `PONG`; a refused port is `DialFailed { port: 18099 }`; the
  dial socket on disk is mode `0600`; dialing the destroyed world is `WorldGone`.
- `two_worlds_isolate_the_same_port_and_dial_independently`: both worlds bind `127.0.0.1:18432` and each dial
  returns its own payload (`world-a` / `world-b`).
- `in_netns_bridge_port_80_reaches_host_ingress_and_dials_back_into_the_world`: host leg
  `dial_world_tcp(18081)` returns the app body; in-netns leg `127.0.0.1:80` → `http-ingress.sock` →
  `dial_world_tcp(18081)` returns `bridge-ok` (real `bash` `/dev/tcp` client inside the world netns).
- `service_logs_survive_idempotent_stop_and_freeze_seals_only_after_stop`: size-capped log capture,
  `follow()` replays and returns on stop, second `stop_service` is `Ok`, logs still readable, `freeze_world`
  flips the live writer to `Stopped`, `state_dir/live.log` size is unchanged 150 ms after freeze, and the sealed
  snapshot clone has the identical size.
- `entry_command_enters_the_persistent_init_pid_with_native_stdio`: the returned `nsenter` prefix is spawned
  directly with piped stdio, keeps host `uid`, the same `/proc/1/ns/pid` inode as `exec_in_world`, and the
  requested `cwd`.
- `world_init_fails_closed_when_the_helper_is_missing`: missing helper → `WorldInitFailed`, no
  half-materialized world directory.
- `multi_mount…`, `freeze_lineage…`, `redacted…`, `r10_tmpfs…`, `r10_r11_…same_port`, and the Incus protocol
  harness all keep passing against the persistent-world design.

## `# Acceptance` — exact outputs

```text
$ cargo build -p slipfork-engine --bins
   Compiling slipfork-engine v0.1.0 (/home/remy/offworld-nexus/slipfork/crates/slipfork-engine)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.48s

$ cargo test -p slipfork-engine --all-targets
running 14 tests
test readiness_timeout_stops_the_supervisor_and_releases_the_service_id ... ok
test r10_tmpfs_fails_closed_without_silent_byte_copy_fallback ... ok
test r12_incus_engine_protocol_harness_matches_conformance_contract ... ok
test redacted_exec_env_and_opaque_world_handles ... ok
test entry_command_enters_the_persistent_init_pid_with_native_stdio ... ok
test world_init_fails_closed_when_the_helper_is_missing ... ok
test multi_mount_repo_rw_and_readonly_artifact_volume_are_real_bind_mounts ... ok
test persistent_init_pid_stays_stable_across_calls_and_is_reaped_on_destroy ... ok
test freeze_lineage_seals_once_and_sibling_clones_stay_isolated ... ok
test service_logs_survive_idempotent_stop_and_freeze_seals_only_after_stop ... ok
test in_netns_bridge_port_80_reaches_host_ingress_and_dials_back_into_the_world ... ok
test dial_world_tcp_reaches_in_world_listener_via_netns_sock_scm_rights ... ok
test two_worlds_isolate_the_same_port_and_dial_independently ... ok
test r10_r11_reflink_netns_conformance_cow_isolation_uid_and_parallel_same_port ... ok
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 20.73s
   (+ `src/lib.rs` unit tests, 3 passed: recv_fd extra-descriptor rejection/no-leak, recv_fd single
      descriptor, poisoned service-log output/diagnostic)

$ cargo test -p slipfork-engine --test critical_engine -- --nocapture persistent_init
test entry_command_enters_the_persistent_init_pid_with_native_stdio ... ok
test persistent_init_pid_stays_stable_across_calls_and_is_reaped_on_destroy ... ok
test result: ok. 2 passed; 0 failed; 12 filtered out; finished in 0.06s

$ cargo test -p slipfork-engine --test critical_engine -- --nocapture service
test service_logs_survive_idempotent_stop_and_freeze_seals_only_after_stop ... ok
test readiness_timeout_stops_the_supervisor_and_releases_the_service_id ... ok
test result: ok. 2 passed; 0 failed; 12 filtered out; finished in 20.48s

$ cargo test -p slipfork-engine --test critical_engine -- --nocapture dial
test in_netns_bridge_port_80_reaches_host_ingress_and_dials_back_into_the_world ... ok
test dial_world_tcp_reaches_in_world_listener_via_netns_sock_scm_rights ... ok
test two_worlds_isolate_the_same_port_and_dial_independently ... ok
test result: ok. 3 passed; 0 failed; 11 filtered out; finished in 0.75s

$ cargo test -p slipfork-engine --test critical_engine -- --nocapture same_port
test two_worlds_isolate_the_same_port_and_dial_independently ... ok
test r10_r11_reflink_netns_conformance_cow_isolation_uid_and_parallel_same_port ... ok
test result: ok. 2 passed; 0 failed; 12 filtered out; finished in 1.72s

$ cargo test -p slipfork-engine --test critical_engine -- --nocapture entry_command
test entry_command_enters_the_persistent_init_pid_with_native_stdio ... ok
test result: ok. 1 passed; 0 failed; 13 filtered out; finished in 0.05s

$ cargo test -p slipfork-engine --test critical_engine -- --nocapture in_netns_bridge_port_80
test in_netns_bridge_port_80_reaches_host_ingress_and_dials_back_into_the_world ... ok
test result: ok. 1 passed; 0 failed; 13 filtered out; finished in 0.69s


$ ! test -f crates/slipfork-engine/src/pty.rs && echo "NO pty.rs OK"
NO pty.rs OK
$ rg -q 'forbid\(unsafe_code\)' crates/slipfork-engine/src/bin/slipfork-world-init.rs && echo "WORLD-INIT forbid OK"
WORLD-INIT forbid OK
$ test "$(rg -l 'unsafe\s*\{' crates/slipfork-engine/src | wc -l)" -eq 1 && echo ONE unsafe module
ONE unsafe module: crates/slipfork-engine/src/linux_cow.rs

$ cargo clippy -p slipfork-engine --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.90s
$ cargo fmt --all -- --check      # exit 0
$ RUSTDOCFLAGS="-D warnings" cargo doc -p slipfork-engine --no-deps
 Documenting slipfork-engine v0.1.0
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.03s
```

`make check` and the harness gates (round 2, after the gate hardening):

```text
$ uv run --no-project tools/guard.py
guard: OK (271 file(s) checked)
$ uv run --no-project tools/plan.py validate
plan OK
$ uv run --no-project tools/critical_guard.py --base 1b4cf79 --node M2.7
critical-guard: 1b4cf79..working tree, 85 changed file(s)
  M  crates/slipfork-engine/tests/critical_engine.rs  [authorized by crates/slipfork-engine/**]
  A  crates/slipfork-vcs/tests/critical_vcs.rs  [added (always allowed)]
  A  crates/slipfork-ospp/tests/critical_ospp.rs  [added (always allowed)]
critical-guard: OK: authorized by M2.7
$ make check
… 24 × `test result: ok` (plan-check, guard, ruff, pytest, bun, cargo fmt, cargo clippy -D warnings,
  cargo test --workspace --all-targets, cargo test --workspace --doc, zensical --strict, docs honesty)
Build finished in 0.19s
check_docs_honesty: OK (8 published doc(s) match real CLI subcommands)
exit=0
```

(Round 1 of the hardening pass saw `make check` fail only in the then-uncommitted `M2.11`
`crates/slipfork-vcs/tests/support/mod.rs`; every `slipfork-engine` target was green throughout, and the
milestone gate re-ran clean once that concurrent edit landed.)

## Files changed (all inside `crates/slipfork-engine/`)

```
 M crates/slipfork-engine/src/lib.rs            LocalIngressBridge, WorldSpec.local_ingress, WorldEntryCommand,
                                                ServiceState/ServiceHandle, trait spawn_service/stop_service/
                                                service_logs/entry_command/dial_world_tcp, new EngineError variants
 M crates/slipfork-engine/src/reflink_netns.rs  persistent world boot/ready/teardown, nsenter exec, service
                                                supervision (+abort on failed readiness), netns.sock SCM_RIGHTS
                                                dial, freeze ordering, registry-entry removal on destroy
 M crates/slipfork-engine/src/linux_cow.rs      send_fd/recv_fd (single audited SCM_RIGHTS pair, extra-descriptor
                                                rejection), signal_process, fd-leak unit tests
 M crates/slipfork-engine/src/incus.rs          entry_command (incus exec) + typed Unsupported + record removal
 M crates/slipfork-engine/src/conformance.rs    shared contract: entry_command shape, typed dial/service_logs,
                                                destroyed world == WorldGone
 M crates/slipfork-engine/tests/critical_engine.rs  persistent_init/service/dial/same_port/entry_command/
                                                in_netns_bridge_port_80/readiness_timeout + fail-closed init helper
 A crates/slipfork-engine/src/bin/slipfork-world-init.rs  persistent PID 1 (forbid unsafe_code)
 A crates/slipfork-engine/src/service.rs        size-capped ServiceLogs (snapshot + follow + poison diagnostic)
 A crates/slipfork-engine/src/world_init.rs     shared 0600 JSON config + AF_UNIX path-limit validation
```

No `Cargo.toml` changes (no new dependency: `serde_json`, `serde`, `libc` were already declared). No edits
outside `crates/slipfork-engine/**`; `plan/` untouched except for `RESULT.md` and `FOLLOWUPS.md`.

## Follow-ups (other nodes)

1. **`M2.8`/`M2.9`:** `slipforkd` must bind the `0600` pathname `AF_UNIX` `<state_root>/http-ingress.sock` and
   serve the same HTTP/WS dispatch loop as its TCP listeners (`H-011` #4 Variant a) — `LocalIngressBridge` is
   configured but the host listener does not exist yet.
2. **`M2.16`:** pass `LocalIngressBridge { port: http_ingress_port, host_ingress_sock: state_dir.join(
   "http-ingress.sock") }` when minting a `Local` world, and keep `--state-dir` short enough for
   `<state_dir>/worlds/…/netns.sock` (≤ 107 bytes). Consider having `slipfork daemon unit` reject an overlong
   state dir up front, and inject the `slipfork-world-init` path explicitly with
   `with_world_init_program` if helpers are installed outside the daemon's own directory.
3. **`M2.21`:** replace `IncusEngine`'s typed `Unsupported` service/dial methods with the live implementation; the
   shared conformance suite already accepts either a working capability or that typed failure.
4. **`M2.16`/`M2.17`:** the CLI still constructs `ReflinkNetnsEngine::new(state_root, uid, gid)`; when the
   composition root moves the helper path into the wiring, switch it to `with_world_init_program` so the
   executable-location assumption lives only at the composition root.
5. **`docs/knowledge/LESSONS.md` (conductor):** `AF_UNIX` pathname sockets cap at 107 bytes, so a `<state_root>`
   with long project/slug components must be rejected before `clone_world`; also `SIGKILL` (not `SIGTERM`) is
   required to tear down a PID-namespace init, because a namespace init ignores fatal signals sent from within
   its own namespace.
6. **`freeze_world` semantics for `M3`:** a frozen world keeps its PID 1 alive (only the on-disk state is sealed),
   which is what `exec_in_world` after freeze relies on; if slot pre-warming wants a frozen world to release its
   namespace, that is a separate lifecycle decision.
7. **Remaining MINORs from the review gate** (deliberately not fixed in `M2.7`, tracked in
   `plan/nodes/M2/M2.7/FOLLOWUPS.md`): `nsenter` argv-construction duplication, `spawn_reader` /
   `spawn_log_reader` merge, `ARCHITECTURE.md` §3.3/§7 unsafe-table accuracy (`kill(2)` added, nonexistent
   `current_username` row removed), and the engine-doc restatement of the `H-011` kernel rule.
