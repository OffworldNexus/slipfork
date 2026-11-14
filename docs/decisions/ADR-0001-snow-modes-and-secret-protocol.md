# ADR-0001: `snow.yml` Modes, `!!` Write-Once Secret Generation, and Open Secret Provider Protocol (`OSPP v1`)

- **Status:** Proposed (awaiting M1 human validation: `H-001`, `H-002`, `H-003`)
- **Date:** 2026-11-14

## Context

Kerfufoo's existing `snow.yml` defines a single top-level `envs:` map tailored to cloud deployments (`DigitalOcean Spaces`, `Sentry`, managed PostgreSQL/Redis), forcing local developers and test suites to maintain parallel `.env` / `.env.test` files that constantly drift and tempt coding agents to monkey-patch `settings.py`. Additionally, internal secrets such as Django's `SECRET_KEY` require manual out-of-band creation before first boot.

## Decision

1. **`modes` Replaces Top-Level `envs` (Mutually Exclusive):**
   - `snow.yml` defines **either** `envs:` (legacy single-mode) **or** `modes:` — having both is a hard validation error.
   - Inside `modes:`, `all` is the sole universal base mode (bare/quoted `*` is rejected because bare `*` is a YAML alias token and Law #3 forbids dual syntaxes).
   - Resolving mode `M` merges `modes.all.envs.<group>` beneath `modes.<M>.envs.<group>`. Kerfufoo `EnvGroup.mode` defaults to `"cloud"`; Slipfork defaults to `"local"`.
2. **`!!` Ensure-Secret Operator in `Faceless` v2:**
   - `${{ secrets::<KEY> !! <generator> }}` (`random_string(N)`, `random_hex(N)`, `random_base64(N)`, `uuid4()`) reads `<KEY>` from `SecretProvider`; if missing, generates a CSPRNG value and persists it atomically via `ensure_secret` (`put_if_absent`).
   - Permitted **only** on the `secrets` namespace (`vars::` or `resources::` with `!!` is a validation error).
   - Missing keys without `??` or `!!` collect into one exhaustive refusal error (`SnowError::MissingValues`) before any service starts.
3. **Open Secret Provider Protocol (`OSPP v1`) & Layered Local Persistence:**
   - Standardizes two HTTPS JSON endpoints (`GET /v1/projects/{slug}/modes/{mode}/bundle` and `POST /v1/projects/{slug}/modes/{mode}/secrets:ensure`) implementable by Kerfufoo or any external secret manager.
   - In `local` mode, `LayeredSecretProvider` reads `LocalVault -> RemoteOspp` and persists `!!` generations to `LocalVault` (`0600`) so offline laptops work out of the box without mutating shared cloud state.

## Consequences

- Eliminates `.env` files, `settings.py` storage hacks, and manual `SECRET_KEY` bootstrapping across both local worlds and Kerfufoo cloud deployments.
