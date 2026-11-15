# ADR-0001: `snow.yml` `modes`, `!!` Write-Once Secret Generation, and Open Secret Provider Protocol (`OSPP v1`)

- **Status:** Accepted (`H-001`, `H-002`, `H-003`, `H-005` validated)
- **Date:** 2026-11-14

## Context

Single-mode `snow.yml` manifests tailored only to cloud deployments (`DigitalOcean Spaces`, `Sentry`, managed PostgreSQL/Redis) force local developers and test suites to maintain parallel `.env` / `.env.test` files that drift and tempt coding agents to monkey-patch `settings.py`. Additionally, internal secrets such as `SECRET_KEY` require manual out-of-band creation before first boot, and local development components frequently need reload or sandbox commands (`uvicorn --reload`, `blastit dev`) rather than production `gunicorn` / `node build` commands.

## Decision

1. **`modes` Is the Sole Canonical Environment & Mode-Scoped Component Schema:**
   - Top-level `envs:` is hard-rejected (`SnowError::TopLevelEnvsUnsupported`) — one canonical `modes:` schema with zero dual-syntax fallback branches.
   - Inside `modes:`, unquoted `all` is the sole universal base mode (`*` is rejected with `SnowError::AsteriskModeForbidden`).
   - Resolving mode `M` merges `modes.all.envs.<group>` beneath `modes.<M>.envs.<group>`.
   - **Mode-Scoped Components (`modes.<M>.components.<id>`, `H-001` — owner-approved add/remove/override design):** `modes.<M>.components.<id>` scopes component definitions per mode: overriding `run_command` on an existing component (`SnowFile::effective_run_command`, parsed and structurally validated in `0.1.0`), adding mode-only components (such as local dev watchers/sandboxes in M2), or disabling cloud-only components in a non-cloud mode (M2 process supervision). `modes.all.components` is rejected so universal defaults have one home (`components[]`).
   - **12-Factor Manifest Purity (`H-005`):** `snow.yml` stays pure of deployment identity — top-level `project:` is rejected (Kerfufoo `EnvGroup.mode` defaults to `"cloud"`; Slipfork defaults to `"local"`). Canonical VCS root discovery and `<vcs_root>/slipfork.toml` `ProjectSlug` precedence are specified normatively in `ADR-0005` §3.
2. **`!!` Ensure-Secret Operator in `Faceless` v2:**
   - `${{ secrets::<KEY> !! <generator> }}` (`random_string(N)`, `random_hex(N)`, `random_base64(N)`, `uuid4()`) reads `<KEY>` from `SecretProvider`; if missing, generates a CSPRNG value and persists it atomically via `ensure_secret` (`put_if_absent`).
   - Permitted **only** on single-key `secrets::<KEY>` (`vars::` or `resources::` with `!!` is a validation error).
   - Missing keys without `??` or `!!` collect into one exhaustive refusal error (`SnowError::MissingValues`) before any service starts.
3. **Open Secret Provider Protocol (`OSPP v1`) & Layered Local Persistence (`H-003`):**
   - Standardizes two HTTPS JSON endpoints (`GET /v1/projects/{slug}/modes/{mode}/bundle` and `POST /v1/projects/{slug}/modes/{mode}/secrets:ensure`) implementable by Kerfufoo or any external secret manager. `OsppBundle.domains` remains in the wire schema strictly for `cloud` deployments; local/shared world origins are never OSPP values, and **OSPP is not a prerequisite for M2** (`ADR-0005` §1).
   - In `local` mode, `LayeredSecretProvider` reads `LocalVault -> RemoteOspp` (when configured) and persists `!!` generations to `LocalVault` (`0600`) so offline laptops work out of the box with `FileVaultProvider` alone and never mutate shared cloud state.

## Consequences

- Eliminates `.env` files, `settings.py` storage hacks, and manual `SECRET_KEY` bootstrapping across both local worlds and Kerfufoo cloud deployments with one unambiguous manifest syntax.
