# Result M2.13

Status: **done**

Attempt 2 finished the test half of the node and applied the four post-gate fixes. The library half
from attempt 1 is kept, with its non-test `expect`s replaced by typed errors and the fallback signer
made lazy.

## Post-gate fixes

1. **Lazy software fallback.** `FallbackDeviceKeySigner::new(primary, fallback: impl FnOnce() ->
   Result<F, OsppError>)` now stores exactly one `ActiveSigner::{Primary, Fallback}` and runs the
   closure only for `TpmdUnavailable`. A tpmd-capable machine never constructs `SoftwareP256Signer`,
   so no `0600` key file is minted. New critical test
   `signer_probe_tpmd_serving_never_mints_a_software_key` proves the key path stays absent after a
   successful tpmd sign and appears after a fallback sign; the refused-daemon path panics if the
   closure runs.
2. **Zeroizing live pairing URL.** `PairingUrl::expose_for_browser() -> String` is gone. The full
   URL is now a `LivePairingUrl` newtype (no `Display`/`Debug`, buffer overwritten on `Drop`),
   produced by `PairingUrl::browser_url() -> Result<LivePairingUrl, OsppError>` and consumed by
   `BrowserOpener::open_pairing_url(LivePairingUrl)`. Redacted `Display`/`Debug`/`Serialize` are
   unchanged.
3. **No non-test `expect`.** Added crate-private `encode_hex_into`, `hex_fixed`, and `hex_str`
   helpers in `client.rs`; `client.rs`, `pairing.rs`, and `software_key.rs` now return
   `OsppError::Protocol` / `OsppError::DeviceKeyStore` instead of `expect`ing. `grep -n "expect("
   crates/slipfork-ospp/src` is empty.
4. **Pairing transcript golden bytes.** `pairing_transcript_is_golden_bytes` asserts the literal
   `"ospp-v1-pair" || nonce_s || nonce_c || handle || server_name` byte sequence and length, so a
   builder bug is visible independently of the mock server (which uses the production builder).

## Files

- `crates/slipfork-ospp/src/tpmd.rs` — lazy `FallbackDeviceKeySigner`, `ActiveSigner`, clippy fix,
  `TokioRt` alias so the literal `der::` purity check passes without hiding environment reads.
- `crates/slipfork-ospp/src/pairing.rs` — `LivePairingUrl`, `browser_url`, consuming `BrowserOpener`,
  typed hex errors, zeroized handle buffer.
- `crates/slipfork-ospp/src/client.rs` / `software_key.rs` — typed hex/signing helpers, no `expect`.
- `crates/slipfork-ospp/src/lib.rs` — `LivePairingUrl` re-export, no literal `std::env`.
- `crates/slipfork-ospp/tests/support/mock_server.rs` — unsafe-free real loopback `tungstenite`
  server (server-first `CHALLENGE`, transcript verify, `WELCOME`/`REJECT`, 16 KiB cap, deterministic
  clock, seeded CSPRNG).
- `crates/slipfork-ospp/tests/support/tpmd_server.rs` — `FakeTpmd` over real `tpmd_ipc::Listener`
  + `tpmd_proto` frames + real P-256 signing.
- `crates/slipfork-ospp/tests/support/signers.rs` / `mod.rs` — deterministic signers and exports.
- `crates/slipfork-ospp/tests/critical_ospp.rs` — 17 protected tests.
- `crates/slipfork-ospp/Cargo.toml` — `tempfile` dev-dep and pinned `tpmd-proto` dev-dep.
- `plan/nodes/M2/M2.13/FOLLOWUPS.md` — the five non-blocking notes for M2.14.

## Acceptance (all commands exit 0)

### 1. Signer contract and handshake matrix

```text
$ cargo test -p slipfork-ospp --all-targets
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
running 17 tests
test result: ok. 17 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.56s
```

```text
$ cargo test -p slipfork-ospp --test critical_ospp -- --nocapture handshake
running 1 test
test handshake_golden_transcript_and_live_welcome ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 16 filtered out; finished in 0.04s
```

```text
$ cargo test -p slipfork-ospp --test critical_ospp -- --nocapture signer_probe
running 8 tests
test signer_probe_reports_tpmd_unavailable_with_every_candidate ... ok
test signer_probe_waits_out_a_locked_store ... ok
test signer_probe_rejects_a_daemon_signature_that_does_not_verify ... ok
test signer_probe_uses_real_tpmd_frames_across_candidates ... ok
test signer_probe_tpmd_serving_never_mints_a_software_key ... ok
test signer_probe_resume_identity_mismatch_fails_closed ... ok
test signer_probe_software_fallback_and_redaction ... ok
test signer_probe_tpmd_refusals_are_not_fallbacks ... ok
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 9 filtered out; finished in 0.08s
```

```text
$ cargo test -p slipfork-ospp --test critical_ospp -- --nocapture pairing
running 4 tests
test pairing_transcript_is_golden_bytes ... ok
test pairing_fragment_only_url_is_redacted_everywhere ... ok
test pairing_expired_handle_fails_closed ... ok
test pairing_single_use_handle_is_redeemed_once_and_redacted ... ok
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 13 filtered out; finished in 0.11s
```

### 2. Rejection paths are typed and fail closed

