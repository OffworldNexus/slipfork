# Result M1.8 (Attempt 2/3)

Folded all four owner release-gate decisions, the OSPP origin-precedence addendum, and the ratified `H-008` shared-ingress ruling into Slipfork's design of record (docs/ADRs/roadmap only; zero `crates/`, `packages/`, `poc/`, or `tools/` edits):

1. **Single Canonical Origin & Sole Shared Ingress (`ADR-0005`, `ADR-0003`, `ADR-0001`, `ARCHITECTURE.md`, `docs/design/target-dx.md`, `plan/ROADMAP.md`, `H-008`):**
   - Ratified `WorldOrigin = Local { url } | Shared { url }` (`crates/slipfork-world` M2) with the owner's release-gate directive quoted verbatim in `ADR-0005` §1 (`H-008`); world cards and `slipfork list` print **exactly one URL per world**, and `LocalHostRoute::url()` / `ResourceCoordinates::local_for_world` is the sole constructor of `WorldOrigin::Local { url }`.
   - Closed the shared-ingress DI loop (`H-008` Option A): `slipforkd` is the **sole ingress** for both `Local` and `Shared` worlds. `WorldRegistry` registers `TunnelRequest` against `slipforkd`'s **planned static port** ahead of materialization (minting `WorldOrigin::Shared` from `TunnelLease::public_url()` before boot and before `per_world = true` seeds, with zero two-origin window), populates `shared_hosts: BTreeMap<String, (ProjectSlug, WorldTarget)>` via `DaemonControl` (`register_world` / `release_world` + `share_world` / `unshare_world`, returning `SharedWorldInfo { public_url, lease_id }`), and proxies shared hostnames via `dial_world_tcp` with `Host` + `X-Forwarded-*` preserved.
   - Scoped `308 Permanent Redirect` (`Expect: 100-continue` answered pre-body) and `421 Misdirected Request` WebSocket refusal strictly to `*.localhost` `Host` headers when a world is `Shared` — explicitly collapsing `snow_domain` ingresses (`admin.<project>.localhost`) into the single shared origin.
   - Recorded `WorldOrigin` non-OSPP ownership, local precedence over provider `domains::`, **OSPP non-prerequisite for M2/M3**, and origin-independent vs `per_world = true` DAG rules (`ADR-0003` §1).

2. **Integration Support Tiers (`ADR-0007`, `ARCHITECTURE.md` §3.6):**
   - Defined typed `IntegrationEntry.evidence` fields separating **machine-verified** tiers (`declared`: `recipe_page` + `critical_test`; `exercised`: `+ ci_job`) from **attested + required-artifact** tiers (`none`: `rationale` + `signoff_doc` + `signoff_commit`; `field-tested`: `+ deploy_ref`; `hardened`: `+ production_suite_ref` + `test_audit`).
   - Specified deterministic `SupportRegistry::embedded()` loading via `include_str!("../../../support/integrations.toml")` (`parse_toml(&str)` in tests, zero ambient `cwd` reads), a **read-only fail-closed `make check` ceiling**, and explicit **`make support-sync`** demotion + docs-table regeneration.

3. **Built-In Typed Coverage, Canonical Per-Run Tree & Accurate `0.1.0` Crate State (`ADR-0006`, `ARCHITECTURE.md` §3.5, `docs/design/*`):**
   - Unified the per-run tree to `.slipfork/runs/<run_id>/<slot>/{runner.sock,coverage,output}` (`SLIPFORK_OUTPUT_DIR`), replaced `WorldEndpoint.app_url` with `origin: WorldOrigin`, documented slot-stable coverage mounts at pre-warm world boot (avoiding `spawn_service` restarts on slot adoption), and unified `[test]` activation (`coverage = true` as sole TOML switch -> `TestSpec { coverage: Option<CoverageConfig> }` with `CoverageThreshold` validated in `0..=100` via `DagError::InvalidCoverageThreshold` / `TestError::InvalidCoverageThreshold`).
   - Removed "unreferenced placeholder" and "test-scoped hooks" claims: `[test]` carries only typed configuration fields, and `0.1.0`'s `slipfork_dag::TestSpec` carries one dead, never-executed string field (pinned by M1 critical tests) scheduled for mandatory deletion before `0.1.0` merges.

## Required Pre-`0.1.0`-Merge Follow-On Code Node (Release Blocker Outside M1.8's Docs Path Lock)

- **REQUIRED BEFORE `0.1.0` MERGES (`M1.9`):** Delete the rejected dead string field on `crates/slipfork-dag/src/lib.rs`'s `TestSpec` along with its assertions in `crates/slipfork-dag/tests/critical_dag.rs` and `crates/slipfork/tests/critical_slug.rs`, replace `TestSpec` with the typed `Option<CoverageConfig>` / `CoverageThreshold(0..=100)` schema (`ADR-0006` §5), add `FixtureSpec.per_world: bool` (`ADR-0003` §1), and commit the initial `support/integrations.toml` registry (`ADR-0007`).

## Acceptance Evidence (Attempt 2 Re-Runs)

- PASS (`exit 1`, 0 hits): `rg -n "after_suite|SLIPFORK_ARTIFACTS_DIR" ARCHITECTURE.md docs CHANGELOG.md`
- PASS (`exit 1`, 0 hits): `rg -n "after_suite|SLIPFORK_ARTIFACTS_DIR|SLIPFORK_RUN_ARTIFACTS_DIR" . --glob '!crates/**' --glob '!packages/**' --glob '!plan/**' --glob '!.git/**'`
- PASS (`exit 0`, normative `*.localhost`-scoped `308`/`421` + `H-008` ratification): `rg -n "single origin|at most one (public )?origin|308" docs/decisions/ADR-0005*`
- PASS (`exit 0`, typed evidence fields + verification class): `test -f docs/decisions/ADR-0007*`
- PASS (`exit 0`): `uvx --offline zensical build --strict && uv run --no-project tools/check_docs_honesty.py`
- PASS (`exit 0`): `make check` and `uv run --no-project tools/critical_guard.py --base 05c85939f7514d9286691b6c8e01e10055bdba64 --node M1.8`
