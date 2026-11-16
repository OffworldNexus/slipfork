# ADR-0002: `WorldEngine` IoC Contract — `BtrfsEngine` Constructor DI, Engine-Owned `ReadinessProbe`, and Shared `$UID:$GID` Mounts

- **Status:** Accepted — rewritten in place for M3 (`0.3.0`) `BtrfsEngine` target state (`ADR-0010`; `H-004` closed; `ADR-0009` `Dockerfile` `L0` rootfs + daemonless `crun` OCI backing services; implemented across `M3.20`, `M3.12`–`M3.15`, and `M3.21`)
- **Date:** 2026-11-16

## Context

Slipfork needs sub-50 ms copy-on-write snapshotting and cloning of full application worlds (`L0` rootfs + PostgreSQL, Redis, and S3 state) with:
1. Complete network, process, mount, and SysV IPC isolation across parallel worlds (`ADR-0010` §6) so `:5432`, `:6379`, `:9000`, `:8000`, `:3000`, and SysV IPC keys never collide.
2. Direct sharing of the host VCS worktree under the developer's exact `$UID:$GID` (never leaving `root:root` files on bind mounts).
3. Genuine protocol-level service readiness on `spawn_service` — a `postgres:18` container binds `:5432` before `initdb`/recovery finishes and SQL `SELECT 1` succeeds, so a TCP-accept-only contract forces callers (`slipforkd`, tests, and M3 Phase 2's `STP v1` test orchestrator / warm slots) to invent divergent polling loops.
4. Clean Inversion of Control inside `slipforkd` (`H-010`) so domain orchestration (`slipfork-edge` and `crates/slipfork-test` (`M3.2` target state)) speaks only to `trait WorldEngine` while unit and daemon-wiring suites run deterministically on non-Btrfs developer machines via constructor DI (`ARCHITECTURE.md` §1).

## Decision

1. **One `WorldEngine` Port, Pure Constructor DI, and Distinct RO/Writable Signatures (`M3.20`, `M3.12`–`M3.15`, `M3.21`):**
   - All world orchestration depends exclusively on `trait WorldEngine` (`crates/slipfork-engine`), passing opaque in-process `WorldHandle`s inside `slipforkd` (`H-010` #1) and explicit `WorldSpec.control_dir` (`ADR-0010` §2), verified against the shared `WorldEngine` conformance suite.
   - Read-only snapshot creation (`create_snapshot`, `freeze_world`) and writable world cloning (`clone_world`) are distinct methods by signature — zero snapshot-mode flags and zero capability boolean bags.
   - `BtrfsEngine` is the **sole** production `WorldEngine` (`ADR-0010` §1). `BtrfsEngine::new(...)` delegates to the explicit DI constructor `BtrfsEngine::with_driver(..., Arc<dyn SubvolumeDriver>) -> Result<Self, EngineError>`, calling `SubvolumeDriver::ensure_supported(&state_dir)` (`KernelBtrfsDriver` in production -> `EngineError::BtrfsRequired`, `ADR-0010` §4–§5). Test-only `SubvolumeDriver` fakes follow the Cargo Feature & Test-Scaffolding Rule in `ARCHITECTURE.md` §1.
2. **Engine-Owned Service Readiness (`ServiceSpec.readiness: ReadinessProbe`, normative, target state `M3.13`–`M3.15` & `M3.2`):**
   - **The engine owns readiness end-to-end:** `ServiceSpec` carries a single typed `readiness: ReadinessProbe` field (`ReadinessProbe::None`, `ReadinessProbe::TcpPort { port, timeout }`, or `ReadinessProbe::Exec { port: Option<u16>, argv, timeout }` — zero separate bare port field).
   - **Single probe supplier (zero `match BackingServiceKey` ladder in `slipforkd`):** Each backing-service `OciRootfs` record (`WorldRootfs::service_images`, `ADR-0009` §2) carries its typed `readiness: ReadinessProbe` from `BaseImageBuilder`, and `slipforkd`'s service-spec builder passes `oci_rootfs.readiness` through to `ServiceSpec.readiness` unchanged; for `ServiceRuntime::WorldProcess` app components, `slipforkd` maps declared `snow.yml` `components[].internal_ports[0]` to `ReadinessProbe::TcpPort` and portless worker components to `ReadinessProbe::None`.
   - **Fail-closed guarantee:** `BtrfsEngine::spawn_service` waits for any declared `port` TCP accept in the world `netns` and then executes `ReadinessProbe::Exec` inside the service's own runtime (`OciContainer` via `crun exec` or `WorldProcess`) until exit `0`; if the process exits early or `timeout` elapses, `spawn_service` stops/reaps the service and fails closed with `EngineError::ServiceReadinessFailed { world, id, last_exit_code, stderr }` carrying captured probe `stderr` and retained service logs. Once `spawn_service` returns `Ok(ServiceHandle)`, the service is usable immediately — no caller hand-rolls `psql` or `redis-cli` polling.
3. **Unified Per-World Subvolume, Namespace Isolation & `$UID:$GID` Ownership:**
   - Unified per-world Btrfs subvolume layout and the `BTRFS_IOC_*` snapshot/clone/destroy sequence follow `ADR-0010` §§2–3 normatively.
   - Persistent `slipfork-world-init` PID 1 (`H-011`) runs under `ADR-0010` §6's unprivileged `unshare` namespace set with identity `$UID:$GID` mapping, bringing up isolated `127.0.0.1` (including `:80` `$BASE_URL` bridges to `<state_dir>/http-ingress.sock`), `pivot_root`ing into the read-only `L0` root, and bind-mounting host worktrees read-write as `$UID:$GID` with zero root-owned file pollution.

## Consequences

- One honest `WorldEngine` trait (this ADR) and one Btrfs production engine (`ADR-0010`) deliver sub-25 ms atomic world snapshots/clones, protocol-verified service readiness on return from `spawn_service`, zero `$UID` drift, and pure constructor DI (`ARCHITECTURE.md` §1).
