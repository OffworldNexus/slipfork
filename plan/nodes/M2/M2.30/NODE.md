---
id: M2.30
title: "slipforkd completion: OSPP v1 session/secrets/dump restore, freeze_world base snapshot, Jj workspace routing & optional slipfork.toml"
kind: task
status: done
owner: implementer
priority: 1
depends_on: [M2.20]
paths: ["crates/slipfork/**", "crates/slipfork-ospp/**", Cargo.lock]
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-16T03:04"
updated: "2026-11-16T04:36"
---

# Goal

Complete `slipforkd` composition-root wiring in `crates/slipfork` (`M2` milestone gate blocker remediation):
1. Wire paired `OSPP v1` persistent `OsppClient`, `LayeredSecretProvider<FileVaultProvider, OsppSecretProvider>`, `OsppClient::resource_offer` -> `ResourcePlan::resolve`, `DumpStreamer` + `DiskSeedDumpStore`, and container-scoped `pg_restore` + `CHECKPOINT;` via `WorldEngine::exec_in_service` (`ADR-0008` §1–§3, `ADR-0009` §2–§3, `H-010` #1).
2. Wire `WorldEngine::freeze_world` to seal a real initialized/restored `<project>-base` snapshot so `slipfork up` and `slipfork fork` CoW-clone warm `PGDATA`/state via `FICLONE` instead of an empty directory.
3. Wire `JjWorkspaceAdapter` for `.jj`-bearing repositories (failing closed on missing `jj`, zero fallback to Git) and restore `slipfork.toml` optionality on `up`/`fork`.

# Context

- `crates/slipfork-ospp`, `crates/slipfork-vcs`, `crates/slipfork-dag`, `crates/slipfork-snow`, and `crates/slipfork-engine` all ship and pass their crate-level critical suites, **but `crates/slipfork` (`slipforkd`) left four `ROADMAP.md` M2 / `H-010` #1 seams unwired:**
  - `crates/slipfork/src/daemon.rs:2019` hardcodes `ResourcePlan::resolve(&spec, ResourceMode::from(target), dump, None)` (`offer = None`), `daemon.rs:2082` sets `readonly_mounts: Vec::new()`, `vaults.rs` opens only `FileVaultProvider`, and `OsppClient` / `OsppSecretProvider` / `LayeredSecretProvider` / `DumpStreamer` / `DiskSeedDumpStore` / `WorldEngine::exec_in_service` / `WorldEngine::freeze_world` have **zero callers** in `crates/slipfork`.
  - `crates/slipfork/src/daemon.rs:541-556` (`ensure_base_snapshot`) snapshots an **empty** directory (`base_seed_dir`) and never calls `freeze_world`, so every `slipfork fork` clones 0 bytes and runs a fresh cold `initdb`.
  - `crates/slipfork/src/bin/slipforkd.rs:192` hardcodes `GitWorktreeAdapter::new()` (`JjWorkspaceAdapter` has zero callers in `crates/slipfork`).
  - `crates/slipfork/src/daemon.rs:422-429` (`load_config`) rejects `NotFound` when `<vcs_root>/slipfork.toml` is absent, breaking `docs/getting-started.md` and `VISION.md` ("plus optional `slipfork.toml`").

# Design / Invariants

1. **Optional `slipfork.toml` (`daemon.rs::load_config`):** When `<vcs_root>/slipfork.toml` is absent (`io::ErrorKind::NotFound`), return `Ok(SlipforkConfig::default())`; read/permission/TOML parse errors still fail closed (`ConfigProjectOverride` already follows this rule).
2. **Paired `OSPP v1` Session & `LayeredSecretProvider` (`ADR-0008` §1–§3, `H-003`, `H-010` #1):**
   - Introduce an injected `OsppSessionStore: Send + Sync` port in `crates/slipfork` (production adapter persists `{ host, port, tls, account }` — zero secrets — atomically `0600` at `<data_home>/ospp-session.json` when `DaemonService::auth_login` redeems a `PairingUrl`).
   - When a paired session coordinate exists, `DaemonService` lazily connects/reuses `Arc<OsppClient>` via `OsppEndpoint::connect(&host, port, tls)`:
     - `vault` for `snow_resolve` and world boot wraps `LayeredSecretProvider::new(FileVaultProvider, OsppSecretProvider::new(Arc::clone(&client), project, mode)?)` (`FileVaultProvider` stays sole `!!` write sink in `local` mode). When unpaired, or when `OsppClient` is unreachable under offline `prefer_remote`, `FileVaultProvider` serves local secrets while `ResourceStrategy::Remote` and `DumpPolicy::RefreshDump` fail closed.
     - `resolve_plan` passes `OsppClient::resource_offer(&project, WORLD_MODE)` into `ResourcePlan::resolve`.
3. **Decompressed Seed Dump + Container `pg_restore`/`CHECKPOINT;` + `freeze_world` Base Snapshot (`ADR-0008` §2, `ADR-0009` §2–§3):**
   - `DiskSeedDumpStore` writes `zstd`-compressed `<sha256>.pgdump`; add a bounded-window (`MAX_DUMP_WINDOW_LOG`) materializer on `SeedDumpStore` / `DiskSeedDumpStore` (`crates/slipfork-ospp`) producing the verified uncompressed `pg_dump -Fc` file (`<sha256>.pgcustom`, `0600` atomic rename) so the `postgres:<v>` container never needs a `zstd` binary.
   - `ensure_base_snapshot` (keyed by `(project, base_digest)` over backing-service versions + resolved dump digests) boots a short-lived seed world on an empty root snapshot, starts backing-service `OciContainer`s (with `readonly_mounts` exposing `/dumps/<sha256>.pgcustom` when `ResourcePlan` selects a dump; fetching via `DumpStreamer::new(&client)` on `SeedDumpStore::lookup` miss or `--refresh-dump`), runs `pg_restore --no-owner --no-privileges --clean --if-exists -U postgres -h 127.0.0.1 -d <db> /dumps/<sha256>.pgcustom` + `psql -U postgres -h 127.0.0.1 -d <db> -v ON_ERROR_STOP=1 -c "CHECKPOINT;"` via **`WorldEngine::exec_in_service`** inside the `pg_clusters.<cluster>` container (`ServiceNotRunning` / non-zero exit fails closed, never retried in the app rootfs), seals the seed world via **`WorldEngine::freeze_world`**, and destroys the seed world **before** `up`/`fork` clones `<project>-base-<digest>` and runs `pre-deploy` hooks.
4. **Jujutsu Workspace Factory Routing (`ADR-0005` §3, `ARCHITECTURE.md` §3.5):**
   - Replace bare `GitWorktreeAdapter::new()` in `slipforkd` with a composition-root `VcsWorkspaceFactory` that checks `request.repo.vcs_root.join(".jj").is_dir()`: when true, delegate strictly to `JjWorkspaceAdapter` (which fails closed with `VcsError::ToolMissing` if `jj` is absent — **never** falling back to Git); otherwise delegate to `GitWorktreeAdapter`.
   - Fix the stale top-of-file doc comment in `crates/slipfork/src/main.rs:9-11` (`snow resolve` runs through `slipforkd`, not CLI-local).
5. **Shared `ReferenceOsppServer` Test Support (Zero Copy-Paste):** Expose `crates/slipfork-ospp/tests/support/reference_server.rs` behind an opt-in `test-server` feature on `slipfork-ospp` (enabled only in `[dev-dependencies]` of `crates/slipfork`) rather than duplicating WebSocket server code.

# Acceptance

```bash
cargo test -p slipfork-ospp --all-targets
cargo test -p slipfork --test critical_ospp_daemon -- --nocapture
cargo test -p slipfork --test critical_cli -- --nocapture
cargo test -p slipfork --test critical_l0_wiring -- --nocapture
cargo clippy --workspace --all-targets -- -D warnings
make check
```

All commands exit `0`. Required positive + adversarial assertions in `crates/slipfork/tests/critical_ospp_daemon.rs` / `critical_cli.rs`:
- `slipfork up` succeeds in a repository with `snow.yml` + `Dockerfile` and **no** `slipfork.toml` (while malformed `slipfork.toml` still fails closed).
- End-to-end against `ReferenceOsppServer`: `slipfork auth login -` -> `slipfork snow resolve` resolves remote `vars::`/`secrets::` while persisting `!!` only to local `0600` vault -> `slipfork up` resolves remote S3 + streams a real `zstd`-compressed `pg_dump -Fc` archive restored via `exec_in_service` inside `postgres:18` + `freeze_world` -> `slipfork fork feat-seed` CoW-clones the restored table rows without re-running `pg_restore`.
- `--refresh-dump` and `local = "remote"` fail closed when unpaired or when the OSPP server is stopped; `--no-dump` skips the dump and still seals a clean `initdb` base snapshot via `freeze_world`.
- A `.jj`-marked colocated repository routes `fork` to `JjWorkspaceAdapter` (succeeding when `jj` is on `PATH` and failing closed with `ToolMissing` when `PATH` omits `jj`, never silently creating a Git worktree).
