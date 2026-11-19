# RESULT — M3.28.4 Engine facts: `world_processes`/`world_lineage`, per-world `BootLineage`, injected clock, one origin renderer

- **Status:** `done` (attempts 1–5; attempt 5 closed the last legality blocker: no second engine
  constructor)
- **Node:** `M3.28.4` (`release/0.1.0`, base `b49faef`)
- **Owner:** implementer

## What shipped

1. **One injected clock port** (`crates/slipfork-engine/src/clock.rs`): `MonotonicClock` with
   `now() -> Instant` (durations) **and** `now_wall() -> SystemTime` (labels), the single production
   adapter `SystemClock`, and the one duration conversion `elapsed_millis(started, finished)`.
   `compose_daemon` builds `Arc<dyn MonotonicClock>` once and injects it into the world engine, the
   rootfs builder, the OCI image store, the postmaster-readiness gate, and its own base-seed
   measurement. No `Instant` crosses a wire; no adapter keeps a second time source.
2. **One engine constructor** — `BtrfsEngine::with_driver_and_clock(.., driver, clock)`.
   `BtrfsEngine::new`/`BtrfsEngine::with_driver` are deleted, so a composition root that forgets the
   clock does not compile into a second time source. The one test call site that used `new` to
   assert `BtrfsRequired` (`critical_btrfs_live`) now passes the driver and clock explicitly.
3. **`WorldEngine::world_processes`** over the engine's own `ServiceRecord` map: service-id order,
   row kind from `ServiceSpec.kind` (a **caller** fact), state from the service's `AtomicU8`, `pid`
   from the in-world `ProcessIdentity`/supervised `Child` only while `Running`, `port` from the
   recorded `readiness_port` while running, `uptime_secs = now - started_at` from the injected
   clock. A stopped/exited service keeps its row with `pid`/`port`/`uptime_secs` = `None`; a
   poisoned supervision record fails closed with `EngineError::Execution`; destroyed/destroying/foreign
   handle → `EngineError::WorldGone`.
4. **`WorldEngine::world_lineage`** returns the `WorldLineage` recorded on **that world's** handle at
   `clone_world`: rootfs digest + cache/millis from the materialized `WorldRootfs`, one row per
   `OciRootfs`, and the seeder's `BootLineage`. Destroyed/foreign handle → `WorldGone`.
5. **One cache verdict** — `CacheOutcome { Hit, Miss }`, replacing the `LineageCache`/`ServiceCache`
   pair; `miss_offline` was deleted with **no production path** (an offline host fails closed rather
   than recording a third verdict). `WorldRootfs`/`OciRootfs` carry `cache` + `millis`; the builder
   and the Docker OCI store stamp them from the injected clock (`Miss` + measured millis, `Hit` +
   `None`).
6. **Honest, separately measured base/seed facts.** `ensure_base_snapshot` returns
   `(SnapshotId, BootLineage)` and measures the **restore** span and the **seal** span separately:
   only a real `Ok(())` seal reports `base.millis`/`seed.millis`/`rebuilt: Some(true)`;
   `SnapshotExists` (base already on disk) reports reuse facts (`base.millis: None`,
   `seed.rebuilt: Some(false)`, no durations); a phase that was not separately measured publishes
   `None`. Reused bases are a named `SealedBase { snapshot, lineage }`. A **pre-flight**
   `WorldEngine::snapshot_exists` skips the whole seeding pipeline on a restarted daemon instead of
   running a doomed `pg_restore`.
7. **`WorldLineage::unknown()` deleted** and every call site replaced: `daemon.rs` `list()` and
   `world_processes()` read the engine's lineage/rows directly, and the daemon-side row `kind` is
   gone (`LiveWorld.services` is `Vec<ServiceHandle>`).
8. **One origin renderer:** `WorldOrigin::local` documents/enforces `:80` → port-less,
   `1..=65535` → `:<port>`, port `0` → typed `WorldOrigin::OriginPortZero`, `snow.yml` domain route
   → `OriginNotDefaultIngress`. No module hand-renders a `.localhost` origin.

## `# Acceptance` (real output, attempt 5)

