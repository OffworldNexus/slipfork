# ADR-0001: `snow.yml` `modes`, `!!` Write-Once Secret Generation, and Open Secret Provider Protocol (`OSPP v1`)

- **Status:** Accepted — amended for M2 (`H-001`, `H-002`, `H-003`, `H-005` validated; M1 REST sketch deleted in favor of `ADR-0008` `OSPP v1` WebSocket)
- **Date:** 2026-11-15

## Context

Single-mode `snow.yml` manifests tailored only to cloud deployments (`DigitalOcean Spaces`, `Sentry`, managed PostgreSQL/Redis) force local developers and test suites to maintain parallel `.env` / `.env.test` files that drift and tempt coding agents to monkey-patch `settings.py`. Additionally, internal secrets such as `SECRET_KEY` require manual out-of-band creation before first boot, and local development components frequently need reload or sandbox commands (`uvicorn --reload`, `blastit dev`) rather than production `gunicorn` / `node build` commands.

## Decision

1. **`modes` Is the Sole Canonical Environment & Mode-Scoped Component Schema (normative):**
   - Top-level `envs:` and `project:` are hard-rejected (`SnowError::ForbiddenTopLevelKey { key }` in M2.4, carrying only the key name) — one canonical `modes:` schema with zero dual-syntax fallback branches.
   - Inside `modes:`, unquoted `all` is the sole universal base mode (`*` is rejected with `SnowError::AsteriskModeForbidden`).
   - Resolving mode `M` merges `modes.all.envs.<group>` beneath `modes.<M>.envs.<group>`.
   - **Mode-Scoped Components (`modes.<M>.components.<id>`, `H-001` + `0.1.0` release-gate confirmation, normative):** `SnowFile::effective_components(mode)` (M2.4; `effective_run_command` delegates to it) is the sole resolver computing `components[]` ⊕ `modes.<M>.components.<id>`: overriding `run_command` on an existing component (parsed and validated in `0.1.0`), adding mode-only components (`run_command` required, M2), or removing components in a mode via the sole canonical key `disabled: true` (M2 process supervision). `modes.all.components` and unknown override fields are rejected (`#[serde(deny_unknown_fields)]`).
   - **12-Factor Manifest Purity (`H-005`):** `snow.yml` stays pure of deployment identity — top-level `project:` is rejected (Kerfufoo `EnvGroup.mode` defaults to `"cloud"`; Slipfork defaults to `"local"`). Canonical VCS root discovery and `<vcs_root>/slipfork.toml` `ProjectSlug` precedence are specified normatively in `ADR-0005` §3.
2. **`!!` Ensure-Secret Operator in `Faceless` v2:**
   - `${{ secrets::<KEY> !! <generator> }}` (`random_string(N)`, `random_hex(N)`, `random_base64(N)`, `uuid4()`) reads `<KEY>` from `SecretProvider`; if missing, generates a CSPRNG value and persists it atomically via `ensure_secret` (`put_if_absent`).
   - Permitted **only** on single-key `secrets::<KEY>` (`vars::` or `resources::` with `!!` is a validation error).
   - Missing keys without `??` or `!!` collect into one exhaustive refusal error (`SnowError::MissingValues`) before any service starts.
3. **Open Secret Provider Protocol (`OSPP`) Capability, Slipfork Wire Ownership & Layered Local Persistence (`H-003`, `ADR-0008`):**
   - `OSPP` is a transport-neutral secret/variable/resource capability consumed in domain code strictly through `trait SecretProvider` (`Send + Sync`, `&self`). `OsppClient` / `OsppSecretProvider` (`crates/slipfork-ospp`) implements `SecretProvider`, and **`ADR-0008` is the sole normative wire specification** — authored and owned by Slipfork (`ospp-v1` WebSocket + `tpmd` P-256 auth + `ResourceStrategy`, to be verified against an in-tree reference server in `M2.13`–`M2.14`; Kerfufoo and compatible vaults are downstream implementers, with the pre-`1.0` M1 REST spec deleted here in `M2.1` and `slipfork_snow::OsppBundle` + `from_bundle` deleted in `M2.4`). Provider `domains::` values apply strictly to `cloud` deployments; local and shared world origins are never OSPP values, and **OSPP is not a prerequisite for M2 or M3** (`ADR-0005` §1).
   - In `local` mode, `LayeredSecretProvider` reads `FileVaultProvider` first and falls back to `OsppSecretProvider` (when paired, converging on any already-stored remote secret), while persisting newly generated `!!` secrets exclusively to `FileVaultProvider` (`0600`) so offline laptops work out of the box and never mutate shared cloud state (`H-003`).

## Consequences

- Eliminates `.env` files, `settings.py` storage hacks, and manual `SECRET_KEY` bootstrapping across both local worlds and Kerfufoo cloud deployments with one unambiguous manifest syntax.
