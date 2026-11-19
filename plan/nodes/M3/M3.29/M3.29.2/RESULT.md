# M3.29.2 — OSPP v1 clean-sheet wire, p256 auth/pairing, ID-only claims, HPKE secrets and sealed dumps

Base: `b8b246b` · Branch: `release/0.1.0` · No commit made.

## What landed

`crates/slipfork-ospp` and `crates/slipfork-ospp-testkit` were rewritten from scratch
against `docs/design/ospp.md` (normative). Nothing old was adapted or aliased.

### Deleted shapes (all gone, no aliases, no deprecated shims)

- Wire: `env.bundle`, `secrets.ensure`, `resources.describe`, `dump.begin`, the
  `capabilities[]` negotiation, binary WebSocket dump frames, `Rc<[u8]>` bundles.
- Modules/types: `protocol.rs` (capability frames), `resources.rs`
  (`RemoteResourceBundle`, `PgDumpMetadata`, `S3BucketCoordinates`), `secret_provider.rs`
  (`OsppSecretProvider`, `LayeredSecretProvider` integration), `disk_dump.rs`
  (`DiskSeedDumpStore`), `tpmd.rs` (`TpmdDeviceKeySigner`, `FallbackDeviceKeySigner`,
  `KeyStorePaths`, capability negotiation), `software_key.rs` merged into `device_key.rs`,
  `hex.rs` (inlined), `dump.rs` old `DumpStreamer`/`SeedDumpStore`/`VerifiedDump`.
- Testkit: `tpmd_server.rs` (`FakeTpmd`), `memory_store.rs` (`MemorySeedDumpStore`),
  `DumpRecord`/`HandleRecord`/`S3BucketRecord`/`SeededEntropy` old shapes (signers split out).
- Dependencies: `tpmd-client`, `tpmd-ipc`, `tpmd-proto` (workspace + crates), `tokio`
  (workspace and crate), `zstd`, `slipfork-dag` from the client, `slipfork-world` retained
  only for `WorldOrigin` in `BrowserOpener`.

### New crate surface (`crates/slipfork-ospp`)

- `wire.rs` — closed frame set, `#[serde(tag = "type", rename_all = "snake_case")]`, one
  type per frame. Parse validates an exact per-frame field allow-list, so unknown frame
  types **and** unknown/extra fields fail closed (`OsppError::Protocol`); no
  `#[serde(flatten)]`, no `extra` map. Frames:
  - client → server: `hello`, `auth_response`, `pair_hello`, `pair_begin`, `subscribe`,
    `unsubscribe`, `dump_request`
  - server → client: `auth_challenge`, `auth_ok`, `pair_challenge`, `pair_handle`,
    `pair_approved`, `env_resolved`, `env_updated`, `dump_ready`, `error`
  - transcripts: `b"ospp-v1" || server_nonce || client_nonce` (auth),
    `b"ospp-v1-pair" || server_nonce || client_nonce` (pairing); raw `r || s`, compressed
    SEC1 P-256 public keys.
- `device_key.rs` — `DeviceKeySigner`, `DeviceKeyStore`, `DeviceKeyPath`,
  `SoftwareP256Key`, `SoftwareP256Signer`, `DeviceSignature`, `FileDeviceKeyStore`
  (`0600`, injected path), `verify_device_signature`.
- `keys.rs` — `SubscriptionKey`: per-subscription `hpke-p256` keypair, in memory only
  (`hpke` zeroizes the private scalar on drop). Secret open: Base mode,
  `DHKEM(P-256, HKDF-SHA256)` + `HKDF-SHA256` + `AES-256-GCM`,
  `info = "ospp-v1-secret"`, `aad = "<component>:<env_key>"`, standard base64 of
  `enc || ciphertext`.
- `dump.rs` — `DumpReader` (client opens with `SetupBaseR` **only**):
  `info = "ospp-v1-dump"`, 65-byte uncompressed `enc` header, then
  `4-byte big-endian plaintext length || ctx.Open(aad = "<subscription_id>:<resource>", chunk)`
  frames, and a mandatory final `len = 0` sealed chunk. EOF before that frame is
  `UnexpectedEof` / `OsppError::TruncatedDump`; a wrong aad fails the frame open.
- `model.rs` — `OsppClaims` (ID-only over `vars`, `secrets`, `resources.<kind>`,
  `domains`, `components`), `SubscribeRequest` (verbatim Git `remote`), and
  `SubscriptionEnv` with value-redacting `Debug`.
- `pairing.rs` — `LivePairingUrl::with_handle` appends `#handle=<handle>` to the
  server-supplied URL only (handle zeroized on `Drop`, redacted `Debug`), ALTCHA
  SHA-256 PoW solver, `BrowserOpener` port.
- `endpoint.rs` — `OsppEndpoint` (`wss://` anywhere, `ws://` loopback only).
- `client.rs` — `OsppClient` (`endpoint`, `pair`, `connect`) and `OsppSession`
  (`subscribe`, `dump`, `unsubscribe`, `close`); `WsOsppClient` over synchronous
  `tungstenite` + `rustls` + `webpki-roots`, one session thread, live `env_updated`
  dispatch, out-of-band HTTPS dump fetch (`rustls`/`webpki-roots`; loopback `http`
  only), no `tokio`, no `std::env`, no `unsafe`, no global mutable state.

### Reference server (`crates/slipfork-ospp-testkit`)

