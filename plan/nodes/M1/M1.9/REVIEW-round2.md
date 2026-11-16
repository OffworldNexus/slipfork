# M1.9 REVIEW — round 2 (post-remediation), Quad Review Gate

VERDICT: PASS
NODE: M1.9
BASE: `09e83153c072221c7828a77de54722c607e92535` (HEAD unchanged)
ROLE: reviewer (functional lens, fresh context)

## EVIDENCE — every `# Acceptance` command re-run by this reviewer

| # | Acceptance item | Result | Command / observed |
|---|---|---|---|
| 1 | 0 hits for `after_suite`/`SLIPFORK_ARTIFACTS_DIR` | PASS | `rg -n "after_suite\|SLIPFORK_ARTIFACTS_DIR" crates support ARCHITECTURE.md docs CHANGELOG.md` -> no output, `exit=1` |
| 2 | tooling tests | PASS | `uv run --no-project --with pytest pytest -q tools/tests` -> `6 passed in 0.06s`, exit 0 |
| 3 | `cargo test -p slipfork-dag` | PASS | `critical_dag` 8 passed, `critical_support` 4 passed, 0 failed, exit 0 |
| 4 | `cargo test -p slipfork -p slipfork-dag` | PASS | `critical_slug` 6 + `critical_dag` 8 + `critical_support` 4 passed, 0 failed, exit 0 |
| 5 | registry file exists + parses typed | PASS | `test -f support/integrations.toml` exit 0; typed parse proven by `embedded_registry_parses_with_unique_ids_and_ordered_tiers` |
| 6 | `make check` + critical guard | PASS | `make check` exit 0 (plan-check, guard, ruff, pytest, bun, cargo fmt/clippy/doc/test, `zensical build --strict` -> `No issues found`, `check_docs_honesty: OK (8 published doc(s))`); `critical_guard.py --base 09e8315 --node M1.9` -> `OK: authorized by M1.9` (only `critical_dag.rs`, `critical_slug.rs`, added `critical_support.rs`) |
| extra | `guard.py` / `plan.py validate` | PASS | `guard: OK (147 file(s) checked)`; `plan OK` |

Scope: every modified/untracked file is inside `NODE.md` `paths` (or `plan/**`). No unauthorized
critical-test edit. `git status`: ARCHITECTURE.md, CHANGELOG.md, `crates/slipfork-dag/{Cargo.toml,
src/lib.rs,tests/critical_dag.rs}`, `crates/slipfork/tests/critical_slug.rs`, ADR-0006,
`docs/reference/manifest.md`, plus untracked `src/support.rs`, `tests/critical_support.rs`,
`support/integrations.toml`.

## ROUND-1 BLOCKER: fixed

