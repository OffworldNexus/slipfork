# M2.5 RESULT — `slipfork-dag`: `[resources]`, `fs` gate, one evidence ladder, H-009 cleanup

STATUS: done (attempt 1; batched-gate legality fix folded)

Base: `6e6a276` (`release/0.2.0`); working tree left uncommitted for `lead`.

## Summary

1. **`[resources]` typed schema + single `ResourcePlan::resolve` owner (`ADR-0008` §3).**
   `SlipforkConfig.resources: ResourcesSpec` parses `[resources.s3_buckets.<id>]` /
   `[resources.pg_databases.<id>]` with optional `local` / `test` keys mapped to
   `ResourceStrategy { Local, PreferRemote, Remote }`. `deny_unknown_fields` plus
   `DagError::EmptyResourceOverride` reject unknown keys, unknown modes, booleans, and empty
   override tables; `ResourcesSpec::validate_declared_ids(snow_s3_ids, snow_pg_ids)` fails closed
   on ids absent from `snow.yml`. `ResourceMode { Local, Test }` derives `Serialize` only and is
   computed exclusively through `From<&WorldTarget>` (`TestSlot(_) => Test`, otherwise `Local`).
   `ResourcesSpec::defaults_for(mode)` is the single default (`Local => PreferRemote`,
   `Test => Local`).
2. **One dump policy, zero precedence ladder.** `DumpPolicy { Default, NoDump, RefreshDump }`
   models the mutually exclusive flags directly: `NoDump` always yields a clean `initdb`,
   `RefreshDump` requires live remote dump metadata (`RemoteDump { refresh: true }`) and fails
   closed with a local strategy, and `Default` delegates to the effective `ResourceStrategy`.
   `Remote` strategies fail closed via `DagError::RemoteResourceUnavailable` when the OSPP offer
   is absent; `PreferRemote` falls back to local with `ResourceReason::NotOffered`.
   `ResourcePlan::diagnostics()` emits the typed `{kind, id, strategy, resolved, reason}` rows for
   `--json`; `RemoteS3Bucket` redacts both credentials in `Debug`. `PlannedPgDatabase.strategy`
   is the **effective** strategy: `NoDump` forces it to `Local` (with `reason: NoDump` and a
   `CleanInitdb` seed) even when the configured request was `remote`, so `strategy` and `target`
   can never contradict each other.
