# Result M2.14

Status: **done**

The authenticated `ospp-v1` channel now serves the full capability set: `env.bundle`,
`secrets.ensure` (a real compare-and-set `SecretProvider`), `resources.describe` (converted only by
`OsppClient::resource_offer`), and the streaming `dump.begin -> binary -> dump.end` seed dump into a
non-tmpfs digest-pinned cache. An in-tree reference server (`tests/support/reference_server.rs`)
implements the same frames independently, so the protected suite proves the production client
against a second implementation rather than against itself.

## What landed

| File | Change |
|---|---|
| `src/protocol.rs` (new) | One normative capability table; `CapabilityRequest`, single `ServerFrame` enum, shared `WireReject` body with connect/pairing conversions, hand-written redacting `Debug` for every secret-bearing frame, mode/key validators. |
| `src/hex.rs` (new) | One crate-private lowercase-hex module (`is_lower_hex_digit`, `is_sha256_hex`, `encode_hex_into`, `hex_fixed`, `hex_str`); deleted the three `is_lower_hex_digit` copies and the `client.rs` helper copies. |
| `src/client.rs` | `Mutex<OsppConnection>` session (instance state, zero globals) so `&self` capabilities share one authenticated WebSocket; `env_bundle`, `ensure_secret`, `resource_offer`; 16 KiB text / 64 KiB binary caps; `deadline` now reports the budget actually armed (handshake vs io); session poisoning after an abandoned dump; `ServerFrame` replaces `ConnectFrame`/`PairingFrame`. |
| `src/resources.rs` | `RemoteResourceBundle`, `S3BucketCoordinates` (redacted `Debug`), `PgDumpMetadata`, typed `PgDumpFormat`; field/digest/byte-count validation and the single crate-private `into_offer` consumed only by `resource_offer`. |
| `src/dump.rs` | Capability port `SeedDumpStore` (`lookup`, `write_from -> VerifiedDump`, `discard`), `DumpStream` completion token, and `DumpStreamer` with a 64 KiB-capped frame reader, incremental SHA-256/byte checks, and session poisoning. **Zero `std::fs`/`zstd`-on-disk code**: publishing, verifying, opening, and discarding belong to the store. |
| `src/disk_dump.rs` (new) | `DiskSeedDumpStore` adapter: `<cache_root>/dumps/<sha>.part` -> verified atomic `<sha>.pgdump`, tmpfs refused via `/proc/self/mountinfo`, refused publication unless `dump.end` was observed, digest/byte-count/bounded-window (`MAX_DUMP_WINDOW_LOG = 23`) zstd validation, and a `VerifiedDump` with `path()`/`open()`/`verify_zstd()`; any failure removes the partial artifact. |
| `src/secret_provider.rs` | `OsppSecretProvider` (`Send + Sync`, `&self`, zero fs writes) scoped to `<project>:<mode>`, mapping capability failures to `ProviderError::Io` whose locus names capability and world scope. |
| `src/error.rs` | `Capability`, `CapabilityRejected`, `SeedDump`, `SeedDumpStore` (all value-free); `Deadline` documents the armed budget. |
| `src/lib.rs` | Public surface: capabilities, `EnvBundle`, resource wire types, `OsppSecretProvider`, dump port/streamer/store, `MAX_DUMP_FRAME`. |
| `tests/support/reference_server.rs` (new) | Handshake + pairing + every capability, independent literal transcript builders and direct `p256` `verify_prehash` (never the production helpers), real CAS secret map, real `zstd` archives in bounded frames, rejection knobs. Replaces `mock_server.rs` (deleted; no second harness). |
| `tests/support/memory_store.rs` (new) | Filesystem-free `SeedDumpStore` double with a real zstd `VerifiedDump`, recording publishes and `discard` calls. |
| `tests/critical_ospp.rs` | 32 protected tests (was 17): vars/secrets/domains bundle + redaction, `!!` CAS, concurrent `ensure`, resource offer -> `ResourcePlan::resolve`, hermetic `test` defaults, required-remote fail-closed, dump happy/truncation/digest/oversized/format/tmpfs/cache tests, a filesystem-free store, truncation `discard`, offline vault, capability denial, pre-`WELCOME` capability. |

## Quad-gate fixes

- **`SeedDumpStore` owns its filesystem (review-ioc).** `write_from` now returns a
  `VerifiedDump` (`path()`, `compressed_bytes()`, `sha256_compressed()`, `open()`, `verify_zstd()`)
  and the port has `discard(sha)`; `src/dump.rs` contains no `std::fs`/on-disk zstd. All disk work
  moved to `src/disk_dump.rs`, and `tests/support/memory_store.rs` proves the streamer runs against
  a store with no filesystem at all.
- **No publish without `dump.end`.** `write_from` consumes `&mut dyn DumpStream` and refuses to
  publish unless `completed()` observed `dump.end`; the streamer still calls `discard(sha)` and
  poisons the session when a stream is abandoned.
- **Frame `Debug` redaction (leak hazard).** `CapabilityRequest` and `ServerFrame` no longer derive
  `Debug`: hand-written impls print `[REDACTED]` for `proposed`, `env.bundle.secrets`,
  `secrets.ensure.value`, and `[WITHHELD]` for server refusal text; a unit test asserts no known
  secret survives `format!("{frame:?}")`.