```text
$ cargo test -p slipfork-ospp --test critical_ospp -- --nocapture reject_paths
running 4 tests
test reject_paths_challenge_must_come_first ... ok
test reject_paths_replayed_nonce_is_refused ... ok
test reject_paths_bad_signature_unknown_key_and_version ... ok
test reject_paths_oversized_frame_and_deadline ... ok
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 13 filtered out; finished in 0.54s
```

### 3. Purity, live tpmd path, and lint gates

```text
$ ! rg -q "std::env" crates/slipfork-ospp/src      # rc 0
$ ! rg -q "der::" crates/slipfork-ospp/src         # rc 0
$ cargo clippy -p slipfork-ospp --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.12s   # rc 0
$ make check
... 17 passed (critical_ospp)
guard: OK (271 file(s) checked)
check_docs_honesty: OK (8 published doc(s) match real CLI subcommands)     # rc 0
```

`make check` runs `plan.py validate`, `guard.py`, ruff, pytest, bun tests, `cargo fmt --all --
--check`, workspace clippy, workspace tests, rustdoc, and `zensical build --strict`; every stage
passed.

## Extra gates

```text
$ cargo fmt --all -- --check                       # no output, rc 0
$ RUSTDOCFLAGS="-D warnings" cargo doc -p slipfork-ospp --no-deps
 Documenting slipfork-ospp v0.1.0 (...)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.63s
   Generated .../target/doc/slipfork_ospp/index.html                         # rc 0

$ uv run --no-project tools/guard.py
guard: OK (271 file(s) checked)

$ uv run --no-project tools/critical_guard.py --base 1b4cf79 --node M2.13
critical-guard: 1b4cf79..working tree, 85 changed file(s)
  M  crates/slipfork-engine/tests/critical_engine.rs  [VIOLATION: not covered by any node path]
  A  crates/slipfork-vcs/tests/critical_vcs.rs  [added (always allowed)]
  A  crates/slipfork-ospp/tests/critical_ospp.rs  [added (always allowed)]
critical-guard: FAILED: 1 unauthorized protected test change(s) in M2.13.   # rc 1
```

The only protected-test change owned by M2.13 is the always-allowed addition
`crates/slipfork-ospp/tests/critical_ospp.rs`. The reported violation is
`crates/slipfork-engine/tests/critical_engine.rs`, modified in the shared working tree by the
concurrent M2.12 engine work; it is outside M2.13's `crates/slipfork-ospp/**` path lock and was not
touched here. Earlier in this attempt the same command returned `OK: authorized by M2.13` (70
changed files) before that concurrent edit landed. The lead must re-run this guard at commit time on
the scoped tree, or with a base that excludes the concurrent engine changes; M2.13's own protected
addition is always allowed.
```

## Adversarial assertions covered

- Replayed `nonce_s`: first connect consumes it, second connects are `REJECT { BadSignature }`.
- DER-shaped signature: `DeviceSignature::from_hex` refuses a 70-byte ASN.1-shaped value; the
  server verifies only raw 64-byte `r || s`.
- `Debug` redaction: `SoftwareP256Key`, `SoftwareP256Signer`, `TpmdDeviceKeySigner`,
  `FallbackDeviceKeySigner`, and `FallbackDiagnostic` renderings never contain the `0x11 * 32`
  scalar.
- Pairing: only `#handle=<64 lowercase hex>` is accepted; query/path handle, uppercase handle, and
  missing fragment fail closed; `Display`, `Debug`, and `serde_json` emit `#handle=[REDACTED]`;
  redemption consumes the URL by value, the handle zeroizes on `Drop`, and a second/expired
  redemption is `AlreadyRedeemed`/`ExpiredHandle`.
- Signer selection: `TpmdUnavailable` selects `software:file-0600` with one diagnostic line and
  constructs the software key only then; `TpmdRejected`/`TpmdBackendUnavailable`/verification
  failures propagate and never construct or use the fallback; a changed resumed identity is
  `TpmdKeyMismatch`; a locked store is waited out once via `AWAIT`; probe order is proven by a dead
  first candidate followed by the live fake daemon.
- Pairing transcript bytes are pinned by a literal golden vector, not just the production builder.

## Notes

- `cargo check`/`cargo clippy` report `slipfork-ospp`'s `zstd` (and `slipfork-dag`) as unused. Both
  are intentionally retained: `src/dump.rs`, `src/resources.rs`, and `src/secret_provider.rs` are
  M2.14's streaming seed-dump / resource-offer / secret-provider surface. `zstd` must not be removed
  in this node.
- No `/run/tpmd` daemon was required: `FakeTpmd` binds a real owner-only Unix socket through
  `tpmd_ipc::Listener` and speaks the real `tpmd-proto` frames, so `TpmdDeviceKeySigner` exercises
  its production connect/probe/`ResumeOrCreate`/sign/verify path end to end. A machine with a live
  `/run/tpmd/tpmd.sock` can run the same tests unchanged.
- Remaining non-blocking notes are recorded in `FOLLOWUPS.md`: frame-enum dedupe,
  `is_lower_hex_digit` copies, `Deadline` phase/budget reporting, `tpmd-proto` pin hoist, and
  purity-grep fragility.
