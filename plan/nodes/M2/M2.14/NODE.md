---
id: M2.14
title: "slipfork-ospp: vars/secrets/resources capabilities plus streaming seed dump and reference server"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M2.13]
paths: ["crates/slipfork-ospp/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-15T16:40"
updated: "2026-11-15T18:51"
---

# Goal

Turn the authenticated `OSPP v1` channel into capabilities: `vars::`, `secrets::`, and `secrets.ensure` (a real `SecretProvider` implementation), remote resource coordinates converted via `OsppClient::resource_offer()` and resolved through `slipfork_dag::ResourcePlan::resolve`, and the streaming `zstd` PostgreSQL seed-dump reader with its cache and policy flags — all exercised by an in-tree reference server harness.

# Context

- `M2.13` shipped the handshake, signer adapters, and the mock server; `M2.5` shipped `ResourcesSpec`, `RemoteResourceOffer`, and `ResourcePlan::resolve`; `M2.4` shipped the `Send + Sync` `&self` `SecretProvider` with compare-and-set `ensure_secret`.
- `ADR-0008` fixes the capability semantics: `!!` values are write-once via `put_if_absent`, `local` dev may consume the shared team S3 bucket and the team dev dump, `test` is hermetic by default, and the Slipfork-assigned `WorldOrigin` always wins over `domains::`.
- Offline-first invariant: with no server reachable, every secret still resolves from `FileVaultProvider` and world creation still succeeds. OSPP is an accelerator, never a hard requirement.

# Design / Invariants

1. **`vars::`, `secrets::`, and zero-fs `OsppSecretProvider` (`ADR-0008` §2):** Requests dispatch through one typed capability enum over the daemon-held `OsppClient` session. `OsppSecretProvider` implements `SecretProvider` (`Send + Sync`, `&self`) strictly over WebSocket with **zero filesystem writes** (`FileVaultProvider` inside `LayeredSecretProvider` is the sole `0600` local vault writer, `H-003`).
2. **Single bundle-to-offer mapper (`OsppClient::resource_offer`, `ADR-0008` §3):** `RemoteResourceBundle` (`s3_buckets.<id>` with redacted `Debug` + `pg_databases.<id>` `PgDumpMetadata`) is converted into `slipfork_dag::RemoteResourceOffer` exclusively via `OsppClient::resource_offer(&self, project: &ProjectSlug, mode: &str) -> Result<Option<slipfork_dag::RemoteResourceOffer>, OsppError>` (redacting secrets, zero second resolution logic in `slipfork-ospp`), which `slipforkd` passes straight into `slipfork_dag::ResourcePlan::resolve(spec, mode, dump_policy, offer.as_ref())`. `WorldOrigin` always overrides provider `domains::`.
3. **Streaming `zstd` seed dumps via `SeedDumpStore::write_from`:** Streams binary WebSocket `zstd` frames (`<= 64 KiB`) with a bounded window + incremental SHA-256 over compressed bytes into `SeedDumpStore::write_from(&self, sha256_compressed, &mut dyn Read)` (aborting and unlinking `.part` before any DB mutation on truncation or digest mismatch). `--no-dump | --refresh-dump` map onto `DumpPolicy { Default, NoDump, RefreshDump }` (mutually exclusive, zero env-var side doors).
4. **Reference server harness.** `tests/support/reference_server.rs` speaks the production client code path (same message types, same auth) and can serve every capability, so the suite proves the client against an independent implementation rather than against itself.
5. **Failure semantics.** Every capability error carries the capability name and the world id but never a secret value; an unauthenticated channel serves no capability at all; `share`-style features are out of scope here (they belong to the edge crate and stay gated on closed-by-deferral `H-006`).
6. `#![forbid(unsafe_code)]`; ambient env, network, and clock access confined to the WebSocket and dump adapter modules.

# Acceptance

```bash
# 1. Capabilities and single resource_offer() mapper against the reference server harness.
cargo test -p slipfork-ospp --all-targets
cargo test -p slipfork-ospp --test critical_ospp -- --nocapture vars_secrets
cargo test -p slipfork-ospp --test critical_ospp -- --nocapture ensure_converges
cargo test -p slipfork-ospp --test critical_ospp -- --nocapture resources_strategy
rg -q 'fn resource_offer' crates/slipfork-ospp/src

# 2. Hermetic test-mode default, explicit remote fail-closed, and streaming dump behaviour.
cargo test -p slipfork-ospp --test critical_ospp -- --nocapture test_mode_hermetic_default
cargo test -p slipfork-ospp --test critical_ospp -- --nocapture dump_stream
cargo test -p slipfork-ospp --test critical_ospp -- --nocapture offline_vault

# 3. Gates.
cargo clippy -p slipfork-ospp --all-targets -- -D warnings
make check
```

All commands exit `0`. Adversarial assertions required: `test` mode under default `[resources]` never pulls remote S3 or a dev DB dump even when the OSPP server offers both, an explicit required remote resource fails closed when missing, a truncated `zstd` stream aborts before any `pg_restore`, two concurrent `ensure` callers observe one value, and with the server stopped every secret still resolves from the local vault.