- **Independent reference server.** The harness now builds `"ospp-v1" || nonce_s || nonce_c ||
  server_name` and the pairing transcript itself and verifies raw `r || s` signatures with direct
  `p256` `verify_prehash`, so a spec-deviating production transcript or verifier cannot pass the
  live handshake tests.

## ADR-0008 capability wire table (now normative in `src/protocol.rs`)

`env.bundle { request_id, project, mode }` -> `env.bundle { request_id, vars, secrets, domains }`;
`secrets.ensure { request_id, project, mode, key, proposed }` -> `secrets.ensure { request_id, key,
value }` (server CAS returns the authoritative value); `resources.describe { request_id, project,
mode }` -> `resources.describe { request_id, s3_buckets, pg_databases }`; `dump.begin { request_id,
project, mode, db_id }` -> `dump.begin { request_id, db_id, sha256_compressed, compressed_bytes,
format }`, binary frames `<= 64 KiB`, `dump.end { request_id, sha256_compressed,
compressed_bytes }`; any refusal -> `capability.error { request_id, capability, kind, message }`
(message withheld client-side so a refusal can never echo a value).

## Acceptance (all commands exit 0)

### 1. Capabilities and the single `resource_offer()` mapper

```text
$ cargo test -p slipfork-ospp --all-targets
running 1 test    (unit: frame Debug redaction)
test result: ok. 1 passed; 0 failed

running 32 tests  (critical_ospp)
test result: ok. 32 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.57s
```

```text
$ cargo test -p slipfork-ospp --test critical_ospp -- --nocapture vars_secrets
running 1 test
test vars_secrets_bundle_is_typed_and_redacted ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 31 filtered out; finished in 0.04s

$ cargo test -p slipfork-ospp --test critical_ospp -- --nocapture ensure_converges
running 1 test
test ensure_converges_two_concurrent_callers_on_one_value ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 31 filtered out; finished in 0.04s

$ cargo test -p slipfork-ospp --test critical_ospp -- --nocapture resources_strategy
running 1 test
test resources_strategy_maps_the_bundle_into_one_offer ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 31 filtered out; finished in 0.04s

$ rg -q 'fn resource_offer' crates/slipfork-ospp/src
rc 0
```

### 2. Hermetic test default, fail-closed remote, streaming dump

```text
$ cargo test -p slipfork-ospp --test critical_ospp -- --nocapture test_mode_hermetic_default
running 1 test
test test_mode_hermetic_default_ignores_an_offered_remote_everything ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 31 filtered out; finished in 0.04s

$ cargo test -p slipfork-ospp --test critical_ospp -- --nocapture dump_stream
running 8 tests
test dump_stream_rejects_tmpfs_cache_roots ... ok
test dump_stream_rejects_an_unknown_archive_format ... ok
test dump_stream_truncation_discards_the_partial_artifact ... ok
test dump_stream_digest_mismatch_aborts_and_unlinks ... ok
test dump_stream_works_with_a_filesystem_free_store ... ok
test dump_stream_truncation_aborts_and_unlinks_before_any_restore ... ok
test dump_stream_rejects_an_oversized_binary_frame ... ok
test dump_stream_issues_a_digest_pinned_archive ... ok
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 24 filtered out; finished in 0.05s

$ cargo test -p slipfork-ospp --test critical_ospp -- --nocapture offline_vault
running 1 test
test offline_vault_resolves_every_secret_without_the_server ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 31 filtered out; finished in 0.16s
```

### 3. Gates

```text
$ cargo clippy -p slipfork-ospp --all-targets -- -D warnings
    Checking slipfork-ospp v0.1.0
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.07s   # rc 0

$ make check
... test result: ok. 1 passed ... (unit)
... test result: ok. 32 passed ... (critical_ospp)
... test result: ok. 14 passed ... (critical_engine)
guard: OK (283 file(s) checked)
check_docs_honesty: OK (8 published doc(s) match real CLI subcommands)
Build finished in 0.18s
# rc 0
```

## Extra gates

```text
$ cargo fmt --all -- --check                       # no output, rc 0
$ RUSTDOCFLAGS="-D warnings" cargo doc -p slipfork-ospp --no-deps
 Documenting slipfork-ospp v0.1.0 (...)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.02s   # rc 0
$ cargo test -p slipfork-ospp --all-targets        # 1 unit + 32 integration passed
$ uv run --no-project tools/guard.py
guard: OK (283 file(s) checked)

$ uv run --no-project tools/critical_guard.py --base HEAD --node M2.14
critical-guard: HEAD..working tree, 47 changed file(s)
  M  crates/slipfork-ospp/tests/critical_ospp.rs  [authorized by crates/slipfork-ospp/**]
  A  crates/slipfork-edge/tests/critical_edge.rs  [added (always allowed)]
critical-guard: OK: authorized by M2.14

$ ! rg -q 'std::env::' crates/slipfork-ospp/src                                   # rc 0
$ ! rg -q 'p256::pkcs8|der::Signature|from_der|to_der' crates/slipfork-ospp/src  # rc 0
$ ! rg -q 'expect\(' crates/slipfork-ospp/src                                     # rc 0
$ ! rg -q 'std::fs|fs::|File::|zstd::|remove_file' crates/slipfork-ospp/src/dump.rs # rc 0
```

