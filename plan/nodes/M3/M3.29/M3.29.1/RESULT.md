# M3.29.1 Result — OSPP v1 Spec Ripple, Two-Tier Secret Boundary & `AF_NETLINK` Sandbox Alignment

## Summary

Rippled `docs/design/ospp.md` (commit `9098b20`, untouched and byte-identical) across `ARCHITECTURE.md`, `ADR-0001`, `ADR-0008`, `docs/VISION.md`, `docs/design/dx-0.1.0-ux.md`, and `docs/design/target-dx.md`, folding in both the `M3.28.20` `AF_NETLINK` unit-sandbox addendum and the owner's two-tier secret clarification:

1. **Single Normative OSPP v1 Wire & Synchronous DI Ports (`ARCHITECTURE.md` §§1–4):**
   - Replaced all superseded wire shapes (`env.bundle`, `secrets.ensure`, `resources.describe`, `dump.begin..dump.end`, `tpmd`, `capabilities[]`, server-speaks-first `challenge`, `LayeredSecretProvider`, `OsppSecretProvider`, `SeedDumpStore`/`DiskSeedDumpStore`) with one normative pointer to `docs/design/ospp.md` and the single client capability seam `trait OsppClient` -> `trait OsppSession` (`pair`, `connect` -> `subscribe`, `dump`, `unsubscribe`, `close`).
   - Project identity on `subscribe` is the **verbatim Git `remote` string** (via injected `RepoRootResolver::origin_remote`; `ProjectSlug` stays strictly local).
   - Each subscription generates an **in-memory** ephemeral `hpke-p256` recipient keypair (`S=` zeroized on `unsubscribe`/`close`/`Drop`): per-variable secrets use RFC 9180 HPKE Base (`info = "ospp-v1-secret"`, `aad = "<component>:<env_key>"`), and out-of-band `HTTPS GET` resource dumps use stateful `SetupBaseS`/`SetupBaseR` (`info = "ospp-v1-dump"`, `aad = "<subscription_id>:<resource>"`, `len = 0` sealed EOF) piped directly through `zstd -d | pg_restore` inside `postgres:<v>` with zero plaintext dump bytes on disk.
   - `slipforkd` owns the open WebSocket subscription lifecycle, restarting affected services in place on `env_updated`, re-subscribing with the same `id` on config/vault change, and unsubscribing on stop.
2. **Two-Tier Secret Model & Automatic ID-Only `claims` (`ARCHITECTURE.md` §3.1, `ADR-0001`, `ADR-0008`, `dx-0.1.0-ux.md`, `target-dx.md`, `VISION.md`):**
   - **Tier 1 — Local `0600` vault (`FileVaultProvider` + `slipfork secret set|unset|list` + local-mode `!!`):** Stores strictly local backing-service credentials (PostgreSQL/Redis passwords surviving restarts to reconnect to persistent Btrfs world state) and local developer overrides. Every key present in `FileVaultProvider` (plus local `resources`/`domains`/`components`) is **automatically listed by ID in `subscribe.claims`** (never values) and substituted locally.
   - **Tier 2 — Remote OSPP secrets (`OsppSession`, strictly in `slipforkd` memory):** Unclaimed server-resolved `{ "encrypted": "<base64>" }` values and encrypted `dump_ready` streams are decrypted strictly in `slipforkd` memory and **never written to `FileVaultProvider` or any file on disk**.
3. **ADRs, Pairing UX & `AF_NETLINK` Sandbox (`ADR-0001`, `ADR-0008`, `dx-0.1.0-ux.md`, `target-dx.md`):**
   - Preserved core decisions in `ADR-0001` and `ADR-0008` while marking superseded wire/layered-vault/disk-dump mechanisms in one sentence each.
   - Updated `slipfork auth <URL>` across `dx-0.1.0-ux.md` and `target-dx.md` to print and open the server-supplied `<url>#handle=<handle>` (fragment only) and return `{"ok": true, "public_key": str, "account": str, "machine": str}`, keeping `slipfork secret set|unset|list` scoped explicitly to the local `0600` vault.
   - Updated `RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX AF_NETLINK` in `ADR-0008` §5, `ARCHITECTURE.md` §2, and `target-dx.md` §1.1, documenting that `AF_NETLINK` is required for `slipfork-world-init`'s `ip link set lo up` in-netns loopback bring-up (`M3.28.20`).

## Acceptance Evidence (Corrected per Owner Two-Tier Ruling)

```console
$ ! grep -rn 'env\.bundle\|secrets\.ensure\|resources\.describe\|dump\.begin\|capabilities\[\]' ARCHITECTURE.md docs/decisions docs/design/*.md && echo old-wire-gone
old-wire-gone
$ grep -q 'never.*disk' ARCHITECTURE.md && grep -q 'local.*vault' ARCHITECTURE.md && echo two-tier-boundary
two-tier-boundary
$ grep -rq 'ospp-v1-secret' ARCHITECTURE.md && grep -rq 'ospp-v1-dump' ARCHITECTURE.md && echo hpke-documented
hpke-documented
$ grep -qE 'remote.*verbatim|verbatim.*remote' ARCHITECTURE.md && echo remote-verbatim
remote-verbatim
$ git diff --exit-code 9098b20 -- docs/design/ospp.md && echo ospp-spec-untouched
ospp-spec-untouched
$ uv run --no-project tools/guard.py && uv run --no-project tools/plan.py validate
guard: OK (589 file(s) checked)
plan OK
$ uvx --offline zensical build --strict
Build started
No issues found
Build finished in 0.22s
```

