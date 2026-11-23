# M1.9 REVIEW — `review-legality` (Quad Review Gate, Law 1)

VERDICT: FAIL
MISSING_INPUT: none

## BLOCKER (stops the commit)

1. `support/integrations.toml:37` — **illegal evidence / false sign-off pointer.**
   `signoff_commit = "09e83153c072221c7828a77de54722c607e92535"` is this node's *base* commit
   (`[M1.8] Fold owner review-gate corrections…`). It does not record the sign-off of
   `signoff_doc = "docs/decisions/ADR-0002-…"`: `git log -- docs/decisions/ADR-0002-…` shows
   `798b9972076c1d9024c2b41db3daef97522d8f65` (added) and `d0b11e069ced52d0e03ebb3b251a1077d0c1bf39`
   (amended); `09e8315` touches neither. The `none` tier exists to be *non-self-attesting*, and the
   only check that runs (`critical_support.rs:167-173`) is 40-hex format, so this false record would
   survive M2's existence check forever.
   **Minimal fix:** point `signoff_commit` at the commit that actually attests ADR-0002
   (`798b9972076c1d9024c2b41db3daef97522d8f65`), and say in `rationale` which decision that commit
   signs off. Do not substitute "the commit this entry lands in" — it does not exist yet and is not
   the attestation.

## MAJOR (FOLLOWUPS, not commit-stopping)

