# M1.9 RESULT — typed `[test]` coverage, `per_world` fixtures, support registry

STATUS: done (attempt 2 — Quad Review Gate remediation)

Base: `09e83153c072221c7828a77de54722c607e92535` (HEAD unchanged).

## Attempt-1 work retained

Typed `TestSpec`/`CoverageConfig`/`CoverageThreshold`, `FixtureSpec.per_world`, zero
`after_suite`/`SLIPFORK_ARTIFACTS_DIR` residue, `SupportRegistry` owned by `slipfork-dag`, and the
`critical_dag.rs` / `critical_slug.rs` / `critical_support.rs` protected tests all remain; no
assertion was weakened.

## Remediation per review finding

1. **BLOCKER — false `signoff_commit` (`support/integrations.toml`).**
   `git log -1 --format=%H -- docs/decisions/ADR-0002-world-engine-ioc-incus-and-reflink.md`
   returns `d0b11e069ced52d0e03ebb3b251a1077d0c1bf39` (the M1.6 amendment; `798b997` added it).
   `incus-engine.signoff_commit` is now `d0b11e0…`, and `rationale` names the decision that commit
   signs off (ADR-0002 world-engine IoC / Incus / reflink). Re-audited every other pointer:
   `recipe_page` paths (`docs/getting-started.md`, `docs/snow-modes.md`, `docs/testing-dag.md`) and
   all `critical_test` symbols resolve; `incus-engine` is the only `none` entry, so it is the only
   `signoff_commit`/`signoff_doc` pair.
2. **MAJOR — empty report set fails closed.** New typed
   `DagError::EmptyCoverageReport`; `RawTestSpec::into_test_spec` rejects an explicit
   `coverage_report = []` while `coverage = true` with an omitted `coverage_report` still defaults
   to `{text}`. Named in `ADR-0006` §5 and `ARCHITECTURE.md` §3.5; pinned by
   `typed_test_coverage_is_single_way_and_fail_closed` (empty list with/without `fail_under`, plus
   `coverage_report = []` without `coverage = true` → `CoverageOptionsRequireEnabled`).
3. **MAJOR — `per_world` fixtures never reused across worlds.**
   `DagPlanner::plan` now reports `CacheAction::Rebuild` for every `per_world = true` node and never
   calls `DigestCache::has_fixture` for it (short-circuit), so the world-independent digest cannot
   resolve to another world's baked-in URLs. `DigestCache`'s port shape is unchanged. New critical
   test `per_world_fixtures_are_never_served_from_the_shared_digest_cache` proves per-world nodes
   stay `Rebuild` while an origin-independent sibling is `Cached` from the same populated cache;
   the existing test still proves an origin-independent child of a per-world parent fails with
   `OriginIndependentChildOfPerWorld`. Documented in `ARCHITECTURE.md` §3.5 and `ADR-0006` §2.
4. **MAJOR — test names/claims are true.** `critical_support.rs` is split into
   `embedded_registry_parses_with_unique_ids_and_ordered_tiers`,
   `embedded_registry_recipe_pages_signoff_docs_and_critical_test_symbols_exist`, and
   `none_tier_signoff_commit_is_format_checked_not_verified_against_git` (40-hex shape only).
   `ARCHITECTURE.md` §3.6 now states exactly what `0.1.0` verifies (paths/symbols, shape-only
   commit) versus what M2 adds (`git` commit existence/attestation, CI job keys, CLI, ceiling).
5. **MAJOR — published manifest docs.** `docs/reference/manifest.md` §2 now documents the shipped
   typed `[test]` table (`coverage`, `coverage_report`, `coverage_fail_under` validated `0..=100`)
   and `[fixtures.<id>].per_world`, with honest scope (parse/validate/plan in `0.1.0`; coverage and
   per-world execution in M3; snapshots shareable unless `per_world`). `CHANGELOG.md` `0.1.0` gains
   the typed `[test]`/`per_world` line and the support-registry line.
6. **Cheap — `parse_toml` is the sole validating path.** `Deserialize` lives only on private
   `RawRegistry`/`RawIntegrationEntry`/`RawEvidence`; public `Evidence`/`IntegrationEntry` have
   private fields + accessors and no `Deserialize`. `validate()` is private and fails closed on an
   empty registry (`RegistryError::EmptyRegistry`, tested for `""` and `integration = []`).
7. **Cheap — one tier→evidence ladder.** `EvidenceField` enum +
   `IntegrationTier::required_evidence()` are used by validation and the critical test; the two
   parallel string tables are gone (`MissingEvidence.field` is now `EvidenceField`).
