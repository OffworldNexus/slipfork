---
id: M2.13
title: "slipfork-ospp: DeviceKeySigner plus OSPP v1 handshake and fragment pairing client"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M2.4, M2.5]
paths: ["crates/slipfork-ospp/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-15T16:40"
updated: "2026-11-15T16:40"
---

# Goal

Implement the `DeviceKeySigner` port with its `tpmd` and software P-256 adapters, the `OSPP v1` server-speaks-first challenge-response handshake, and the browser-fragment pairing client, proven against an in-tree mock server suite.

# Context

- `docs/research/tunnel-weaver-and-systemd-facts.md` is the primary source: device keys are ECDSA P-256 with SHA-256; a `KeyId` is the SEC1 compressed 33-byte public key rendered `p256:<66 lowercase hex>`; signatures are raw fixed-width `r||s` (64 bytes, never DER); the transcript is `"ospp-v1" || nonce_s(32) || nonce_c(32) || server_name`; pairing handles are 32 random bytes as 64 lowercase hex characters delivered only in the URL fragment; `tpmd` client candidates are `$XDG_RUNTIME_DIR/tpmd/tpmd.sock` then `/run/tpmd/tpmd.sock`.
- `ADR-0008` (from `M2.1`) is normative for the wire; `M2.4` made `SecretProvider` `Send + Sync` with `&self`; `M2.5` supplies the `[resources]` parsing this crate consumes in `M2.14`.
- This node touches only `crates/slipfork-ospp/**`. Offline laptops must remain fully functional, so the client never becomes a hard dependency of world creation.

# Design / Invariants

1. **Signer port.** `DeviceKeySigner { fn key_id(&self) -> KeyId; fn sign(&self, message: &[u8]) -> Result<DeviceSignature, OsppError> }`. `KeyId` and `DeviceSignature` are validated newtypes (`p256:` + 66 lowercase hex; exactly 64 bytes). No `Debug` impl prints private key material.
2. **Adapters and selection.** `TpmdDeviceKeySigner` probes the two socket candidates in order over an injected `KeyStorePaths` value (no `std::env::var` in this crate), using a line-framed request and refusing any response that does not verify against the returned `key_id`. `SoftwareP256Signer` reads the `0600` key file from the vault. A single factory performs the fallback and emits exactly one diagnostic line naming the reason; the fallback is never silent and never fabricates a signature.
3. **Handshake.** Subprotocol `ospp-v1`; server-first `CHALLENGE { nonce_s }`; client `HELLO { version: u16 = 1, key_id, nonce_c, sig }` over exactly the transcript bytes above, with `server_name` being the dialed hostname string identical on both sides; server `WELCOME { params }` or `REJECT { UnknownKey | BadSignature | UnsupportedVersion { min, max } }` mapped onto typed `OsppError` variants. Frames are capped at 16 KiB and the first server frame has a bounded deadline.
4. **Replay and downgrade resistance.** `nonce_s` is single-use per connection, `nonce_c` must be 32 bytes of CSPRNG output from the injected entropy source, and a `REJECT` never retries with a weaker signature encoding. TLS exporter channel binding is documented as reserved, not silently assumed.
5. **Pairing.** The URL fragment `#handle=<64 lowercase hex>` is the only delivery channel: the client parses it, sends it once, never logs it, and zeroizes it after use. A malformed handle or a handle-bearing URL handed to `--json` output is redacted. A pairing attempt without a fragment fails closed naming the missing handle.
6. **Tests.** `tests/support/mock_server.rs` provides a deterministic clock, a seeded CSPRNG double, and an in-process WebSocket server; `crates/slipfork-ospp/tests/critical_ospp.rs` covers accept, bad signature, unknown key, version range rejection, stale nonce replay, fragment pairing, and the two-socket probe order.

# Acceptance

```bash
# 1. Signer contract and handshake matrix.
cargo test -p slipfork-ospp --all-targets
cargo test -p slipfork-ospp --test critical_ospp -- --nocapture handshake
cargo test -p slipfork-ospp --test critical_ospp -- --nocapture signer_probe
cargo test -p slipfork-ospp --test critical_ospp -- --nocapture pairing

# 2. Rejection paths are typed and fail closed.
cargo test -p slipfork-ospp --test critical_ospp -- --nocapture reject_paths

# 3. Purity and lint gates.
! rg -q "std::env" crates/slipfork-ospp/src
! rg -q "der::" crates/slipfork-ospp/src
cargo clippy -p slipfork-ospp --all-targets -- -D warnings
make check
```

All commands exit `0`. Adversarial assertions required: a replayed `nonce_s` is refused, a DER-encoded signature is refused, `Debug` of any signer or key type contains no key bytes, and the pairing handle never appears in a log line or a `--json` rendering.
