# ADR-0001: `snow.yml` `modes`, `!!` Write-Once Secret Generation, and Open Secret Provider Protocol (`OSPP v1`)

- **Status:** Accepted — amended for `0.1.0` OSPP v1 (`docs/design/ospp.md`, `ADR-0008`, `M3.29`; `H-001`, `H-002`, `H-003`, `H-005` validated; M1 REST sketch and M2 `LayeredSecretProvider`/`OsppSecretProvider` deleted pre-`1.0`)
- **Date:** 2026-11-15

## Context

Single-mode `snow.yml` manifests tailored only to cloud deployments (`DigitalOcean Spaces`, `Sentry`, managed PostgreSQL/Redis) force local developers and test suites to maintain parallel `.env` / `.env.test` files that drift and tempt coding agents to monkey-patch `settings.py`. Additionally, internal secrets such as `SECRET_KEY` and local backing-service passwords require automatic write-once creation that survives daemon/host restarts, while CI-managed secrets must be fetched end-to-end encrypted without ever touching local disk, and local development components frequently need reload or sandbox commands (`uvicorn --reload`, `blastit dev`) rather than production `gunicorn` / `node build` commands.

## Decision

1. **`modes` Is the Sole Canonical Environment & Mode-Scoped Component Schema (normative):**
   - Top-level `envs:` and `project:` are hard-rejected (`SnowError::ForbiddenTopLevelKey { key }` in M2.4, carrying only the key name) — one canonical `modes:` schema with zero dual-syntax fallback branches.
   - Inside `modes:`, unquoted `all` is the sole universal base mode (`*` is rejected with `SnowError::AsteriskModeForbidden`).
   - Resolving mode `M` merges `modes.all.envs.<group>` beneath `modes.<M>.envs.<group>`.
   - **Mode-Scoped Components (`modes.<M>.components.<id>`, `H-001` + `0.1.0` release-gate confirmation, normative):** `SnowFile::effective_components(mode)` (M2.4; `effective_run_command` delegates to it) is the sole resolver computing `components[]` ⊕ `modes.<M>.components.<id>`: overriding `run_command` on an existing component (parsed and validated in `0.1.0`), adding mode-only components (`run_command` required, M2), or removing components in a mode via the sole canonical key `disabled: true` (M2 process supervision). `modes.all.components` and unknown override fields are rejected (`#[serde(deny_unknown_fields)]`).
   - **12-Factor Manifest Purity (`H-005`):** `snow.yml` stays pure of deployment identity — top-level `project:` is rejected (Kerfufoo `EnvGroup.mode` defaults to `"cloud"`; Slipfork defaults to `"local"`). Canonical VCS root discovery and `<vcs_root>/slipfork.toml` `ProjectSlug` precedence (local only — OSPP `subscribe` uses the verbatim Git `remote` string) are specified normatively in `ADR-0005` §3 and `docs/design/ospp.md`.
2. **`!!` Ensure-Secret Operator in `Faceless` v2:**
   - `${{ secrets::<KEY> !! <generator> }}` (`random_string(N)`, `random_hex(N)`, `random_base64(N)`, `uuid4()`) reads `<KEY>` from `SecretProvider`; if missing, generates a CSPRNG value and persists it atomically via `ensure_secret` (`put_if_absent`).
   - Permitted **only** on single-key `secrets::<KEY>` (`vars::` or `resources::` with `!!` is a validation error).
   - Missing keys without `??` or `!!` collect into one exhaustive refusal error (`SnowError::MissingValues`) before any service starts.
3. **Open Secret Provider Protocol (`OSPP v1`) as CI Clutch & Two-Tier Secret Boundary (`docs/design/ospp.md`, `ADR-0008`, `H-003`):**
   - **CI holds remote authority; remote OSPP secrets never touch disk.** **`docs/design/ospp.md` is the sole normative OSPP v1 wire specification** (`Sec-WebSocket-Protocol: ospp-v1` all-JSON persistent WebSocket, `p256` auth & ALTCHA+PoP browser-fragment pairing, verbatim Git `remote` project identity, uniform ID-only `claims`, per-subscription in-memory `hpke-p256` secret encryption + out-of-band encrypted resource dumps, and live `env_updated` pushes). Provider `domains::` values apply strictly to `cloud` deployments; local world origins are never OSPP values, and **OSPP is never a prerequisite for local worlds** (`ADR-0005` §1).
   - **Two-tier secret boundary (`ARCHITECTURE.md` §3.1):** `FileVaultProvider` (`0600` `<data_home>/slipfork/vaults/<project>.json`, exposed to `DaemonService` solely through `crates/slipfork-edge`'s `trait SecretVault` and to `SnowFile` evaluation through read/ensure `trait SecretProvider`) stores **strictly local** credentials and overrides — locally-provisioned backing-service credentials (local PostgreSQL/Redis passwords that must survive restarts to reconnect to persistent Btrfs subvolume state), `slipfork secret` overrides, and local-mode `!!` secrets. `SecretVault::keys` (plus local `resources`/`domains`/`components`) automatically populates ID-only `subscribe.claims`, while unclaimed server-resolved variables (guaranteed template-free plaintext) and `{ "encrypted": "<base64>" }` secrets / dump streams are decrypted strictly in `slipforkd` memory and **never written to `FileVaultProvider` or any file on disk**.
   - *(Superseded M1 draft deleted pre-`1.0`: M1's two-endpoint REST sketch and `slipfork_snow::OsppBundle` + `from_bundle` were deleted in favor of `docs/design/ospp.md` and `ADR-0008`.)*

## Consequences

- Eliminates `.env` files, `settings.py` storage hacks, and manual `SECRET_KEY` bootstrapping across offline local worlds and CI-paired worlds while preserving local Btrfs service credentials across restarts and keeping remote CI secrets 100% off disk.