2. `support/integrations.toml:29-37` — `incus-engine` claims tier `none` ("no meaningful test
   surface; nothing to integrate", `support.rs:35`), but `IncusEngine` ships
   (`crates/slipfork-engine/src/incus.rs`) and is driven through the shared `WorldEngine` conformance
   suite over a local Unix-socket `/1.0` server (`crates/slipfork-engine/tests/critical_engine.rs`).
   The rationale ("no end-to-end test surface") is narrower than the tier definition, so the record
   under-claims and mislabels. ADR-0007 has no tier for "adapter shipped + conformance-tested, no live
   daemon, no recipe page" — that is a spec gap (`direction-challenge`), not something to invent here.
   Fix after human direction: either promote to `declared` with a real recipe page, or reword the tier
   semantics/rationale so `none` is truthful.
3. `crates/slipfork-dag/src/lib.rs:149-165` — `coverage = true` + `coverage_report = []` parses into
   `Some(CoverageConfig { reports: {} })`: coverage is enabled with zero report formats and silently
   produces nothing. The node's acceptance says "`coverage` cannot be set without a report format".
   Fix: reject an explicitly empty `coverage_report` with a typed error (needs lead sign-off on the
   error variant; RESULT decision 1 chose not to add one) or document the empty-set semantics in
   ADR-0006 §5 so the code stops being undocumented behaviour.
4. `docs/reference/manifest.md` (published, `zensical.toml` nav) — the page claims "Exact … `slipfork.toml`
   (`crates/slipfork-dag`) schemas implemented in `0.1.0`" but documents no `[test]` table
   (`coverage` / `coverage_report` / `coverage_fail_under`) and no `FixtureSpec.per_world`, both of
   which the shipped parser now accepts. Path-locked (not in M1.9 `paths`) → needs its own node.

## MINOR (FOLLOWUPS)

5. `support/integrations.toml:46` — `dag-config-cli` cites
   `slipfork::critical_slug::worktree_resolves_to_canonical_repo_slug_and_honors_only_vcs_root_slipfork_toml`
   (slug precedence) as the proof for the `docs/testing-dag.md` recipe; the test that actually pins
   that `[fixtures]`/`dag plan` recipe is
   `slipfork_dag::critical_dag::r9_orthogonal_invalidation_front_edit_never_invalidates_db_snapshots_and_vice_versa`.
6. `crates/slipfork-dag/src/support.rs:144-179` — `parse_toml` accepts an empty document
   (`RawRegistry` `#[serde(default)]`) and `validate()` passes; only the critical test asserts
   non-empty. `slipfork support` (M2) could therefore render an empty matrix while docs claim support.
   Fail closed on an empty registry inside `validate()`.
7. `support/integrations.toml:17-18` — `reflink-netns-world-engine` points `recipe_page` at the
   CLI `snap/fork/exec/drop` flow in `docs/getting-started.md` but cites a library conformance test;
   the "recipe pinned by a critical test" link is loose (no CLI-level critical test named).

## Checked and clean

- No `exercised`/`field-tested`/`hardened` claim anywhere; no `ci_job` claimed; `.github/workflows/`
  does not exist, so `exercised` is correctly impossible.
- All `recipe_page`/`signoff_doc` paths exist; all three `critical_test` symbols exist in
  `crates/*/tests/critical_*.rs` (`cargo test -p slipfork-dag`: 7 + 2 passed, re-run by reviewer).
- No `#[allow]`, `#[ignore]`, `unwrap()`, or weakened assertions in the touched code; the
  `critical_dag.rs` / `critical_slug.rs` edits replace deleted-field assertions with stronger typed
  ones. `deny_unknown_fields` retained on every wire struct.
- No hand-rolled TOML/serde parsing: `toml` + `serde` reused; `RawTestSpec`/`RawRegistry` are the
  documented parse-time validation layer, not duplicate schema types.
- `per_world` is not inert: digest-covered, carried on `PlannedFixture`, and fails closed on an
  origin-independent child (`DagError::OriginIndependentChildOfPerWorld`).
- ADR-0006 §5 wording change ("fails deserialization" → "fails parse-time validation") matches the
  implementation; ARCHITECTURE §3.5/§3.6 match the shipped types. No published doc claims
  `slipfork support` or the registry exists in `0.1.0`.

DOCS_CODE_AGREEMENT: divergences at `docs/reference/manifest.md` (item 4) and the undocumented
empty-`coverage_report` behaviour (item 3); everything else agrees in both directions.

---

# M1.9 REVIEW — `reviewer` (functional lens, Quad Review Gate)

VERDICT: FAIL
BASE: `09e83153c072221c7828a77de54722c607e92535`

## EVIDENCE (every `# Acceptance` item re-run independently)

| Acceptance item | Result | Command |
|---|---|---|
| 1. 0 hits for `after_suite`/`SLIPFORK_ARTIFACTS_DIR` | PASS | `rg -n "after_suite\|SLIPFORK_ARTIFACTS_DIR" crates support ARCHITECTURE.md docs CHANGELOG.md` -> no output, `exit=1` |
| 2. tooling tests | PASS | `uv run --no-project --with pytest pytest -q tools/tests` -> `6 passed in 0.05s`, exit 0 |
| 3. `cargo test -p slipfork-dag` | PASS | `critical_dag` 7 passed, `critical_support` 2 passed, 0 failed, exit 0 |
| 4. `cargo test -p slipfork -p slipfork-dag` | PASS | `critical_slug` 6 + `critical_dag` 7 + `critical_support` 2 passed, exit 0 |
| 5. registry file exists + parses typed | PASS | `test -f support/integrations.toml` exit 0; parse proven by `embedded_registry_parses_and_every_evidence_pointer_exists` |
| 6. `make check` + critical guard | PASS | `make check` exit 0 (fmt/clippy/doc/test, `zensical --strict`, `check_docs_honesty: OK (8 doc(s))`); `critical_guard.py --base 09e8315 --node M1.9` -> `OK: authorized by M1.9` (only the 3 authorized/added critical files) |
| Extra | PASS | `tools/guard.py` -> `OK (146 files)`; `tools/plan.py validate` -> `plan OK` |

Scope: every modified/untracked file is inside `NODE.md` `paths` (or `plan/**`, ignored). No unauthorized critical-test edit.

## BLOCKER (stops the commit)

1. `support/integrations.toml:37` — **fabricated `signoff_commit`.** `signoff_commit = "09e83153c072221c7828a77de54722c607e92535"` (this node's base commit) does not touch the cited `signoff_doc = "docs/decisions/ADR-0002-world-engine-ioc-incus-and-reflink.md"`: `git log --oneline -- docs/decisions/ADR-0002-*.md` returns only `798b997` (added) and `d0b11e0` (amended), and `git show --stat 09e8315` touches ADR-0001/0003/0005/0006/0007 only. `signoff_commit` is normative in `ADR-0007` §1 and documented in `ARCHITECTURE.md:397` as the "40-hex commit SHA recording the sign-off"; the only check that runs (`critical_support.rs:167-173`) is 40-hex format, so the false record would survive M2's existence check forever. Independently confirmed (concurring with `review-legality`).
   **Minimal fix:** point `signoff_commit` at `798b9972076c1d9024c2b41db3daef97522d8f65` (or `d0b11e069ced52d0e03ebb3b251a1077d0c1bf39` for the current amendment) and name in `rationale` which decision that commit signs off.

## MAJOR (FOLLOWUPS, not commit-stopping)

2. `crates/slipfork-dag/src/lib.rs:155-162` — `coverage = true` + `coverage_report = []` parses into `Some(CoverageConfig { reports: {} })`: coverage is enabled with **zero report formats** and silently reports nothing in M3. Verified end-to-end (`slipfork dag plan --file <toml with coverage_report = []>` -> exit 0). Acceptance item 3 says "`coverage` cannot be set without a report format"; the critical test only pins the `coverage = true`-alone default, so that clause is unproven for the explicit empty list.
   **Fix:** reject an explicitly empty `coverage_report` with a typed `DagError` (document it in `ADR-0006` §5 + `ARCHITECTURE.md` §3.5) or normalise `[]` to `{text}` and document that; add the assertion to `typed_test_coverage_is_single_way_and_fail_closed`.
3. `crates/slipfork-dag/src/lib.rs:367-371` — `per_world` fixtures can be reported `CacheAction::Cached`. The fixture digest is world-independent (`runtime_envs` are origin-free by contract, `ADR-0003` §1), so world B sees world A's digest and `DigestCache::has_fixture` ("already materialized") returns true -> the plan implies reusing a snapshot with world A's baked-in absolute URLs, exactly what `ADR-0003` §1 forbids ("never sealed into a shared snapshot").
   **Fix:** force `CacheAction::Rebuild` for `spec.per_world` (or add a typed `CacheAction::PerWorld`), and pin it in `per_world_fixtures_are_typed_and_cannot_parent_origin_independent_nodes`.
4. `crates/slipfork-dag/tests/critical_support.rs:167-173` — test name `embedded_registry_parses_and_every_evidence_pointer_exists` over-claims: `signoff_commit` is only 40-hex format-checked, never verified to exist or to attest the cited doc (this is what let BLOCKER-1 through).
   **Fix:** rename to `..._every_path_and_symbol_exists` and/or add `git cat-file -e <sha>` (M2 defers git checks, so rename now).
5. `docs/reference/manifest.md` §2 — published page claims "Exact … `slipfork.toml` (`crates/slipfork-dag`) schemas implemented in `0.1.0`" but documents neither the `[test]` table (`coverage`/`coverage_report`/`coverage_fail_under`) nor `FixtureSpec.per_world`, both shipped by this node. Path-locked (not in M1.9 `paths`) -> needs its own node.

## MINOR (FOLLOWUPS)

6. `support/integrations.toml:29-37` — `incus-engine` tier `none` ("no meaningful test surface", `support.rs:35`) while `IncusEngine` ships and is conformance-tested (`slipfork_engine::critical_engine::r12_incus_engine_fails_closed_when_socket_missing_and_speaks_exact_rest_contract`). The rationale is narrower than the tier label; reword it ("adapter + protocol harness shipped, no live daemon/recipe") or escalate the tier semantics.
7. `ARCHITECTURE.md:27` — diagram label `SupportRegistry::embedded() (support/integrations.toml, M2/M3)` is stale: the file, `embedded()` and `validate()` ship in `0.1.0`; only `slipfork support` is M2/M3.
8. `crates/slipfork-dag/src/lib.rs:94` — `CoverageThreshold::MAX` is a public const absent from the `ARCHITECTURE.md` §3.5 code block; `SupportRegistry`'s struct/`Serialize` derive is absent from §3.6.
9. `crates/slipfork-dag/src/support.rs:144-152` — `parse_toml("")` returns an empty registry and `validate()` passes; only the critical test asserts non-emptiness. Fail closed on an empty registry inside `validate()`.
10. `support/integrations.toml:46` — `dag-config-cli` cites the slug-precedence test; the test that pins the `docs/testing-dag.md` `[fixtures]`/`dag plan` recipe is `slipfork_dag::critical_dag::r9_orthogonal_invalidation_front_edit_never_invalidates_db_snapshots_and_vice_versa`.
11. `crates/slipfork-dag/src/lib.rs:155-158` — duplicate report formats (`["html","html"]`) collapse silently into the `BTreeSet`; reject or document the dedupe.
12. `CHANGELOG.md` 0.1.0 section — does not mention typed `[test]` coverage, `per_world`, or the support registry (out of `paths`; follow-up).

## Checked and clean

- Typed errors are real: `DagError::CoverageOptionsRequireEnabled` / `InvalidCoverageThreshold` / `OriginIndependentChildOfPerWorld` are produced by parse-time validation and planner validation, never a stringly `DagError::Toml`; `coverage_fail_under` rejects `101`, `-1`, `"eighty"`, `85.5` and accepts `0`/`100` (re-run).
- `per_world` is not inert: parsed with `#[serde(default)]`, digest-covered, carried on `PlannedFixture`, and an origin-independent child of a per-world parent fails closed.
- `SupportRegistry` uses compile-time `include_str!` (no ambient cwd), `deny_unknown_fields` on the raw document + `Evidence` + `IntegrationEntry`, tier-ordered required evidence matching `ADR-0007` §1 exactly, and duplicate-id/blank-pointer refusal with typed variants.
- No `unwrap`/`expect`/`#[allow]`/`#[ignore]`/`todo!` in the touched source; no `sleep()`; adversarial cases assert exact error variants.
- `critical_dag.rs` / `critical_slug.rs` edits replace deleted-field assertions with stronger typed ones; the orthogonal-invalidation and slug-precedence assertions are intact.

DOCS_CODE_AGREEMENT: divergences at `docs/reference/manifest.md` §2 (item 5, missing shipped `[test]`/`per_world`), `ARCHITECTURE.md:27` stale M2/M3 label (item 7), undocumented `CoverageThreshold::MAX`/`SupportRegistry` shape (item 8), and the undocumented empty-`coverage_report` behaviour (item 2). `ADR-0006` §1/§5, `ADR-0003` §1 and `ADR-0007` §1 otherwise agree with the code in both directions.
