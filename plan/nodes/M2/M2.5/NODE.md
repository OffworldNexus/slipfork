---
id: M2.5
title: "slipfork-dag: [resources] ResourceStrategy, fs feature gate, single evidence ladder, H-009 cleanup"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M2.3]
paths: ["crates/slipfork-dag/**", support/integrations.toml]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-15T16:40"
updated: "2026-11-15T18:38"
---

# Goal

Land `slipfork-dag`'s M2 slice: the typed `[resources]` `ResourceStrategy` schema with mode-aware defaults and fail-closed validation, the `fs` feature gate that keeps concrete filesystem adapters out of the domain core (M1.6 #11), the single tier-to-evidence ladder exposed for `slipfork support --json`, and the M1.9 test/dedup cleanups.

# Context

- `SlipforkConfig::parse_toml` is the sole parser of `slipfork.toml` (including `[artifacts]`, `[fixtures]`, `[test]`), and `SupportRegistry`/`IntegrationTier`/`Evidence` live here (`support/integrations.toml` is embedded via `include_str!`).
- `ADR-0008` (locked by `M2.1`) is normative for `[resources]`; `ADR-0007` requires the `--json` view to emit `required_evidence` so the `make check` ceiling consumes the one Rust ladder instead of re-deriving it in Python.
- M1.6 #11: `FsMerkleHasher`/`FsDigestCache` (`std::fs` adapters) live inside the domain crate and should be gated as concrete adapters.
- M1.9 minors in scope: `critical_dag.rs` duplicated per-world TOML block, `critical_support.rs` repeated `none` record plus a third copy of the presence predicate, `support.rs` restating the nine evidence fields four times and a `Display` that duplicates serde kebab-case strings, and `CoverageReportFormat`'s unused `Serialize` derive.

# Design / Invariants

1. **`[resources]` schema & single `ResourcePlan::resolve` owner (`ADR-0008` §3):** `SlipforkConfig.resources: ResourcesSpec` parsed from a `#[serde(deny_unknown_fields)]` wire table (`[resources.s3_buckets.<id>]` and `[resources.pg_databases.<id>]` with optional `local` and `test` keys mapped to `ResourceStrategy { Local, PreferRemote, Remote }`; zero boolean mode flags; unknown keys, unknown modes, or empty override tables fail closed, plus `ResourcesSpec::validate_declared_ids(snow_s3_ids, snow_pg_ids)`):
   - `ResourceMode { Local, Test }` derives `Serialize` only (**never** `Deserialize`) and is computed strictly from `WorldTarget` (`TestSlot(_) => Test`, `Canonical | UserFork(_) => Local`).
   - `DumpPolicy { Default, NoDump, RefreshDump }` models mutually exclusive `--no-dump | --refresh-dump` (zero precedence ladder; under `Default`, effective `ResourceStrategy` decides, and `RefreshDump` with `Local` or offline OSPP fails closed).
   - `ResourcePlan::resolve(spec: &ResourcesSpec, mode: ResourceMode, dump_policy: DumpPolicy, offer: Option<&RemoteResourceOffer>) -> Result<ResourcePlan, DagError>` is the **single** resolution owner consumed by `slipforkd`, `OsppClient`, and `TestCoordinator` (`Local` defaults to `PreferRemote`; `Test` defaults hermetically to `Local`).
2. **`H-009` Option B cleanup (`support/integrations.toml`):** Remove the `0.1.0` `incus-engine` (`tier = "none"`) placeholder from `support/integrations.toml` so the 5-tier scale stays strictly scoped to user-reachable integrations (internal library adapters without user-reachable CLI + recipe stay omitted until `declared`, `ADR-0007` §1 & `H-009`).
3. **`fs` feature (`M1.6` #11):** `FsMerkleHasher`/`FsDigestCache` move behind `#[cfg(feature = "fs")]` in a dedicated adapter module (`default = []` in `crates/slipfork-dag/Cargo.toml`); `cargo check -p slipfork-dag --no-default-features` must succeed (proving the domain core is filesystem-free), while `crates/slipfork` enables `features = ["fs"]`.
4. **One ladder:** `IntegrationTier::required_evidence() -> &'static [EvidenceField]` stays the only tier-to-evidence table; `EvidenceField` gains `as_str()` used by `Display`, serde, and the `support --json` view (single source of strings). `Evidence` keeps private fields with `field()` only, and `critical_support.rs` checks **all present** `EvidenceField` file/symbol pointers (not only tier-required ones, closing `M1.9` minor).
5. **Cleanups:** shared per-world TOML fixture and `none`-record builders in the two critical suites; delete `CoverageReportFormat`'s unused `Serialize`; no `publish = false` edits here (owned by `M2.2`).
6. Zero `slipfork-engine` dependency, zero `std::env`, zero clock reads.

# Acceptance

```bash
# 1. Full crate suites.
cargo test -p slipfork-dag --all-targets
cargo test -p slipfork-dag --test critical_dag
cargo test -p slipfork-dag --test critical_support

# 2. Domain core is filesystem-free without the adapter feature.
cargo check -p slipfork-dag --no-default-features
cargo check -p slipfork-dag --features fs

# 3. [resources] mode-aware defaults (hermetic test default + explicit override) & H-009 Option B.
cargo test -p slipfork-dag --test critical_dag resources
! rg -q 'incus-engine' support/integrations.toml
! rg -q 'std::fs' crates/slipfork-dag/src/lib.rs
test "$(rg -c 'fn required_evidence' crates/slipfork-dag/src/support.rs)" = "1"
rg -q 'pub struct ResourcePlan' crates/slipfork-dag/src

# 4. Gates.
cargo clippy -p slipfork-dag --all-targets -- -D warnings
make check
```

All commands exit `0`. Adversarial assertions required: `test`-mode defaults are strictly hermetic (ephemeral local S3 + clean `initdb`, zero shared S3 or dev dump unless explicitly configured), unknown `[resources]` keys/modes fail closed, `incus-engine` is absent from `support/integrations.toml`, and a `per_world = true` digest is still never reported as `CacheAction::Cached`.