Real loopback WebSocket (`Sec-WebSocket-Protocol: ospp-v1`), P-256 signature and ALTCHA
verification, socket-bound pairing with browser-push verdicts, single-pass `snow.yml`
evaluation (claimed `${{ … }}` copied **byte-verbatim**, claimed components omitted),
secret/resource contamination → `{ "encrypted": … }`, live `env_updated`, out-of-band
dump endpoint serving the sealed stream, typed `error` codes (`unsupported_version`,
`unsupported_auth`, `unsupported_encryption`, `unauthorized_key`, `bad_proof`,
`rejected`, `throttled`, `unresolved`, `dump_unavailable`, `bad_frame`).

### Dependency changes

- `Cargo.toml`: added `hpke = { version = "0.15.1", default-features = false,
  features = ["alloc", "nistp", "aes", "hkdfsha2"] }`; removed `tokio`, `zstd`,
  `tpmd-client`, `tpmd-ipc`, `tpmd-proto`.
- `slipfork-ospp`: `base64`, `hex`, `hpke`, `p256`, `rustls`, `serde`, `serde_json`,
  `sha2`, `slipfork-snow`, `slipfork-world`, `thiserror`, `tungstenite`,
  `webpki-roots`; dev-dep `slipfork-ospp-testkit`.
- `slipfork-ospp-testkit`: `base64`, `hex`, `hpke`, `p256`, `serde`, `serde_json`,
  `sha2`, `slipfork-ospp`, `slipfork-snow`, `tungstenite`.
- `Cargo.lock` regenerated (drops the three git `tpmd` packages and `tokio`/`zstd`
  edges from these crates).

## Acceptance (exact)

```console
$ cargo test -p slipfork-ospp --lib
running 15 tests
test result: ok. 15 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s

$ cargo test -p slipfork-ospp --test critical_ospp
running 8 tests
test bad_signature_fails_closed_with_bad_proof ... ok
test pairing_rejection_is_a_typed_error_frame ... ok
test hpke_round_trip_pinning_rejects_a_wrong_aad ... ok
test pairing_appends_the_server_url_fragment_and_reaches_pair_approved ... ok
test handshake_and_subscribe_resolve_id_only_claims ... ok
test unsubscribe_releases_the_subscription ... ok
test pushed_env_updated_is_decrypted_in_memory_and_delivered ... ok
test dump_stream_restores_and_a_truncated_tail_fails_closed ... ok
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.61s

$ cargo test -p slipfork-ospp-testkit
running 2 tests
test reference_server::tests::claimed_expressions_survive_byte_verbatim ... ok
test reference_server::tests::unclaimed_secrets_contaminate_and_claim_nothing ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

$ ! grep -rn 'env\.bundle\|secrets\.ensure\|resources\.describe\|dump\.begin\|tpmd\|capabilities' crates/slipfork-ospp/src crates/slipfork-ospp-testkit/src && echo ospp-v1-only
ospp-v1-only

$ ! grep -rn 'serde(flatten)\|extra:' crates/slipfork-ospp/src && echo no-catch-all
no-catch-all

$ cargo clippy -p slipfork-ospp -p slipfork-ospp-testkit --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.88s

$ cargo fmt -p slipfork-ospp -p slipfork-ospp-testkit -- --check
fmt-clean

$ uv run --no-project tools/guard.py
guard: OK (595 file(s) checked)

$ uv run --no-project tools/plan.py validate
plan OK
```

### `critical_guard` (external violation, not this node)

```console
$ uv run --no-project tools/critical_guard.py --base b8b246b --node M3.29.2
critical-guard: b8b246b..working tree, 61 changed file(s)
  M  crates/slipfork-ospp/tests/critical_ospp.rs  [authorized by crates/slipfork-ospp/**]
  M  crates/slipfork/tests/critical_cli.rs  [VIOLATION: not covered by any node path]
critical-guard: FAILED: 1 unauthorized protected test change(s) in M3.29.2.
```

`crates/slipfork/tests/critical_cli.rs` is modified by the concurrently running
`M3.28.8` in `crates/slipfork/src|tests`, which this node must not touch. This node's own
protected suite is authorized by its `paths`.

## Known deviations / caveats

1. **Dump transport in the test double.** The reference server serves the sealed stream
   over loopback `http://` (same loopback rule as `ws://`), because a browser-trusted TLS
   certificate cannot be minted offline. The production client's `https://` path is real
   (`rustls` + `webpki-roots`). The sealed-stream **format** is identical in both cases and
   is what the suite proves: `dump_stream_restores_and_a_truncated_tail_fails_closed`
   restores a 150 000-byte payload, rejects a stream with the `len = 0` EOF frame removed,
   and rejects a stream opened with the wrong `<subscription_id>:<resource>` aad.
2. **Workspace-wide clippy/fmt/`make check` are not green, by design.** `crates/slipfork`
   still consumes the deleted OSPP API and its rewrite is `M3.29.3`, outside this node's
   path scope (and `M3.28.8` is concurrently editing the same crate, also leaving it
   unformatted). Acceptance for this node is therefore scoped to
   `-p slipfork-ospp -p slipfork-ospp-testkit`, as instructed. `cargo fmt --all -- --check`
   fails only on pre-existing/concurrent `crates/slipfork/src/restart.rs` (and similar)
   diffs; the scoped check above is clean.
3. **ALTCHA.** The reference server's challenge payload is a standard ALTCHA SHA-256
   subset (`algorithm`/`challenge`/`salt`/`maxnumber`); the client preserves all server
   fields and adds `number` in the opaque base64 solution, so a server-supplied signature
   would pass through untouched. The server verifies the recomputed hash.

No commit was made.
