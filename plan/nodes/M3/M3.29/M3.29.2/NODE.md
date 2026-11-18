---
id: M3.29.2
title: "Implement OSPP v1 wire, p256 pairing, ID-only claims, hpke-p256 per-variable encryption and encrypted dump streaming"
kind: task
status: active
owner: implementer
priority: 1
depends_on: [M3.29.1]
paths: ["crates/slipfork-ospp/**", "crates/slipfork-ospp-testkit/**", Cargo.toml, Cargo.lock]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-18T00:08"
updated: "2026-11-18T01:26"
---

# Goal

Clean-sheet rewrite of `crates/slipfork-ospp` + `crates/slipfork-ospp-testkit` (and the reference server they ship) so the wire is exactly `docs/design/ospp.md` in `0.1.0`: all-JSON `ospp-v1` frames, `p256` auth + ALTCHA/PoP pairing, uniform ID-only `claims`, `hpke-p256` per-variable encryption, live `env_updated`, and out-of-band encrypted dumps. Every old shape is deleted, not kept.

# Context

`docs/design/ospp.md` (commit `9098b20`) is the sole normative spec; `M3.29.1` aligned `ARCHITECTURE.md`/ADRs to it. Today `crates/slipfork-ospp/src/**` implements the superseded shape (`env.bundle`, `secrets.ensure`, `resources.describe`, `dump.begin`, `tpmd` capability negotiation, `Rc<[u8]>` bundles) and `slipfork-ospp-testkit` fabricates the old frames. Nothing is pushed; zero compatibility is owed.

# Design / Invariants

1. **Frames.** Exactly the types and fields in the spec, one Rust type per frame, `#[serde(tag = "type", rename_all = "snake_case")]`, unknown frames fail closed (`OsppError::Protocol`), no `#[serde(flatten)]` catch-all, no `extra: Map`.
2. **Handshake/auth.** `hello` → `auth_challenge` → `auth_response` → `auth_ok`. ECDSA P-256/SHA-256 over `b"ospp-v1" || server_nonce || client_nonce`, raw `r || s`, compressed public key; a bad signature or an unsupported `auth_methods`/`encryption_methods` entry fails closed and closes the socket.
3. **Pairing.** `pair_hello`/`pair_challenge`/`pair_begin`/`pair_handle` + push `pair_approved`/`error`. ALTCHA solution is carried opaque to us; `hostname` is sent; the signature covers `b"ospp-v1-pair" || server_nonce || client_nonce`. The client **appends** `#handle=<handle>` to the server's `url` — it never computes the URL itself. One pending pairing per key, socket-bound; a disconnect cancels.
4. **Subscription.** `subscribe { id, remote, encryption, snow, claims }`, `env_resolved`, `env_updated`, `unsubscribe`. `remote` is the verbatim Git remote string. `claims` is ID-only and uniform over `vars`, `secrets`, `resources.<kind>`, `domains`, `components` — serialize exactly the claimed ids, never values.
5. **HPKE.** `hpke-p256` via RFC 9180 Base mode, `DHKEM(P-256, HKDF-SHA256)`, `HKDF-SHA256`, `AES-256-GCM`, `info = "ospp-v1-secret"`, `aad = "<component>:<env_key>"`, `enc || ciphertext` in standard base64. Per-variable seal + open both directions; a wrong `aad`/component/key fails to open.
6. **Dumps.** `dump_request`/`dump_ready`; out-of-band HTTPS `GET`; stateful `SetupBaseS(public_key, info = "ospp-v1-dump")`; 65-byte uncompressed `enc` header; frames `4-byte BE len || ctx.Seal(aad = "<subscription_id>:<resource>", chunk)`; final `len = 0`; decoder fails closed on a truncated/missing EOF frame. No binary WebSocket frames anywhere.
7. **Reference server (testkit).** Implements the whole spec against `snow.yml` + ID-only claims: single-pass evaluation that leaves claimed `${{ … }}` untouched, secrets contamination → `{ "encrypted": … }`, live `env_updated` push, an HTTPS dump endpoint serving the sealed stream, ALTCHA + PoP verification, and typed `error` frames with the spec's codes.
8. **One capability trait** for the client side (connect/subscribe/dump/close) so the daemon can inject a fake; the encryption keypair is per-subscription and lives in memory only (no fs, no env).
9. Delete every old frame, type, and helper in the same commit (`env.bundle`, `secrets.ensure`, `resources.describe`, `dump.begin`, `tpmd` negotiation, `capabilities[]`). No aliases, no `#[deprecated]`, no "legacy" module.
10. No new global state, no `unsafe`, no `std::env`/fs reads outside tests; `hpke`/`p256` are the expected new dependencies (already declared in `Cargo.toml` if `M3.29.1` landed them there).

# Acceptance

```console
$ cargo test -p slipfork-ospp --lib
... test result: ok
$ cargo test -p slipfork-ospp --test critical_ospp
... test result: ok
$ cargo test -p slipfork-ospp-testkit
... test result: ok
$ ! grep -rn 'env\.bundle\|secrets\.ensure\|resources\.describe\|dump\.begin\|tpmd\|capabilities' crates/slipfork-ospp/src crates/slipfork-ospp-testkit/src && echo ospp-v1-only
ospp-v1-only
$ ! grep -rn 'serde(flatten)\|extra:' crates/slipfork-ospp/src && echo no-catch-all
no-catch-all
$ cargo clippy --workspace --all-targets -- -D warnings
    Finished
```

- `critical_ospp` must prove, against the reference server: full handshake + `auth_ok`; pairing to `pair_approved`; `subscribe`/`env_resolved` with a claimed `${{ … }}` surviving verbatim and a secret returned `{ "encrypted": … }` that opens only with the right `aad`; a pushed `env_updated`; `unsubscribe`; and a dump stream that fails closed on a truncated tail.
- Round-trip pinning: a fixed key pair + fixed plaintext produce a value that the client opens and a **wrong** `aad` rejects.
