---
id: M2.7
title: "slipfork-engine: spawn/stop_service with logs, streaming exec, dial_world_tcp"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M2.6]
paths: ["crates/slipfork-engine/**"]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-15T16:40"
updated: "2026-11-15T20:53"
---

<!-- splice: replace everything after the frontmatter of plan/nodes/M2/M2.7/NODE.md with this file. Frontmatter, `# Acceptance`, and the recorded RESULT stay untouched (`M2.7` is `done`); `# Acceptance` is byte-identical to today's so the recorded evidence remains valid. Source of truth: ADR-0009 §2–§3 + ARCHITECTURE.md §3.3 (`M2.25`). -->

# Goal

Complete the `WorldEngine` persistent per-world PID 1 (`crates/slipfork-engine/src/bin/slipfork-world-init.rs`), service supervision, `<world_dir>/netns.sock` `SCM_RIGHTS` TCP dialer, `<state_root>/http-ingress.sock` in-netns `$BASE_URL` bridge, and direct CLI `entry_command` on `ReflinkNetnsEngine` (`H-010`, `H-011`).

# Context

- `M2.6` landed multi-mount `WorldSpec`, redacted `ExecEnv`, opaque `WorldHandle`, and `freeze_world`; this node lands the persistent world-init process, service supervision with retained logs, `dial_world_tcp`, `entry_command`, and the in-netns `$BASE_URL` bridge.
- **Kernel facts (`H-011`, `docs/knowledge/LESSONS.md`):** `setns(CLONE_NEWNET)` requires `CAP_SYS_ADMIN` in both the caller's and target's userns (`EPERM` from `slipforkd`), and `setns(CLONE_NEWUSER)` returns `EINVAL` in multithreaded processes. Therefore `ReflinkNetnsEngine::clone_world` spawns a persistent per-world PID 1 (`crates/slipfork-engine/src/bin/slipfork-world-init.rs`) inside `unshare --map-user=$UID --map-group=$GID --keep-caps --mount --net --pid --fork --mount-proc` (`H-011` #2).
- `H-010`'s "zero `SCM_RIGHTS`" rule applies strictly to the CLI <-> `slipforkd` interactive-TTY boundary, never to `slipfork-engine` internals (`H-011` #1). `unsafe` across the entire workspace stays confined to **one** module (`crates/slipfork-engine/src/linux_cow.rs`), which owns `ioctl(FICLONE)`, `current_uid_gid`, `current_username` (`getpwuid_r(geteuid())`), and the **single** audited `sendmsg`/`recvmsg` `SCM_RIGHTS` FD-transfer helper (`H-011` #3; zero `pty.rs` module).
- Log capture powers `slipfork logs [--world <fork>] [<target>]` and failure diagnostics, and must survive `stop_service` so failed worlds stay inspectable.
- **Contract hand-off (`ADR-0009` §2–§3):** this node lands the **only** supervision surface (`spawn_service` / `stop_service` / `service_logs`) with its payload carrying `argv`/`cwd` directly. `M2.28` reshapes that payload to `ServiceSpec { id, runtime: ServiceRuntime, env, readiness_port }` and adds `ServiceRuntime::OciContainer`, `exec_in_service`, and `service_entry_command`; it must **not** add a second supervisor, a `ContainerRuntime` trait, or an argv-shaped container path.

# Design / Invariants

1. **Persistent per-world PID 1 (`src/bin/slipfork-world-init.rs`) & `freeze_world` ordering (`H-011` #2):** `#![forbid(unsafe_code)]` `slipfork-world-init` is spawned by `clone_world` inside `unshare --map-user=$UID --map-group=$GID --keep-caps --mount --net --pid --fork --mount-proc`, bringing `lo` up, writing `0` to `/proc/sys/net/ipv4/ip_unprivileged_port_start`, binding `WorldSpec.mounts`, and recording a persistent `init_pid` that survives across calls until `destroy_world` signals/reaps the PID namespace cleanly. `freeze_world` stops any running services in `world` cleanly before reflinking `state_dir` so frozen snapshots are crash-consistent; because this node supervises exactly one runtime (`WorldProcess`), the stop-`WorldProcess`-then-`OciContainer` ordering that `ADR-0009` §3 requires is added by `M2.28` at the same call site — never by a parallel path.
2. **`spawn_service` / `stop_service` & retained `ServiceLogs` — one surface for every service kind:** Starts world-owned processes inside `init_pid`'s namespaces (`nsenter --target <init_pid>` or via `slipfork-world-init`) with clean `ExecEnv`, capturing stdout/stderr into a world-scoped size-capped log (`ServiceLogs::snapshot()` + `follow(&mut dyn FnMut(StreamChunk))`); `stop_service` signals, waits with a bounded grace period, is idempotent on repeat calls, and **preserves captured logs**. The payload is `ServiceSpec` and the engine owns the **single** dispatch over service kinds: `M2.28` replaces the current `argv`/`cwd` fields with `runtime: ServiceRuntime { WorldProcess | OciContainer }` behind these same three methods (`ARCHITECTURE.md` §3.3), so `crun` backing services inherit log retention, readiness, and idempotent stop with zero new plumbing.
3. **Streaming `exec_in_world` & Direct `entry_command` (`H-010` #1):** `exec_in_world(&self, world, argv, cwd, overlay_env, on_output)` enters `init_pid`'s namespaces and streams interleaved `StreamChunk`s while returning untruncated `ExecOutput`. `entry_command(&self, world: &WorldHandle, cwd: &Path) -> Result<WorldEntryCommand, EngineError>` returns `{ program, args_prefix, cwd }` (`nsenter --target <init_pid> --user --preserve-credentials --net --mount --pid --wd=<cwd> --` on `ReflinkNetnsEngine`; `incus exec` on `IncusEngine`) for direct CLI `Stdio::inherit()` entry. Service-scoped entry (`crun exec` for `slipfork db` -> `psql`) is a separate `service_entry_command(&ServiceHandle)` added in `M2.28` — `entry_command` never guesses a container.
4. **`dial_world_tcp(port)` over `<world_dir>/netns.sock` (`H-011` #3):** `slipfork-world-init` binds a `0600` pathname `AF_UNIX` socket `<world_dir>/netns.sock` (never an `@` abstract socket). Callers connect, send `port: u16`, and `slipfork-world-init` connects `127.0.0.1:<port>` inside the world netns and hands the connected `TcpStream` back via `linux_cow.rs`'s single audited `sendmsg`/`recvmsg` `SCM_RIGHTS` helper (with per-call `// SAFETY:` proofs). Dialing a destroyed world returns `EngineError::WorldGone`. Backing-service containers share the same `netns` (`ADR-0009` §2), so `:5432`, `:6379`, and `:9000` are reachable through this one dialer.
5. **In-netns `$BASE_URL` `:80` bridge via `<state_root>/http-ingress.sock` (`H-011` #4 Variant a):** When `WorldSpec.local_ingress` (`Option<LocalIngressBridge { port, host_ingress_sock }>`) is `Some`, `slipfork-world-init` binds `127.0.0.1:<port>` inside the world netns (including `:80`, enabled by in-userns `CAP_NET_BIND_SERVICE` + `ip_unprivileged_port_start=0`) and splices each accepted TCP stream bidirectionally to pathname `AF_UNIX` `host_ingress_sock` (`<state_root>/http-ingress.sock`).
6. No engine-kind ladders and no service-kind ladders outside `crates/slipfork-engine`; `unsafe` strictly confined to `crates/slipfork-engine/src/linux_cow.rs` alone.

# Acceptance

```bash
# 1. Persistent init_pid survives across calls, services stop cleanly before freeze_world, and logs survive stop_service.
cargo build -p slipfork-engine --bins
cargo test -p slipfork-engine --all-targets
cargo test -p slipfork-engine --test critical_engine -- --nocapture persistent_init
cargo test -p slipfork-engine --test critical_engine -- --nocapture service

# 2. dial_world_tcp reaches an in-world listener via netns.sock SCM_RIGHTS, and two worlds isolate the same port.
cargo test -p slipfork-engine --test critical_engine -- --nocapture dial
cargo test -p slipfork-engine --test critical_engine -- --nocapture same_port

# 3. Direct entry_command and :80 bridge reachable both from host and in-netns via http-ingress.sock.
cargo test -p slipfork-engine --test critical_engine -- --nocapture entry_command
cargo test -p slipfork-engine --test critical_engine -- --nocapture in_netns_bridge_port_80

# 4. Unsafe stays confined to linux_cow.rs alone (zero pty.rs, forbid(unsafe_code) on slipfork-world-init).
! test -f crates/slipfork-engine/src/pty.rs
rg -q 'forbid\(unsafe_code\)' crates/slipfork-engine/src/bin/slipfork-world-init.rs
test "$(rg -l 'unsafe\s*\{' crates/slipfork-engine/src | wc -l)" -eq 1
cargo clippy -p slipfork-engine --all-targets -- -D warnings
make check
```

All commands exit `0`. Adversarial assertions required: `init_pid` stays identical across consecutive `exec_in_world`/`entry_command` calls and is reaped on `destroy_world`, `dial_world_tcp` after `destroy_world` fails with `WorldGone`, `stop_service` twice is idempotent while its logs remain readable, `freeze_world` stops live services before reflinking `state_dir`, and `:80` is reachable both from host (`http-ingress.sock` -> `dial_world_tcp`) and from inside the world netns (`127.0.0.1:80` -> `slipfork-world-init` -> `http-ingress.sock` -> `dial_world_tcp`).