8. **Cheap — cruft removed.** Dropped unused `Serialize` from `SlipforkConfig`/`TestSpec`/
   `CoverageConfig`/`CoverageThreshold`/`ArtifactSpec`/`FixtureSpec`/registry claim types (kept on
   `CoverageReportFormat`, which M3's `CoverageSummary` serializes, and on the plan output types);
   removed redundant `#[serde(default)]` on `Option` fields; removed unused `Ord` from
   `CoverageThreshold` and documented `CoverageThreshold::MAX` in `ARCHITECTURE.md` §3.5; made
   `support` a private module with crate-root re-exports; added `publish = false` to
   `crates/slipfork-dag/Cargo.toml`.
9. **Cheap — accuracy fixes.** `ARCHITECTURE.md` diagram label now says the registry is embedded in
   `0.1.0` with the CLI in M2; `dag-config-cli.critical_test` cites
   `slipfork_dag::critical_dag::r9_orthogonal_invalidation_front_edit_never_invalidates_db_snapshots_and_vice_versa`;
   `incus-engine.rationale` truthfully says it is conservatively reported at the lowest tier
   (adapter ships and passes the shared `WorldEngine` conformance suite; `declared` additionally
   needs a published recipe page that `0.1.0` does not have) without inventing a tier.

## Files

- `crates/slipfork-dag/src/lib.rs` (modified)
- `crates/slipfork-dag/src/support.rs` (modified)
- `crates/slipfork-dag/Cargo.toml` (modified: `publish = false`)
- `crates/slipfork-dag/tests/critical_dag.rs` (modified, authorized)
- `crates/slipfork-dag/tests/critical_support.rs` (modified, authorized addition)
- `crates/slipfork/tests/critical_slug.rs` (attempt-1 change, unchanged)
- `support/integrations.toml` (modified)
- `ARCHITECTURE.md` (modified)
- `docs/decisions/ADR-0006-polyglot-runner-protocol-and-world-owned-stack.md` (modified)
- `docs/reference/manifest.md` (modified)
- `CHANGELOG.md` (modified)

## Acceptance evidence (exact commands / results)

1. `rg -n "after_suite|SLIPFORK_ARTIFACTS_DIR" crates support ARCHITECTURE.md docs CHANGELOG.md`
   → no output, `exit=1` (0 hits).
2. `uv run --no-project --with pytest pytest -q tools/tests`
   → `6 passed in 0.05s`, exit 0.
3. `cargo test -p slipfork-dag`
   → `critical_dag`: 8 passed; `critical_support`: 4 passed; 0 failed; exit 0.
4. `cargo test -p slipfork -p slipfork-dag`
   → `critical_slug`: 6 passed; `critical_dag`: 8 passed; `critical_support`: 4 passed; 0 failed;
   exit 0.
5. `test -f support/integrations.toml` → exit 0; parsing through the typed schema is proven by
   `embedded_registry_parses_with_unique_ids_and_ordered_tiers` and
   `embedded_registry_recipe_pages_signoff_docs_and_critical_test_symbols_exist`.
6. `make check` → exit 0 (plan-check, guard, ruff, pytest, bun, cargo fmt/clippy/doc/test,
   `zensical build --strict` → `No issues found`,
   `check_docs_honesty: OK (8 published doc(s) match real CLI subcommands)`).
7. `uv run --no-project tools/critical_guard.py --base 09e83153c072221c7828a77de54722c607e92535 --node M1.9`
   → `critical-guard: OK: authorized by M1.9` (17 changed files; only the authorized
   `critical_dag.rs`, `critical_slug.rs`, and the added `critical_support.rs`).
8. `cargo fmt --all -- --check` → exit 0.
   `cargo clippy --workspace --all-targets -- -D warnings` → `Finished`, no warnings.
9. `uv run --no-project tools/guard.py` → `guard: OK (147 file(s) checked)`.
   `uv run --no-project tools/plan.py validate` → `plan OK`.

## Deferred / observed (not fixed — outside this node's instructions)

- `Evidence`/`IntegrationEntry` `Display` duplication and the repeated synthetic `none` TOML record
  in `critical_support.rs` (reviewer-deferred followups).
- `ADR-0003:18` "M2/M3 schema extension" wording and `ADR-0007` §status milestone ambiguity
  (outside `paths`).
- M2 compliance tool: `git` commit existence/attestation and `.github/workflows` job-key
  verification (the `ci_job` branch is dormant until an `exercised` entry ships).
- `reflink-netns-world-engine.recipe_page` points at the CLI flow in `docs/getting-started.md`
  while `critical_test` cites the library conformance test; the link is loose but both pointers
  exist (legality REVIEW item 7).
