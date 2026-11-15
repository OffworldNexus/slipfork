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
updated: "2026-11-15T16:40"
---

# Goal

Turn the authenticated `OSPP v1` channel into capabilities: `vars::`, `secrets::`, and `secrets.ensure` (a real `SecretProvider` implementation), remote resource coordinates resolved through `[resources] ResourceStrategy`, and the streaming `zstd` PostgreSQL seed-dump reader with its cache and policy flags — all exercised by an in-tree reference server harness.

# Context

- `M2.13` shipped the handshake, signer adapters, and the mock server; `M2.5` shipped `ResourcesSpec::defaults_for(mode)` and the fail-closed `test`-mode rules; `M2.4` shipped the `Send + Sync` `&self` `SecretProvider` with compare-and-set `ensure_secret`.
- `ADR-0008` fixes the capability semantics: `!!` values are write-once via `put_if_absent`, `local` dev may consume the shared team S3 bucket and the team dev dump, `test` is hermetic by default, and the Slipfork-assigned `WorldOrigin` always wins over `domains::`.
- Offline-first invariant: with no server reachable, every secret still resolves from `FileVaultProvider` and world creation still succeeds. OSPP is an accelerator, never a hard requirement.

# Design / Invariants

1. **`vars::` and `secrets::`.** Lookups go through one typed capability enum (never a stringly dispatch map) and cache only within a single world boot scope. `secrets.ensure` implements `SecretProvider::ensure_secret` by delegating to the remote compare-and-set and then persisting the authoritative value in the `0600` vault; two concurrent ensurers across a remote and a local path converge on exactly one value.
2. **Resource coordinates.** `s3_buckets.<id>.{endpoint, region, bucket, access_key, secret_key}` and `pg_databases.<id>` dump metadata deserialize into typed structs with redacted `Debug` on secrets. Resolution consults the effective `[resources]` `ResourceStrategy` (`ResourcesSpec` + `defaults_for(mode)`): `local` dev defaults to consuming an OSPP-offered shared dev S3 bucket (falling back cleanly to local S3 when OSPP does not offer that bucket or when offline), while `test` defaults **hermetically** to local ephemeral S3 and clean `initdb -> migrate -> [fixtures.*]` (ignoring remote S3 and skipping remote dev dumps unless explicitly configured in `[resources]`). When `[resources]` explicitly requires a remote resource or dump (`required` / explicit remote strategy) and OSPP is unreachable or omits it, resolution fails closed (`OsppError::RequiredRemoteResourceUnavailable`) rather than silently substituting local state. A `domains::` entry is always outweighed by the `WorldOrigin` assigned before materialization.
3. **Seed dumps.** The reader streams `zstd` frames with a bounded window and never buffers the whole dump in memory; the digest is SHA-256 over the compressed stream and keys the artifact cache; materialization applies `pg_restore` into the `base` snapshot before any `pre-deploy` hook. `--no-dump` and `--refresh-dump` are explicit typed policy values passed through the composition root, never environment variables. A digest mismatch aborts before any mutation.
4. **Reference server harness.** `tests/support/reference_server.rs` speaks the production client code path (same message types, same auth) and can serve every capability, so the suite proves the client against an independent implementation rather than against itself.
5. **Failure semantics.** Every capability error carries the capability name and the world id but never a secret value; an unauthenticated channel serves no capability at all; `share`-style features are out of scope here (they belong to the edge crate and stay gated on the open external-input items).
6. `#![forbid(unsafe_code)]`; ambient env, network, and clock access confined to the WebSocket and dump adapter modules.

# Acceptance

```bash
# 1. Capabilities against the reference server harness.
cargo test -p slipfork-ospp --all-targets
cargo test -p slipfork-ospp --test critical_ospp -- --nocapture vars_secrets
cargo test -p slipfork-ospp --test critical_ospp -- --nocapture ensure_converges
cargo test -p slipfork-ospp --test critical_ospp -- --nocapture resources_strategy

# 2. Hermetic test-mode default, explicit remote fail-closed, and streaming dump behaviour.
cargo test -p slipfork-ospp --test critical_ospp -- --nocapture test_mode_hermetic_default
cargo test -p slipfork-ospp --test critical_ospp -- --nocapture dump_stream
cargo test -p slipfork-ospp --test critical_ospp -- --nocapture offline_vault

# 3. Gates.
cargo clippy -p slipfork-ospp --all-targets -- -D warnings
make check
```

All commands exit `0`. Adversarial assertions required: `test` mode under default `[resources]` never pulls remote S3 or a dev DB dump even when the OSPP server offers both, an explicit required remote resource fails closed when missing, a truncated `zstd` stream aborts before any `pg_restore`, two concurrent `ensure` callers observe one value, and with the server stopped every secret still resolves from the local vault.