## Downstream Followups for `lead` (`M3.29.2`, `M3.29.3` & `M3.29.4`)

- **`M3.29.2` Cargo manifest & description retitle:** Add `hpke = { version = "0.15.1", default-features = false, features = ["alloc", "nistp", "aes", "hkdfsha2"] }` to workspace `Cargo.toml`, wire `base64`/`hpke` in `crates/slipfork-ospp/Cargo.toml` while removing `tokio` and `tpmd-*`, and retitle `crates/slipfork-ospp/Cargo.toml:3`'s `description` to match OSPP v1.
- **`M3.29.3` scope & `paths` alignment with owner's two-tier ruling + IoC seams:** Keep `FileVaultProvider` wired behind `crates/slipfork-edge`'s `trait SecretVault` (`get`/`put`/`remove`/`keys`) and keep `slipfork secret set|unset|list`; expand `M3.29.3` (or split leaf paths) to cover `crates/slipfork-edge/**` (`trait SecretVault`, `DaemonControl::auth_pair` + `OsppEndpoint` validation), `crates/slipfork-dag/**` (removing `RemoteResourceOffer` from `ResourcePlan::resolve`), `crates/slipfork-engine/**` (`WorldEngine::update_service_env` and clean-only `BaseSeedSpec` without remote stream variants), and `crates/slipfork-vcs/**` (`RepoRootResolver::origin_remote`).
- **`M3.29.4` public-docs alignment:** Keep `slipfork secret` documented as local-only `0600` vault management while documenting that remote OSPP secrets and dump streams never touch disk.

## Follow-Up Resolutions (`review-ioc` & `review-legality`)

1. **Capability-honest `trait SecretVault` (`crates/slipfork-edge`, IoC #1):** Declared `SecretVault { get, put, remove, keys }` in `slipfork-edge` (zero `slipfork-snow` dep), wired over `FileVaultProvider` solely at `slipforkd`'s Composition Root; `SecretProvider` stays the narrow read/ensure input to `SnowFile` evaluation, and `SecretVault::keys(project)` feeds `OsppClaims`.
2. **Deleted `RemoteResourceOffer` & `offer` input (IoC #2 / Legality):** Removed `RemoteResourceOffer` across `ARCHITECTURE.md`, `ADR-0008`, `dx-0.1.0-ux.md`, and `target-dx.md`; stated once that "remote resource offer" is not a concept in OSPP v1 (`ResourcePlan::resolve` resolves from `ResourcesSpec`, `ResourceMode`, `DumpPolicy`, and `ospp_paired` only; dumps use explicit `OsppSession::dump`).
3. **Deleted `BaseSeedSource::RemoteStream` — daemon owns streaming dump restore (IoC #3 / Legality):** `slipforkd` streams `OsppSession::dump` (`SetupBaseR`) directly into `zstd -d | pg_restore` before `WorldEngine::freeze_world`; `WorldEngine` (`crates/slipfork-engine`) sees only a clean `BaseSeedSpec` or an already-sealed base snapshot (`ADR-0009` §2 also updated to drop the `.pgcustom` mount).
4. **`WorldEngine::update_service_env(&ServiceHandle, &ExecEnv)` (IoC #4):** Added to `trait WorldEngine` (`ARCHITECTURE.md` §3.3) so `env_updated` mutates overlay env in place without `DaemonService` re-authoring `ServiceSpec`, then restarts via `M3.28.17`'s shared boot pipeline.
5. **Single `OsppEndpoint` authority (IoC #5):** Declared one boot-injected `OsppEndpoint` on `slipforkd` / `OsppClient`; `DaemonControl::auth_pair` refuses any CLI `<URL>` that differs from it.
6. **Honest `FileDeviceKeyStore` wording & `DeviceKeyPath` (IoC #6):** Clarified `crates/slipfork-ospp` performs zero **OSPP secret** or plaintext-dump writes while `FileDeviceKeyStore` writes only the `0600` machine-auth P-256 scalar at injected `DeviceKeyPath`.
7. **Unclaimed template-free plaintext guarantee & single substitution port (IoC #7):** Documented that `docs/design/ospp.md` guarantees template-free plaintext for unclaimed entries (resolved or refused), while claimed `${{ ... }}` entries are substituted locally solely through `SecretProvider` (`SecretVault`) + local `ResourceCoordinates` without re-scanning server outputs.
8. **`M3.29` target-state Cargo dependencies (Legality #1):** Labeled `crates/slipfork-ospp` as `M3.29` target state (`M3.29.2`) and pinned `hpke = { version = "0.15.1", default-features = false, features = ["alloc", "nistp", "aes", "hkdfsha2"] }`.
9. **Single `auth_failed` CLI error code (Legality #2):** Added `auth_failed` (`extras`: `detail`) to `dx-0.1.0-ux.md` §10 mapping `rejected | already_paired | throttled | bad_proof | endpoint_mismatch | disconnected`.
10. **Client opens dumps with `SetupBaseR` only (Legality #3):** Fixed `ARCHITECTURE.md` §2 & §3.2 so `SetupBaseS` is server-side only and the client opens streams exclusively with `SetupBaseR`.
11. **`ADR-0011` `tpmd` supersede note & deduplicated ADR history (Legality #4):** Updated `ADR-0011` §2 (`slipforkd` no longer reads `XDG_RUNTIME_DIR` after `M3.29` deletes the `tpmd` signer in `ADR-0008` §1) and consolidated each ADR to one non-duplicated supersession sentence.