3. **`fs` feature (`M1.6` #11).** `FsMerkleHasher` / `FsDigestCache` moved to
   `crates/slipfork-dag/src/fs.rs` behind `#[cfg(feature = "fs")]` (default stays `[]`); the
   domain root no longer imports filesystem/path modules, and `crates/slipfork` already enables
   `features = ["fs"]`.
4. **One evidence ladder (`ADR-0007`, `H-009` Option B).** `IntegrationTier::required_evidence()`
   remains the only tier table; `EvidenceField` gained a public `as_str()`, `ALL`, and a
   `Serialize` impl so `Display`, serde, and `support --json` share one string source.
   `Evidence::is_present()` is the single presence predicate. `critical_support.rs` now verifies
   **every present** `EvidenceField` pointer (commits are 40-hex shape only; git/CI remain the M2
   compliance tool's job). Removed the `0.1.0` `incus-engine` `tier = "none"` record from
   `support/integrations.toml`.
5. **M1.9 cleanups.** Shared `PER_WORLD_TOML` fixture in `critical_dag.rs`, shared `none_record`
   builder in `critical_support.rs`, deleted the unused `Serialize` derive on
   `CoverageReportFormat`.

## Acceptance (`# Acceptance` block, all exit 0)

| # | Command | Result |
|---|---|---|
| 1 | `cargo test -p slipfork-dag --all-targets` | PASS — 11 `critical_dag` + 4 `critical_support`, 0 failed |
| 1 | `cargo test -p slipfork-dag --test critical_dag` | PASS — 11 passed |
| 1 | `cargo test -p slipfork-dag --test critical_support` | PASS — 4 passed |
| 2 | `cargo check -p slipfork-dag --no-default-features` | PASS — `Finished dev profile` |
| 2 | `cargo check -p slipfork-dag --features fs` | PASS — `Finished dev profile` |
| 3 | `cargo test -p slipfork-dag --test critical_dag resources` | PASS — 3 resources tests |
| 3 | `! rg -q 'incus-engine' support/integrations.toml` | PASS — exit 0 |
| 3 | `! rg -q 'std::fs' crates/slipfork-dag/src/lib.rs` | PASS — exit 0 |
| 3 | `test "$(rg -c 'fn required_evidence' ...)" = "1"` | PASS — `1` |
| 3 | `rg -q 'pub struct ResourcePlan' crates/slipfork-dag/src` | PASS — exit 0 |
| 4 | `cargo clippy -p slipfork-dag --all-targets -- -D warnings` | PASS — clean |
| 4 | `make check` | PASS — exit 0 (plan, guard, ruff, pytest, bun, fmt, workspace clippy, workspace tests/docs, zensical strict, docs honesty) |

Additional verification:

- `cargo test -p slipfork-dag --all-targets --no-default-features` — 15 passed, 0 failed.
- `cargo test -p slipfork-dag --all-targets --features fs` — 17 passed (2 fs adapter unit tests +
  15 critical), 0 failed.
- `cargo clippy -p slipfork-dag --all-targets --features fs -- -D warnings` — clean.
- `uv run --no-project tools/guard.py` — `guard: OK (241 file(s) checked)`.
- `cargo fmt -p slipfork-dag -- --check` — clean.
- `uv run --no-project tools/critical_guard.py --base 6e6a276 --node M2.5` — our two modified
  critical suites are `[authorized by crates/slipfork-dag/**]`; the command exits `1` only because
  the shared working tree also contains concurrent M2.4/M2.6 edits to
  `critical_engine.rs`/`critical_snow.rs`/`critical_world.rs`, which are outside M2.5's paths.

Adversarial coverage added: Test-mode defaults stay hermetically local (`CleanInitdb` + local S3)
even when OSPP offers every bucket/dump; explicit `test = "remote"` overrides only that resource;
unknown `[resources]` keys/modes/booleans/empty overrides fail closed; unknown ids fail
`validate_declared_ids`; explicit `Remote` and `RefreshDump` fail closed without an offer;
`RefreshDump` + local strategy fails closed; `NoDump` + `remote` reports the effective `Local`
strategy/target with `reason: NoDump` while seeding from `CleanInitdb`; `per_world = true`
fixtures are still never `CacheAction::Cached`.

## Files

- `crates/slipfork-dag/src/resources.rs` (new: `[resources]` types, offer types, single
  `ResourcePlan::resolve`, diagnostics)
- `crates/slipfork-dag/src/fs.rs` (new: `FsMerkleHasher` / `FsDigestCache` behind `fs`)
- `crates/slipfork-dag/src/lib.rs` (modified: `SlipforkConfig.resources`, typed errors, module
  split, `CoverageReportFormat` cleanup)
- `crates/slipfork-dag/src/support.rs` (modified: `EvidenceField::as_str`/`ALL`/`Serialize`,
  `Evidence::is_present`)
- `crates/slipfork-dag/tests/critical_dag.rs` (modified: shared per-world fixture + `[resources]`
  critical tests)
- `crates/slipfork-dag/tests/critical_support.rs` (modified: all-present pointer verification,
  `none_record` builder, single presence predicate)
- `support/integrations.toml` (modified: removed `incus-engine` `none` placeholder)

## Followups

- `M2.14` consumes the exported offer/plan types (`RemoteResourceOffer::new`,
  `with_s3_bucket`, `with_pg_dump`, `ResourcePlan::resolve`); no further API changes expected from
  M2.5. Concurrent M2.4/M2.6 edits to `slipfork-snow`/`slipfork-world` briefly broke the
  workspace build during this attempt; the final `make check` above is the reconciled tree.
