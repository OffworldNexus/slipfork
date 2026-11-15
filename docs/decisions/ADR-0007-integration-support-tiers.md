# ADR-0007: Integration Support Tiers, Evidence Registry, and `make check` Ceiling Enforcement

- **Status:** Accepted — schema & embedded registry shipped in `0.1.0` (`crates/slipfork-dag` + `support/integrations.toml` + `critical_support.rs`); `slipfork support` CLI, generated docs matrix, `git`/CI compliance verifier, and `H-009` pre-`declared` adapter classification land in M2/M3 (no `slipfork support` subcommand in the `0.1.0` binary)
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

   - **Conservative `0.1.0` Floor & `H-009` Pre-CLI Adapter Rule:** In `0.1.0`, `incus-engine` ships as a library Unix-socket REST `/1.0` adapter passing the shared `WorldEngine` conformance suite while CLI/daemon wiring awaits `H-004` (so no user `recipe_page` exists under Zero Vaporware); it is conservatively recorded at `tier = "none"` with explicit disclosure in `rationale`. Before M2 renders `slipfork support [--json]` or the generated docs table, `H-009` (`plan/HUMAN.md`) settles whether M2 keeps the 5-tier scale strictly scoped to user-reachable integrations (omitting pre-CLI adapters until `declared`) or adds an intermediate conformance-only rung.

2. **Single Embedded Source of Truth (`0.1.0`), CLI View, and Generated Docs (M2/M3):**
   - **Single Registry & Deterministic Loading (`0.1.0`):** `support/integrations.toml` is the sole source of truth (hand-maintained prose support matrices are forbidden). `crates/slipfork-dag` embeds it at compile time via **`include_str!("../../../support/integrations.toml")` (`SupportRegistry::embedded()`)**, while unit/conformance tests call pure `SupportRegistry::parse_toml(&str)` — **zero ambient `cwd` or runtime filesystem reads**. `crates/slipfork-dag/tests/critical_support.rs` verifies unique IDs, required evidence presence, tier-required file paths and `critical_*` symbols, and 40-hex `signoff_commit` shape in `0.1.0`.
   - **CLI View (`slipfork support`, M2):** `crates/slipfork/src/main.rs` wires `slipfork support` (including `--json` machine output) in M2 to render each integration's verified tier and evidence pointers.
   - **Generated Documentation Table (M2):** Published documentation renders its support matrix strictly from `support/integrations.toml` so the Zensical site can never drift from reality.

3. **Governance: Plan-Node Promotion, Read-Only `make check` Ceiling, and Explicit `make support-sync` (M2/M3):**
   - **Promotion is an explicit plan node:** Advancing an integration's tier requires a dedicated plan task supplying the required evidence fields and passing the Quad Review Gate.
   - **One read-only check + one explicit mutator:**
     - **`make check` is a strictly read-only fail-closed ceiling:** In M2, the compliance verifier adds `git` commit existence/attestation (verifying `signoff_commit` exists and touches `signoff_doc`) and `.github/workflows/*.yml` job-key checks alongside file-path and `critical_*` symbol checks, **exiting non-zero** if any `IntegrationEntry` claims a tier above its present evidence or if the generated docs table differs from `support/integrations.toml`. `make check` never mutates files.
     - **`make support-sync` performs explicit demotion and table regeneration:** When a test, recipe, CI job, or audit is removed, running `make support-sync` prints every demotion (`id`, `from -> to`, missing pointer), lowers affected entries in `support/integrations.toml` to the highest tier whose required evidence remains present (never promoting), and rewrites the generated docs table.
   - **Replaces Ad-Hoc Claims:** Replaces all ad-hoc "supported frameworks" claims in `docs/VISION.md`, `README.md`, and published `docs/`. Until `slipfork support` and the generated docs table ship in M2, `0.1.0` published docs claim only shipped M1 primitives and the M1 PoC benchmark (`make bench`).

## Consequences

- Support claims become typed, non-self-attesting repository contracts: `make check` fails closed on any deleted test or CI job without mutating the working tree, while `make support-sync` demotes stale tiers deterministically.