```console
$ cargo test -p slipfork-engine --lib
test result: ok. 66 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 9.75s

$ cargo test -p slipfork-engine --test critical_engine -- --test-threads=1
test result: ok. 15 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 75.72s

$ cargo test -p slipfork-world --test critical_world
test result: ok. 16 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

$ ! grep -rn 'WorldLineage::unknown' crates/ --include=*.rs && echo no-placeholder-lineage
no-placeholder-lineage

$ ! grep -rnE 'format!\("http://\{[a-z_]+(host|route)' crates/ --include=*.rs | grep -v '^crates/slipfork-world/' && echo one-origin-renderer
one-origin-renderer

$ cargo clippy --workspace --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.48s

$ cargo fmt --all -- --check && echo fmt-ok
fmt-ok
```

Supporting runs this attempt: `cargo test -p slipfork-engine --test critical_btrfs_live --
--test-threads=1` (migrated constructor) → 1 passed; `cargo test -p slipfork-edge --test
critical_edge` → 37 passed; `cargo test -p slipfork --lib` → 35 passed;
`! grep -rn 'BtrfsEngine::new\b' crates/ ARCHITECTURE.md docs/` → `one-constructor`.

```console
$ uv run --no-project tools/critical_guard.py --base b49faef --node M3.28.4
critical-guard: b49faef..working tree, 37 changed file(s)
  M  ... 7 protected suites  [authorized by <paths>]
critical-guard: OK: authorized by M3.28.4

$ uv run --no-project tools/guard.py
guard: OK (552 file(s) checked)
$ uv run --no-project tools/plan.py validate
plan OK
$ git diff --shortstat
 33 files changed, 1212 insertions(+), 286 deletions(-)
```

(`git diff --shortstat` covers tracked files; `crates/slipfork-engine/src/clock.rs` and this
`RESULT.md` are new untracked files. The earlier `make check` run reached `ts-test` and stopped on
this host's missing `bun`; the Rust half — `make rs-fmt rs-lint docs-check` and `make rs-test` — is
green.)

## Quad-round fixes applied across attempts

- **Fabricated durations (blocker):** a cache hit can no longer claim "rebuilt 2.1s" — only a real
  seal reports durations/`rebuilt: true`, and seal and restore are distinct measurements.
- **Second paths removed:** `BtrfsEngine::new`, `with_driver`, `DockerOciImageStore::with_clock`,
  the `SystemClock` default in `OciImageStore::new`, and `PostgresReady`'s `Fn() -> Instant` clock.
- **One helper / one enum / one kind:** shared `elapsed_millis`; single `CacheOutcome`;
  `ServiceSpec.kind` is set by every spawn caller (`Component` / `BackingService`) instead of being
  derived from the spawn mechanism.
- **Dead surface:** `loopback_http_url` and its `critical_world` case deleted; `BtrfsEngine::new`
  deleted and every doc that prescribed it (`ARCHITECTURE.md`, `ADR-0010`, `ADR-0002`,
  `docs/design/world-base-image.md`) rewritten to name `with_driver_and_clock`; the collapsed enum
  is documented in `ARCHITECTURE.md` §3.3/§3.4 and `docs/design/dx-0.1.0-ux.md` §8.2, with
  `miss_offline` recorded as deleted (never promised back).
- **Coverage:** `assert_world_engine_conformance` now proves the two mandatory accessors: a world
  with no spawned service reports an **empty** row set (no fabricated rows), any non-running row
  carries no live fact, `Hit ⇒ millis: None` for the rootfs and every service image, the lineage
  digest is the materialized Merkle key, and a destroyed/foreign handle yields `WorldGone` for both.

## Protected tests (extended, never weakened)

- Engine unit tests: `world_processes_keeps_stopped_rows_and_stamps_uptime_from_the_injected_clock`,
  `world_lineage_records_each_world_own_boot_facts` (fake clock, per-world isolation, `WorldGone`).
- `critical_world`: added `world_origin_local_never_renders_port_zero`; the port-less/`:65535`/
  named-domain cases are unchanged.
- `critical_engine`/`critical_btrfs_live`/`critical_edge`: constructor and new-field migrations plus
  the refined conformance matrix; `critical_guard` reports every protected edit authorized.

## Notes for downstream nodes

- `M3.28.17` (`restart`/`up`) consumes `WorldProcessReport`/`WorldLineage` unchanged.
- A resource spawned as `WorldProcess` reports `BackingService` whenever its caller says so:
  `ServiceSpec.kind` is authoritative.
- No global state, no `unsafe`, no new dependency, no `#[allow]`, no compatibility shim.
