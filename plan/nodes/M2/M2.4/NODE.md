---
id: M2.4
title: "slipfork-snow: Send+Sync providers, redacted Debug, mode-scoped effective_components"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M2.3]
paths: ["crates/slipfork-snow/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-15T16:40"
updated: "2026-11-15T16:40"
---

# Goal

Make `slipfork-snow`'s secret plumbing concurrency-safe and leak-free (`Send + Sync` `SecretProvider`/`EntropySource` with `&self`, redacting `Debug`), and land the mode-scoped `modes.<mode>.components.<id>` add/remove/override resolution that `slipfork up` supervision consumes in M2.

# Context

- M2 resolves worlds and secrets from several threads, but `SecretProvider::ensure_secret` currently takes `&mut self`, which forces exclusive ownership of the vault.
- M1.6 follow-up #21 (highest-priority hygiene item): `ResourceCoordinates`/`S3Coordinates` derive `Debug` with a `pub secret_key`, and `RejectedKey` derives `Debug` over a raw `serde_json::Value`, so a literal secret inside a top-level `envs:` block would print on `{:?}`. Follow-up #7: `RejectedKey` has two divergent error shapes for the same forbidden key.
- `H-001` (closed) approved mode-scoped component add/remove/override in `snow.yml`; `0.1.0` only parsed `run_command` overrides on existing components. `ADR-0008`/`ADR-0001` §1 fix the single canonical spelling for removal.
- `crates/slipfork-snow` must stay free of ambient env/net/clock; the `0600` vault adapter is the only filesystem code.

# Design / Invariants

1. **Trait shape:** `pub trait SecretProvider: Send + Sync` and `pub trait EntropySource: Send + Sync` with `fn get(&self, ...)` and `fn ensure_secret(&self, key: &str, proposed: &str) -> Result<String, ProviderError>`. Interior synchronization lives only inside `FileVaultProvider` (`flock` on the vault file + `O_EXCL` temp + atomic `rename`, preserving `0600`).
2. **One duplicate-suppression contract:** `ensure_secret` on `FileVaultProvider` is a real compare-and-set — two concurrent racers return the **same** authoritative value and the file ends with exactly one write. `LayeredSecretProvider` keeps read `Local -> Remote` and write `Local` semantics (`H-003`).
3. **Redaction:** hand-written `Debug` impls for `ResourceCoordinates`, `S3Coordinates`, and `SecretValue` that print `SecretValue(<redacted>)`; `RejectedKey` collapses to a single `SnowError::ForbiddenTopLevelKey { key }` variant carrying only the **key name** (never the value), deleting the second error shape (M1.6 #7, #21).
4. **Mode-scoped components:** `SnowFile::effective_components(&self, mode) -> Result<ResolvedComponents, SnowError>` returning the ordered set of `(id, ResolvedComponent { run_command, disabled })` produced by `components[]` ⊕ `modes.<mode>.components.<id>` with: override of an existing component, addition of a mode-only component (must carry `run_command`), and removal via the single spelling `disabled: true`. `modes.all.components` stays rejected; unknown ids fail closed; `effective_run_command` is re-expressed in terms of `effective_components` so there is exactly one resolution path (no second reader).
5. `#![forbid(unsafe_code)]`, no ambient reads, `serde(deny_unknown_fields)` on every nested component struct so the "unknown fields" claim in `docs/reference/manifest.md` becomes true (M1.6 #16 follow-up evidence, docs themselves are out of scope here).

# Acceptance

```bash
# 1. Compile-time Send + Sync contract for every provider.
cargo test -p slipfork-snow --all-targets
cargo test -p slipfork-snow --test critical_snow

# 2. Redaction is total: no Debug path can print a secret value.
cargo test -p slipfork-snow --test critical_snow debug
! rg -n 'derive\(Debug\)' crates/slipfork-snow/src/model.rs | rg 'S3Coordinates|ResourceCoordinates|RejectedKey'

# 3. Mode-scoped add/remove/override resolves through one function.
rg -q 'fn effective_components' crates/slipfork-snow/src/model.rs
cargo clippy -p slipfork-snow --all-targets -- -D warnings
make check
```

All commands exit `0`. The critical suite must additionally assert the adversarial paths: concurrent `ensure_secret` race returns one value, `RejectedKey` for top-level `envs:` names only the key, and `disabled: true` on `modes.all.components` is still refused.