## Adversarial assertions covered

- `test` mode under default `[resources]` stays `Local` / `CleanInitdb` for both S3 and PostgreSQL
  even when the reference server offers both; only an explicit `test = "remote"` changes that, and
  then a missing offer is `DagError::RemoteResourceUnavailable` (fail closed).
- Truncation (server closes before `dump.end`) and a corrupted payload both abort with
  `OsppError::SeedDump`; the dumps directory is empty afterwards (no `.part`, no published
  `.pgdump`), and the abandoned session is poisoned so a desynchronized frame stream is never
  reused for a database mutation.
- A filesystem-free `SeedDumpStore` publishes and serves the same real zstd archive (no `path`,
  `open()`/`verify_zstd()` from memory); the domain streamer contains no `std::fs`, so the port is
  honestly implementable over object storage or another host, not only a local disk.
- A truncated stream calls `SeedDumpStore::discard(sha)` and publishes nothing (asserted on the
  in-memory double); `write_from` also refuses publication unless `DumpStream::completed()` observed
  `dump.end`.
- Two concurrent `ensure` callers observe one value (`ensure_calls` all return it, the reference
  store holds it); `put_if_absent` identity is proven, not a client-side cache.
- With the reference server dropped, `LayeredSecretProvider<FileVaultProvider, OsppSecretProvider>`
  still resolves every vault secret; a generated `!!` is written only to the `0600` vault and the
  server records zero `secrets.ensure` calls.
- `domains::` are served only in `cloud` mode; a local provider scope returns `None`.
- No `Debug`/`Display` of `EnvBundle`, `OsppSecretProvider`, `RemoteResourceBundle`, or the offer
  contains a secret value or access key; the unit test also proves `CapabilityRequest::SecretsEnsure`
  and `ServerFrame::{EnvBundle, SecretsEnsure, CapabilityError}` render `[REDACTED]`/`[WITHHELD]`
  for every secret-bearing field.
- The reference server builds both signed transcripts from literal byte sequences and verifies
  signatures with direct `p256` `verify_prehash`, never calling the production transcript builders
  or verifier; the golden-byte transcript tests stay independent on the client side.
- Binary frames over 64 KiB, unknown dump formats, capability frames before `CHALLENGE`, and
  denied capabilities all fail closed with typed errors naming the capability and `<project>:<mode>`
  where a capability was invoked.

## M2.13 follow-ups

1. **Frame enum dedupe** — applied (`ServerFrame` + shared `WireReject`).
2. **`is_lower_hex_digit` triple copy** — applied (`src/hex.rs`).
3. **`Deadline` phase/budget** — applied (`OsppConnection::budget` tracks the armed handshake/io
   budget and `OsppError::Deadline.millis` reports it).
4. **`tpmd-proto` pin hoist** — not applied: it requires `[workspace.dependencies]` in the root
   `Cargo.toml`, outside this node's `crates/slipfork-ospp/**` path lock. The crate keeps the pinned
   dev-dependency unchanged.
5. **Purity-grep fragility** — applied: verification now greps `std::env::`,
   `p256::pkcs8|der::Signature|from_der|to_der`, and `expect(` instead of `std::env` / `der::`.

## Notes

- **IoC boundary for seed dumps (review-ioc fix):** `src/dump.rs` owns only the `SeedDumpStore` /
  `VerifiedDump` capabilities and the WebSocket frame stream; it contains zero `std::fs` and zero
  on-disk zstd. `src/disk_dump.rs` is the sole filesystem adapter (`<sha>.part` -> verified atomic
  `<sha>.pgdump`, tmpfs rejection, `discard`, `open`, `verify_zstd`), and
  `tests/support/memory_store.rs` proves a non-filesystem store satisfies the identical contract.
- `slipfork_snow::ProviderError` has no remote variant, so capability failures surface as
  `ProviderError::Io { path: "ospp-v1/<capability>/<project>:<mode>", message }`. This is the only
  honest mapping without changing `slipfork-snow` (outside this path lock); values never appear.
- Cache reuse is the caller's decision (`SeedDumpStore::lookup` + `ResourcePlan`); `DumpStreamer`
  always performs the live `dump.begin` exchange, so `--refresh-dump` semantics stay in
  `ResourcePlan`/`slipforkd` with zero second `ResourceStrategy`/`DumpPolicy` resolver.
- `DiskSeedDumpStore::open` refuses `tmpfs` (verified against `/dev/shm`); tests use
  `CARGO_TARGET_TMPDIR` so normal runs never land on tmpfs.
- Non-blocking clean-ups collected in `plan/nodes/M2/M2.14/FOLLOWUPS.md` (one `scope_label`, one
  `exchange(...)` helper, hex-predicate reuse, `CAPABILITY_*` constants in the harness,
  `send_reject` dedupe, unused `getrandom` removal).
