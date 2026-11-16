# ADR-0007: Integration Support Tiers, Evidence Registry, and `make check` Ceiling Enforcement

- **Status:** Accepted — amended for M2 (`H-009` Option B ratified; schema + embedded `support/integrations.toml` + `critical_support.rs` shipped in `0.1.0`; `slipfork support [--json]` with `required_evidence`, generated `docs/support-matrix.md`, read-only `make check` ceiling, and explicit `make support-sync` land in M2)
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

   - **User-Reachable Scope Rule & Category Mapping (`H-009` Option B, normative):** The 5-tier scale (`none`, `declared`, `exercised`, `field-tested`, `hardened`) is scoped strictly to **user-reachable** integrations across the six `IntegrationCategory` variants (`world-engine`, `secrets` including `ospp-v1` auth/dumps, `tunnel`, `vcs` including `git`/`jj` workspaces and `git-remote-slipfork`, `test-runner`, and `cli` including `slipforkd` edge & world CLI). Internal library adapters whose user-facing CLI wiring or published recipe has not shipped yet (`incus-engine` while gated on open `H-004`, `WeavedRegistrar` while gated on deferred `weaved` IPC per closed `H-006`) MUST be **omitted** from `support/integrations.toml` until user-reachable at `declared` (`0.1.0` shipped a temporary `incus-engine` `tier = "none"` placeholder which `M2.5` deletes). Tier `none` is reserved strictly for platforms/libraries with no meaningful lifecycle or test integration surface.

2. **Single Embedded Source of Truth (`0.1.0`), CLI View with `required_evidence`, and Generated Docs (M2):**
   - **Single Registry & Deterministic Loading (shipped in `0.1.0`):** `support/integrations.toml` is the sole source of truth (hand-maintained prose support matrices are forbidden). `crates/slipfork-dag` embeds it at compile time via **`include_str!("../../../support/integrations.toml")` (`SupportRegistry::embedded()`)**, while unit/conformance tests call pure `SupportRegistry::parse_toml(&str)` — **zero ambient `cwd` or runtime filesystem reads**. `crates/slipfork-dag/tests/critical_support.rs` verifies unique IDs, required evidence presence, **all present** `EvidenceField` file paths and `critical_*` symbols, and 40-hex `signoff_commit` shape.
   - **CLI View (`slipfork support [--json]`, M2):** `crates/slipfork/src/main.rs` wires `slipfork support` in M2 to render a human table by default and, under `--json`, each integration's verified `tier`, `evidence`, and **`required_evidence` array sourced directly from `IntegrationTier::required_evidence()`** so `tools/check_support.py` consumes the single Rust ladder rather than re-deriving tier rules in Python. `slipfork support` always reports and exits `0`; gating belongs to `tools/check_support.py`.
   - **Generated Documentation Table (`docs/support-matrix.md`, M2):** Published documentation renders its support matrix strictly from `support/integrations.toml` via one deterministic renderer (`make support-sync`) so the Zensical site can never drift from reality.

3. **Governance: Plan-Node Promotion, Read-Only `make check` Ceiling, and Explicit `make support-sync` (M2):**
   - **Promotion is an explicit plan node:** Advancing an integration's tier requires a dedicated plan task supplying the required evidence fields and passing the Quad Review Gate.
   - **One read-only check + one explicit mutator (`tools/check_support.py`):**
     - **`make check` (`tools/check_support.py --check`) is a strictly read-only fail-closed ceiling:** Consumes `slipfork support --json` (`required_evidence` from the single Rust ladder), verifies `signoff_commit` exists in `git` and touches `signoff_doc`, verifies `.github/workflows/*.yml` `ci_job` keys, checks all present file-path and `critical_*` symbol pointers, and verifies `docs/support-matrix.md` is **byte-identical to the rendered Markdown output of `support/integrations.toml`** — **exiting non-zero** on any discrepancy without mutating files.
     - **`make support-sync` (`tools/check_support.py --sync`) performs explicit demotion and table regeneration:** Inspects every entry in `support/integrations.toml`; when required evidence for a claimed tier is missing, prints every demotion (`id`, `from -> to`, missing pointer), lowers affected entries to the highest tier whose required evidence remains present (never promoting, and failing closed if no valid lower tier is satisfied under `H-009` Option B), and writes `docs/support-matrix.md`.
   - **Replaces Ad-Hoc Claims:** Replaces all ad-hoc "supported frameworks" claims in `docs/VISION.md`, `README.md`, and published `docs/`. Until `slipfork support` and `docs/support-matrix.md` ship in M2, `0.1.0` published docs claim only shipped M1 primitives and the M1 PoC benchmark (`make bench`).

## Consequences

- Support claims become typed, non-self-attesting repository contracts: `make check` fails closed on any deleted test or CI job without mutating the working tree, while `make support-sync` demotes stale tiers deterministically.
