# ADR-0007: Integration Support Tiers, Evidence Registry, and `make check` Ceiling Enforcement

- **Status:** Accepted (normative specification for integration support tiers, `support/integrations.toml` schema, `slipfork support`, and `make check` / `make support-sync` governance; registry file enters before `0.1.0` merge or in M2, while CLI & CI verifier land in M2/M3 — nothing in the `0.1.0` binary today)
- **Date:** 2026-11-15

## Context

Slipfork targets multiple hypervisors (`ReflinkNetnsEngine`, `IncusEngine`), VCS backends (`Git`, `Jujutsu`), secret/tunnel integrations (`FileVaultProvider`, `OSPP v1`, `weaved`), and language/framework test stacks (`Django`/`pytest`, `FastAPI`/`Flask`, `Playwright`/`Vitest`, `PHPUnit`, `cargo nextest`, generic CLIs). Ad-hoc prose lists of "supported frameworks" blur the line between a sketched manifest recipe, a CI-exercised adapter, and a battle-tested production integration — inviting accidental vaporware or stale claims when tests or CI jobs change.

## Decision

1. **Ordered 5-Tier Support Scale Separating Machine-Verified vs Attested + Artifact Tiers (normative):**
   Every integration in `support/integrations.toml` declares an `id`, `category`, claimed `tier` (`none < declared < exercised < field-tested < hardened`), and typed `evidence` fields (`#[serde(deny_unknown_fields)]`). Tiers divide explicitly into **machine-verified repository tiers** (`declared`, `exercised`, proven entirely by in-repo files, test symbols, and CI workflow jobs) and **attested + required-artifact tiers** (`none`, `field-tested`, `hardened`, which pair human/audit attestation with a required verifiable repository artifact or commit):

   | Tier | Verification Class | Meaning | Required Typed `IntegrationEntry.evidence` Fields in Repo |
   |---|---|---|---|
   | `none` | Attested + artifact | No meaningful test surface; nothing to integrate (e.g. a pure CLI library) | `rationale` (non-empty) + `signoff_doc` (existing ADR/review path) + `signoff_commit` (40-hex commit SHA) |
   | `declared` | **Machine-verified** | Manifest recipe written, never exercised outside its author | `recipe_page` (existing doc path) + `critical_test` (`<crate>::<test_file>::<test_fn>` symbol in `critical_*`) |
   | `exercised` | **Machine-verified** | CI runs a real project through it | `declared` fields + `ci_job` (`<workflow.yml>#<job_id>` present in `.github/workflows/` with passing gate) |
   | `field-tested` | Attested + artifact | A real team/product runs its suite through it | `exercised` fields + `deploy_ref` (committed deployment record `support/deployments/<id>.md`) |
   | `hardened` | Attested + artifact | Field-tested at scale, including browser/multi-worker/flaky-suite behaviour | `field-tested` fields + `production_suite_ref` (committed scale record) + `test_audit` (`docs/testing/AUDIT-*.md`) |

2. **Single Embedded Source of Truth, CLI View, and Generated Docs (M2/M3 target — not in `0.1.0`):**
   - **Single Registry & Deterministic Loading:** `support/integrations.toml` is the sole source of truth (hand-maintained prose support matrices are forbidden). `slipfork support` loads it via **compile-time `include_str!("../../../support/integrations.toml")` (`SupportRegistry::embedded()`)**, while unit/conformance tests call pure `SupportRegistry::parse_toml(&str)` — **zero ambient `cwd` or runtime filesystem reads**.
   - **CLI View (`slipfork support`):** M2/M3 adds `slipfork support` (including `--json` machine output) rendering each integration's verified tier and evidence pointers.
   - **Generated Documentation Table:** Published documentation renders its support matrix strictly from `support/integrations.toml` so the Zensical site can never drift from reality.

3. **Governance: Plan-Node Promotion, Read-Only `make check` Ceiling, and Explicit `make support-sync`:**
   - **Promotion is an explicit plan node:** Advancing an integration's tier requires a dedicated plan task supplying the required evidence fields and passing the Quad Review Gate.
   - **One read-only check + one explicit mutator:**
     - **`make check` is a strictly read-only fail-closed ceiling:** It verifies every evidence pointer on disk (file existence, `critical_*` test function symbol, `.github/workflows/*.yml` job key, `git` commit format/existence, and `AUDIT-*.md` path) and **exits non-zero** if any `IntegrationEntry` claims a tier above its present evidence or if the generated docs table differs from `support/integrations.toml`. `make check` never mutates files.
     - **`make support-sync` performs demotion and table regeneration:** When a test, recipe, CI job, or audit is removed, running `make support-sync` lowers affected entries in `support/integrations.toml` to the highest tier whose required evidence remains present (never promoting) and rewrites the generated docs table.
   - **Replaces Ad-Hoc Claims:** Replaces all ad-hoc "supported frameworks" claims in `docs/VISION.md`, `README.md`, and published `docs/`. Until `slipfork support` and the CI verifier ship in M2/M3, `0.1.0` docs claim only shipped M1 primitives and the M1 PoC benchmark (`make bench`).

## Consequences

- Support claims become typed, non-self-attesting repository contracts: `make check` fails closed on any deleted test or CI job without mutating the working tree, while `make support-sync` demotes stale tiers deterministically.
