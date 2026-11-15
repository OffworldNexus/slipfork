# Milestone 1 Technical Design: Core IoC Architecture & Real CoW PoC

- **Milestone:** `M1` (`0.1.0` — Global Target Design & Initial Measured PoC)
- **Status:** Accepted — amended for M2 (`H-001`..`H-003`, `H-005`, `H-008`, `H-009`, `H-010` validated, `H-006` closed by deferral; `ADR-0001`..`ADR-0008` locked)
- **ADRs:** `ADR-0001`, `ADR-0002`, `ADR-0003`, `ADR-0004`, `ADR-0005`, `ADR-0006`, `ADR-0007`, `ADR-0008`

---

## 1. Composition Root & DI Tree

All crates obey strict constructor/parameter Dependency Injection with zero mutable globals and zero direct `std::env` / `std::process` calls outside adapters wired at the Composition Root (`crates/slipfork/src/main.rs` and `packages/opencode-slipfork/src/index.ts`).

**M1 (`0.1.0`) Shipped Wiring:** `crates/slipfork/src/main.rs` wires `FileVaultProvider`, `OsCsprngEntropy`, `ResourceCoordinates::local_for_world`, `FsMerkleHasher`, `FsDigestCache`, and `ReflinkNetnsEngine`, while `crates/slipfork-dag` embeds `support/integrations.toml` (`SupportRegistry::embedded()`). `crates/slipfork-world` is the pure identity layer (`WorldTarget`, `LocalHostRoute`, `TunnelRegistrar`/`TunnelLease`, plus M2 `WorldOrigin` & `<= 31`-byte `WeaverServiceName`) shared by `slipfork-snow` and `slipfork-dag` in `0.1.0` (`M2.2` adds the `slipfork-engine -> slipfork-world` edge plus `slipfork-edge`/`slipfork-ospp`/`slipfork-vcs`, and M3 adds `slipfork-test`). M2 adds `crates/slipfork-edge` (unprivileged `LISTEN_FDS` `slipforkd` with single-origin `308`/`421` enforcement, `WorldRegistry`, `UnavailableTunnelRegistrar` until `WeavedRegistrar` lands in `M2.22`), `crates/slipfork-vcs` (`RepoRootResolver`, `GitWorktreeAdapter`, `JjWorkspaceAdapter`, `git-remote-slipfork`), `crates/slipfork-ospp` (`OSPP v1` WebSocket + `tpmd` P-256 `DeviceKeySigner` + streaming `zstd` seed dumps, `ADR-0008`), and `slipfork support [--json]` (`ADR-0007`); M3 adds `crates/slipfork-test` (`SocketRunnerAdapter`, `CommandTestAdapter`, typed `CoverageDriver`).

