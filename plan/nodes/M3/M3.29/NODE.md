---
id: M3.29
title: "Implement OSPP v1 (docs/design/ospp.md): live subscriptions, HPKE end-to-end secrets, and the zero-disk-secret lifecycle in 0.1.0"
kind: epic
status: ready
priority: 1
depends_on: []
paths: []
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-18T00:08"
updated: "2026-11-18T00:08"
---

# Goal

Ship the rewritten **OSPP v1** (`docs/design/ospp.md`, commit `9098b20`) as the only secret path in `0.1.0`: one all-JSON persistent `ospp-v1` WebSocket, `p256` auth and pairing with ALTCHA + proof-of-possession, uniform ID-only `claims`, **HPKE end-to-end encrypted** per-variable secrets and out-of-band encrypted resource dumps, live `env_updated` subscription while worlds run, and **zero secret storage on disk** — `FileVaultProvider`, `vaults/<project>.json` and the `slipfork secret` subcommand are deleted.

# Context (owner directive, 2026-11-18)

The owner reviewed the rewritten spec and said: *"well, let's see if this works when implemented but it already looks a lot better to me, ripple out the shit of this spec into everything else that needs to be updated and schedule all the required changes for current release"*. So this epic is **`0.1.0`**, not `0.2.0`, and it runs alongside `M3.28`.

`docs/design/ospp.md` is the **sole normative** OSPP v1 specification for `0.1.0`. It supersedes the wire in `ARCHITECTURE.md` §5–§7, `ADR-0001`, `ADR-0008` (`env.bundle`, `secrets.ensure`, `resources.describe`, `dump.begin`, `0600` disk vaults), and anything in `docs/design/target-dx.md` / `dx-0.1.0-ux.md` that offers `slipfork secret` or a disk vault.

What changes, in one paragraph each:

1. **Transport & framing.** Persistent WebSocket, `Sec-WebSocket-Protocol: ospp-v1`, every frame a UTF-8 JSON text message with `"type"`. Dumps are out-of-band HTTPS, never WebSocket binary frames.
2. **Handshake/auth.** `hello { version: 1, auth_methods: ["p256"] }` → `auth_challenge { method, challenge: { nonce } }` → `auth_response { response: { public_key, client_nonce, signature } }` → `auth_ok { encryption_methods: ["hpke-p256"] }`. Signature over `"ospp-v1" || server_nonce || client_nonce`. No `tpmd`, no `capabilities[]`.
3. **Pairing.** `pair_hello` → `pair_challenge { method, altcha, challenge }` → `pair_begin { altcha, hostname, response }` → `pair_handle { url, handle }` → push `pair_approved { account, machine }` / `error` (`rejected`, `already_paired`, `throttled`, `bad_proof`). Signature over `"ospp-v1-pair" || server_nonce || client_nonce`. Handle is 32-byte hex, fragment-only, socket-bound in memory, one pending pairing per key.
4. **Subscription.** `subscribe { id, remote, encryption: { method: "hpke-p256", public_key }, snow, claims }` → `env_resolved { id, components }`, server-pushed `env_updated { id, components }`, `unsubscribe { id }`. Project identity is **only** the verbatim Git `remote` string. `claims` is uniform and ID-only across `vars`, `secrets`, `resources.<kind>`, `domains`, `components`: a claimed entry is not resolved and its `${{ … }}` expression is left untouched; everything unclaimed is resolved by the server or refused.
5. **Secrets.** Plain values are strings; anything whose evaluation touched an unclaimed `secrets::` key or a sensitive resource credential is `{ "encrypted": "<base64>" }` (RFC 9180 HPKE Base, `DHKEM(P-256, HKDF-SHA256)`, `HKDF-SHA256`, `AES-256-GCM`, `info = "ospp-v1-secret"`, `aad = "<component>:<env_key>"`). Slipfork decrypts in memory, substitutes its locally-claimed values in memory, injects plaintext env into children, and **never writes a secret to disk**. Purely local runs generate ephemeral in-memory session secrets.
6. **Live updates.** The WebSocket stays open while worlds run; on `env_updated` the affected services restart with the new environment. Editing `snow.yml`/claims re-sends `subscribe` with the same `id`; stopping the world sends `unsubscribe`.
7. **Dumps.** `dump_request { id, subscription_id, resource }` → `dump_ready { id, url }`, then out-of-band `HTTPS GET` encrypted with stateful HPKE `SetupBaseS(public_key, info = "ospp-v1-dump")`: 65-byte uncompressed `enc`, then `4-byte BE length + ctx.Seal(aad = "<subscription_id>:<resource>", chunk)` frames, final `len = 0` EOF. Decrypted on the fly into `zstd -d | pg_restore`; zero plaintext dump on disk.

# Acceptance

At the gate:

```console
$ uv run --no-project tools/plan.py validate
plan OK
$ make check
... exit 0
$ make docs-check
... exit 0
$ ! grep -rn 'FileVaultProvider\|slipfork secret\|vaults/' crates/ docs/ README.md && echo no-disk-vaults
no-disk-vaults
$ ! grep -rn 'env\.bundle\|secrets\.ensure\|resources\.describe\|dump\.begin\|tpmd' crates/slipfork-ospp crates/slipfork-ospp-testkit docs/reference/ospp.md && echo ospp-v1-only
ospp-v1-only
```

- Every child is `done`; the epic is `done` only with `M3.28.13`'s live evidence plus a zero-secret-on-disk proof.
- `docs/design/ospp.md` is never edited by this epic — it is the input. If implementation and spec disagree, the spec wins or a `direction-challenge` is filed.

# Node map

- `M3.29.1` architect: ripple the spec through `ARCHITECTURE.md`, `ADR-0001`, `ADR-0008`, `docs/VISION.md`, `dx-0.1.0-ux.md`, `target-dx.md`; delete `slipfork secret` + `0600` disk vaults from every command table/schema.
- `M3.29.2` implementer: clean-sheet `slipfork-ospp` + `slipfork-ospp-testkit` + `critical_ospp.rs` (`hpke`/`p256`).
- `M3.29.3` implementer: delete the disk vault (`FileVaultProvider`, `vaults.rs`, `slipfork secret`), in-memory session secrets, live `slipforkd` subscriptions, streaming HPKE dump restore, `auth` pairing; update `critical_ospp_daemon`/`critical_cli`.
- `M3.29.4` doc-writer: publish `docs/reference/ospp.md` and purge the old secret/vault claims from every published page.
