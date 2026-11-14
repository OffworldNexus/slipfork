# Milestone 1 Technical Design: Core IoC Architecture & Real CoW PoC

- **Milestone:** `M1` (`0.1.0` — Global Target Design & Initial Measured PoC)
- **Status:** Drafted for Human Validation (`plan/HUMAN.md` `H-001`..`H-004`)
- **ADRs:** `ADR-0001`, `ADR-0002`, `ADR-0003`, `ADR-0004`

---

## 1. Composition Root & DI Tree

All crates obey strict constructor/parameter Dependency Injection with zero mutable globals and zero direct `std::env` / `std::process` calls outside adapters:

| Port (Trait) | Crate | Implementations in M1 | Shared Conformance Suite |
|---|---|---|---|
| `SecretProvider` | `slipfork-snow` | `MemorySecretProvider`, `FileVaultProvider` (`0600` atomic write), `LayeredSecretProvider`, `OsppBundleSnapshot` | `conformance::run_secret_provider_suite` |
| `EntropySource` | `slipfork-snow` | `OsCsprngEntropy`, `DeterministicEntropy` (tests) | Unit + critical tests |
| `InputHasher` | `slipfork-dag` | `FsMerkleHasher` (SHA-256 over sorted relative paths + contents), `MapHasher` (tests) | `critical_dag.rs` |
| `WorldEngine` | `slipfork-engine` | `ReflinkNetnsEngine` (Linux `ioctl(FICLONE)` + unprivileged `CLONE_NEWUSER\|NEWNET\|NEWNS\|NEWPID`), `IncusEngine` (Unix-socket HTTP `/1.0` client + protocol builder) | `conformance::run_world_engine_contract` + `critical_engine.rs` |
| `SlipforkClient` | `packages/opencode-slipfork` | `CliSlipforkClient`, `FakeSlipforkClient` (tests) | Plugin unit tests |

---

## 2. Numbered Testable Requirements (`R1..R14`)

- **`R1` (`modes` vs `envs` mutual exclusion):** `SnowFile::parse_yaml` rejects any `snow.yml` declaring both `envs` and `modes` with `SnowError::EnvsAndModesMutuallyExclusive`.
- **`R2` (`modes.all` + `modes.<mode>` merge):** Resolving mode `M` merges `modes.all.envs[g]` under `modes[M].envs[g]` for each group `g`, then merges component `envs: [g1, g2]` left-to-right.
- **`R3` (`*` forbidden in `modes`):** Using `"*"` inside `modes` returns `SnowError::AsteriskModeForbidden` naming `all`.
- **`R4` (`??` read-only fallback):** `${{ vars::KEY ?? "fallback" }}` returns `"fallback"` when absent without calling `ensure_secret`.
- **`R5` (`!!` write-once secret generation):** `${{ secrets::KEY !! random_string(50) }}` generates once via `EntropySource`, persists via `SecretProvider::ensure_secret`, and returns the stored value identically on subsequent resolves.
- **`R6` (`!!` namespace restriction):** Using `!!` on any namespace other than `secrets` fails with `SnowError::EnsureOnlyAllowedOnSecrets`.
- **`R7` (Exhaustive missing-value refusal):** Missing keys without `??` or `!!` are collected across all requested env groups and returned together in `SnowError::MissingValues` — never failing one variable at a time.
- **`R8` (`SecretValue` redaction):** Resolved values contaminated by `secrets::*` wrap in `SecretValue`, whose `Debug` and `Display` print `"[REDACTED]"` unless `.expose_secret()` is called explicitly at the adapter boundary.
- **`R9` (Orthogonal DAG invalidation):** Mutating an `[artifacts.<id>]` input file changes that artifact's digest while leaving all `[fixtures.<id>]` digests bit-for-bit identical; mutating a parent fixture invalidates child fixtures while leaving sibling branches and artifacts untouched.
- **`R10` (Real `FICLONE` CoW cloning):** `ReflinkNetnsEngine::clone_world` clones directories using Linux `ioctl(FICLONE)` (`0x40049409`) for regular files and fails closed (`EngineError::ReflinkUnsupported`) if the backing filesystem rejects `FICLONE` (e.g., `/tmp` `tmpfs`) — never silently degrading to slow byte-copying without CoW!
- **`R11` (Unprivileged isolated `netns` + `$UID` preservation):** `ReflinkNetnsEngine::exec_in_world` enters a fresh `CLONE_NEWUSER | CLONE_NEWNET | CLONE_NEWNS | CLONE_NEWPID` namespace with identity UID/GID map (`"$UID $UID 1"`), brings `lo` up (`127.0.0.1`), preserves `geteuid() == host_uid` on created files, and allows two concurrent worlds to bind `127.0.0.1:5432` and `127.0.0.1:8000` simultaneously with zero port conflict.
- **`R12` (`IncusEngine` REST contract):** `IncusEngine` builds exact Incus `/1.0/instances` CoW copy payloads (`source.type = "copy"`, `instance_only = true`, `devices.workspace` with `type = "disk"`, `shift = "true"`) over Unix domain sockets and fails closed (`EngineError::DaemonUnavailable`) when the socket is absent.
- **`R13` (OpenCode V2 implicit routing):** `@slipfork/opencode` registers `worktree.transform`, `shell.hook("create.before")`, and `session.hook("context")` without replacing the `shell` tool.
- **`R14` (Real M1 PoC Benchmark):** `poc/run_benchmark.py` measures real PostgreSQL 18.7 + XFS `FICLONE` + isolated `netns` + warm Python Zygote + headless Chromium Playwright across sequential/truncate baseline vs Slipfork Cold DAG vs Slipfork Warm DAG vs worktree fork latency.