`support/integrations.toml:37` `signoff_commit = "d0b11e069ced52d0e03ebb3b251a1077d0c1bf39"`.
`git log --format=%H -- docs/decisions/ADR-0002-…md` returns exactly `d0b11e0` and `798b997`;
`git show d0b11e0 -- docs/decisions/ADR-0002-…md` flips `Status: Proposed (awaiting M1 human
validation: H-004)` -> `Accepted (H-004 tracked for M2 live host incusd mount)` — i.e. the commit
*is* the sign-off. `git cat-file -e d0b11e0` succeeds. `rationale` names the signed-off decision
("the M1.6 amendment that established the current ADR-0002 world-engine IoC, Incus, and reflink
decision"). No other entry claims `none`, so this is the only `signoff_commit`/`signoff_doc` pair.

## Round-1 MAJORs re-verified

- **(a) empty report set fails closed.** `RawTestSpec::into_test_spec` (`lib.rs:150-171`) returns
  `DagError::EmptyCoverageReport` for an explicit `coverage_report = []` under `coverage = true`,
  and `None` still defaults to `BTreeSet::from([Text])`. Re-ran end-to-end:
  `slipfork dag plan --file empty.toml` -> exit 1, `` `coverage_report` must list at least one
  report format when `coverage = true` ``; `default.toml` -> exit 0; `coverage_fail_under = 101`
  -> exit 1. Pinned by `typed_test_coverage_is_single_way_and_fail_closed` (both empty-list forms,
  plus `coverage_report = []` without `coverage = true` -> `CoverageOptionsRequireEnabled`).
- **(b) `per_world` never cached / never an origin-independent parent.** `lib.rs:363-370`
  short-circuits `spec.per_world || !cache.has_fixture(&digest)` -> `Rebuild`; `lib.rs:293-303`
  rejects `parent_spec.per_world && !spec.per_world` with `OriginIndependentChildOfPerWorld`.
  `per_world` is hashed into the fixture digest. Pinned by
  `per_world_fixtures_are_never_served_from_the_shared_digest_cache` (populated cache: per-world
  nodes `Rebuild`, shared sibling `Cached`, digest still world-independent) and
  `per_world_fixtures_are_typed_and_cannot_parent_origin_independent_nodes`.
- **(c) test names are true.** `none_tier_signoff_commit_is_format_checked_not_verified_against_git`
  asserts only lowercase 40-hex; `embedded_registry_recipe_pages_signoff_docs_and_critical_test_symbols_exist`
  no longer claims commit verification; `embedded_registry_parses_with_unique_ids_and_ordered_tiers`
  and `validate_rejects_overclaiming_entries_empty_pointers_and_duplicate_ids` match their bodies.
- **(d) published manifest docs.** `docs/reference/manifest.md` §2 now documents the shipped typed
  `[test]` table (`coverage`, `coverage_report` with `html|xml|json|lcov|text`, `coverage_fail_under`
  `0..=100`) and `[fixtures.<id>].per_world`, with honest scope (parse/validate/plan in `0.1.0`;
  coverage execution + per-world execution in M3) and no unimplemented CLI claim. The published
  example was extracted verbatim and run: `slipfork dag plan --file <example>` -> exit 0
  (`migrate` -> `seed_catalog` -> `seed_sites` waves). `CHANGELOG.md` 0.1.0 records both additions.

## BLOCKER

None.

## MAJOR (FOLLOWUPS — recorded, not commit-stopping per gate freeze)

1. `support/integrations.toml:29-37` — `incus-engine` still claims tier `none`, whose `ADR-0007` §1
   definition is "No meaningful test surface; nothing to integrate", while its own `rationale` says
   the adapter ships and passes the shared `WorldEngine` conformance suite. The rationale is now
   transparent (conservative under-claim, not an over-claim), but the tier label still contradicts
   the tier definition. This is the round-1 spec gap; it needs human direction (`direction-challenge`):
   either promote to `declared` with a real recipe page, or amend `ADR-0007` §1 tier semantics.
2. `ARCHITECTURE.md:13` and `ARCHITECTURE.md:26` — the composition-root paragraph and the mermaid
   edge state that `crates/slipfork/src/main.rs` wires `SupportRegistry::embedded()`, but `main.rs`
   imports only `{DagPlanner, FsDigestCache, FsMerkleHasher, SlipforkConfig}` and never references
   `SupportRegistry`. Fix: say "schema + embedded file in `0.1.0`; `main.rs` wiring and
   `slipfork support` in M2", and mark the `CliCtl --> SR` edge `M2` (or drop it).

## MINOR (FOLLOWUPS)

3. `crates/slipfork-dag/tests/critical_support.rs:104-145` — the path/symbol sweep iterates only
   `tier().required_evidence()`, so an *extra* optional pointer (e.g. a `deploy_ref` on a `declared`
   entry) would go unverified while `ARCHITECTURE.md:453` says "every `recipe_page`/`signoff_doc`/
   `deploy_ref`/… path exists". Either iterate all present `EvidenceField`s or qualify the sentence
   with "tier-required".
4. `ARCHITECTURE.md:386-409` — `IntegrationTier::Display` and `EvidenceField::Display` are
   implemented but not shown/documented in §3.6; `RegistryError::MissingEvidence` is only implied by
   "missing/empty". Add them to the code block/prose.
5. `docs/decisions/ADR-0003-…md:18` still labels `per_world = true` a "M2/M3 schema extension"
   although the schema ships in `0.1.0` (M1.9). Outside M1.9 `paths` -> needs its own node.
6. `docs/design/m1-core-and-poc.md:13` still says "M2 adds … `support/integrations.toml`" although
   the registry file ships now. Design doc, outside `paths` -> followup.
7. `crates/slipfork-dag/Cargo.toml:9` — `publish = false` is set only on `slipfork-dag` while every
   sibling crate stays publishable; there is no `cargo publish`/crates.io flow in the repo, so this
   is an inconsistent release posture rather than a functional defect. Drop it or apply it
   workspace-wide when the release story is decided.
8. `docs/reference/manifest.md:116` says per-world execution lands in "Milestone 3" while
   `ADR-0003` §1 says "M2/M3"; align the milestone label.

## Docs↔code agreement (both directions)

`ARCHITECTURE.md` §3.5/§3.6, `ADR-0006` §1/§2/§5, `ADR-0007` §1 and `docs/reference/manifest.md` §2
agree with the shipped code on every normative name and semantic: `CoverageReportFormat`
(`snake_case`, `Serialize+Deserialize`), `CoverageThreshold` (`u8`, `MAX = 100`, `percent`),
`CoverageConfig { reports, fail_under }`, `TestSpec { coverage }`, `FixtureSpec.per_world`
(default `false`), `DagError::{CoverageOptionsRequireEnabled, EmptyCoverageReport,
InvalidCoverageThreshold, OriginIndependentChildOfPerWorld}`, `IntegrationTier` ordering +
`required_evidence()` ladder, `IntegrationCategory`, `EvidenceField`, `Evidence` (private fields,
no `Deserialize`), `IntegrationEntry` accessors, `SupportRegistry::{embedded, parse_toml, entries}`,
`RegistryError::{Toml, EmptyRegistry, DuplicateId, MissingEvidence}`. Nothing implemented is
undocumented except the `Display` impls (MINOR 4); the only contradictions are the wiring claim
(MAJOR 2) and stale milestone labels (MINOR 5/6/8).

## Regression / honesty

- `critical_dag.rs` assertions grew 24 -> 44; the `r9_orthogonal_invalidation_…` body is byte-for-byte
  unchanged; `critical_slug.rs` changed only its `slipfork.toml` fixture (2 lines) and kept every
  assertion. `critical_support.rs` is a new, additive suite.
- No `#[ignore]`, `#[allow]`, `sleep()`, `unwrap()`, `todo!` in the touched source/tests.
- Adversarial cases assert exact typed variants; positive and fail-closed paths both covered.
- `deny_unknown_fields` retained on `FixtureSpec`, `ArtifactSpec`, `RawTestSpec`, `RawSlipforkConfig`,
  `RawRegistry`, `RawIntegrationEntry`, `RawEvidence`.
- No secret values in any touched `Debug`/`Display`; no ambient `env`/cwd I/O in `slipfork-dag`
  (`include_str!` + `env!("CARGO_MANIFEST_DIR")` only).

DOCS_CODE_AGREEMENT: ok on all normative fields; non-normative divergences at ARCHITECTURE.md:13/26
(MAJOR 2) and stale milestone labels ADR-0003:18 / docs/design/m1-core-and-poc.md:13 / manifest.md:116.