| Port (Trait) | Crate | Implementations | Shared Conformance / Critical Suite |
|---|---|---|---|
| `SecretProvider` & `EntropySource` | `slipfork-snow` (+ `slipfork-ospp` M2) | `MemorySecretProvider`, `FileVaultProvider` (`0600` — M2 default, zero OSPP prereq), `LayeredSecretProvider`, `OsppSecretProvider` (`OSPP v1` WebSocket, M2 `ADR-0008`); `OsCsprngEntropy` | `conformance::assert_secret_provider_conformance` + `critical_snow.rs` |
| `InputHasher`, `DigestCache` & `ArtifactStore` | `slipfork-dag` | `FsMerkleHasher`, `FsDigestCache`, `CowArtifactStore<C: VolumeCloner>` (M3) | `critical_dag.rs` |
| `WorldEngine` & `VolumeCloner` | `slipfork-engine` | `ReflinkNetnsEngine` (`linux_cow.rs` `ioctl(FICLONE)` + unprivileged `CLONE_NEWUSER\|NEWNET\|NEWNS\|NEWPID`), `IncusEngine` (Unix-socket HTTP `/1.0`) | `conformance::assert_world_engine_conformance` + `critical_engine.rs` |
| `WorldTarget`, `LocalHostRoute` & `WorldOrigin` | `slipfork-world` (M1 + M2 `WorldOrigin`) | `WorldTarget::{Canonical, UserFork, TestSlot}`, `LocalHostRoute`, M2 `WorldOrigin::{Local, Shared}` | `critical_world.rs` (`.localhost` namespace) |
| `WorldRegistry` & `TunnelRegistrar` | `slipfork-edge` (M2) / `slipfork-world` trait | `WorldRegistry` (`WorldOrigin` + `dial_world_tcp` + `308`/`421` shared-origin edge rules), `UnavailableTunnelRegistrar` (M2; deleted when `WeavedRegistrar` lands in `M2.22`) | `InMemoryTunnelRegistrar` conformance + M2 IPC conformance |
| `VcsWorkspaceFactory` & `VcsWorkspace` | `slipfork-vcs` (M2) | `GitWorktreeAdapter` (`git worktree` + scratch-index `refs/slipfork/<fork>/wip`), `JjWorkspaceAdapter` (`jj workspace` + `@` commit) | M2 `VcsWorkspace` conformance |
| `TestAdapter` & `CoverageDriver` | `slipfork-test` (M3) | `SocketRunnerAdapter` (`RunnerProtocol v1`: `pytest-slipfork` #1, Playwright, PHPUnit, nextest), `CommandTestAdapter`; per-language `CoverageDriver` + surviving `SLIPFORK_OUTPUT_DIR` | M3 `TestAdapter` conformance |
| `SlipforkGateway` | `packages/opencode-slipfork` | `CliSlipforkGateway`, `FakeSlipfork` (tests) | `tests/coordinator.critical.test.ts` |

---

## 2. Numbered Testable Requirements (`R1..R16`)

- **`R1` (Top-level `envs` & `project` hard rejection):** `SnowFile::parse_yaml` rejects top-level `envs:` and `project:` (consolidated into `SnowError::ForbiddenTopLevelKey { key }` in `M2.4`; identity lives in `slipfork.toml`).
- **`R2` (`modes.all` + `modes.<mode>` merge & 2-level `components.<id>.run_command` override):** Resolving mode `M` merges `modes.all.envs[g]` under `modes[M].envs[g]` and resolves `effective_run_command` via `modes[M].components[id]` -> `components[id]`, rejecting `modes.all.components` and unknown component IDs.
- **`R3` (`*` forbidden in `modes`):** Using `"*"` inside `modes` returns `SnowError::AsteriskModeForbidden` naming `all`.
- **`R4` (`??` read-only fallback):** `${{ vars::KEY ?? "fallback" }}` returns `"fallback"` when absent without calling `ensure_secret`.
- **`R5` (`!!` write-once secret generation):** `${{ secrets::KEY !! random_string(50) }}` generates once via `EntropySource`, persists via `SecretProvider::ensure_secret`, and returns the stored value identically on subsequent resolves.
- **`R6` (`!!` namespace restriction):** Using `!!` on any namespace other than `secrets` fails with `SnowError::EnsureOnlyAllowedOnSecrets`.
- **`R7` (Exhaustive missing-value refusal):** Missing keys without `??` or `!!` are collected across all requested env groups and returned together in `SnowError::MissingValues` — never failing one variable at a time.
- **`R8` (`SecretValue` redaction):** Resolved values contaminated by `secrets::*` wrap in `SecretValue`, whose `Debug` and `Display` print `"[REDACTED]"` unless `.expose()` is called explicitly at the adapter boundary.
- **`R9` (Orthogonal DAG invalidation):** Mutating an `[artifacts.<id>]` input file changes that artifact's digest while leaving all `[fixtures.<id>]` digests bit-for-bit identical; mutating a parent fixture invalidates child fixtures while leaving sibling branches and artifacts untouched.
- **`R10` (Real `FICLONE` CoW cloning):** `ReflinkNetnsEngine::clone_world` clones directories using Linux `ioctl(FICLONE)` (`0x40049409`) for regular files and fails closed (`EngineError::ReflinkUnsupported`) if the backing filesystem rejects `FICLONE` (e.g., `/tmp` `tmpfs`).
- **`R11` (Unprivileged isolated `netns` + `$UID` preservation):** `ReflinkNetnsEngine::exec_in_world` enters a fresh `CLONE_NEWUSER | CLONE_NEWNET | CLONE_NEWNS | CLONE_NEWPID` namespace with identity UID/GID map (`"$UID $UID 1"`), brings `lo` up (`127.0.0.1`), preserves `geteuid() == host_uid` on created files, and allows concurrent worlds to bind `127.0.0.1:5432` and `127.0.0.1:8000` simultaneously.
- **`R12` (`IncusEngine` REST contract):** `IncusEngine` builds exact Incus `/1.0/instances` CoW copy payloads (`source.type = "copy"`, `instance_only = true`, `devices.workspace` with `type = "disk"`, `shift = "true"`) over Unix domain sockets and fails closed (`EngineError::DaemonUnavailable`) when the socket is absent.
- **`R13` (OpenCode V2 implicit routing):** `@slipfork/opencode` registers `worktree.transform`, `shell.hook("create.before")`, and `session.hook("context")` without replacing the `shell` tool.
- **`R14` (Real M1 PoC Benchmark):** `poc/run_benchmark.py` measures real PostgreSQL 18.7 + XFS `FICLONE` + isolated `netns` + warm Python Zygote + headless Chromium Playwright across sequential/truncate baseline vs Slipfork Cold DAG vs Slipfork Pre-Warmed Pool (`3.12x` speedup, `123 ms`/test).
- **`R15` (`.localhost` 1/2/3-label routing & fork disambiguation):** `ResourceCoordinates::local_for_world` emits `http://(<fork>|test-<slot>.)?<project>.localhost`; `slipfork_world::WorldTarget::validate` rejects dots, reserved `test-*` prefixes, and `domains[].id` collisions so `LocalHostRoute::parse` disambiguates `<project>.localhost`, `<domain>.<project>.localhost`, `<fork>.<project>.localhost`, and `<domain>.<fork>.<project>.localhost` unambiguously.
- **`R16` (Polyglot `RunnerProtocol v1`, built-in `CoverageDriver` & `SLIPFORK_OUTPUT_DIR` orthogonality):** Slipfork contains zero Blastit- or `pytest`-hardwired core traits and zero user-authored shell pipelines (`ADR-0006` §1); `RunnerProtocol v1` coordinates any language over `AF_UNIX` NDJSON, `TestAdapter::coverage` (`CoverageDriver`) owns multi-world coverage paths and combination, and surviving `SLIPFORK_OUTPUT_DIR` serves Blastit, Allure, and Playwright traces uniformly.
