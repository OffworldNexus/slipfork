# M2.2 RESULT — workspace wedge: `slipfork-edge` / `slipfork-vcs` / `slipfork-ospp` skeletons + M2 dependency declaration

STATUS: done (attempt 2 — Round-1 Quad Gate remediation)

Base: `aa4c5246451c1cba78a9cd431f59406638cefc76` (`release/0.2.0`); working tree left uncommitted for `lead`.

## Summary

1. **Three documented skeletons.** `crates/slipfork-edge`, `crates/slipfork-vcs`, `crates/slipfork-ospp` each ship a `Cargo.toml`, a `src/lib.rs` with `#![forbid(unsafe_code)]` plus `//!` docs naming the DI ports they will own, and module files that contain only `//!` ownership docs (no behaviour, no `todo!()`, no `pub` API beyond the documented stubs):
   - edge: `activation` (`std::env` `LISTEN_PID`/`LISTEN_FDS` boundary, `ListenerSet`), `daemon` (`DaemonControl` + `0600` `<runtime_dir>/daemon.sock` + `DaemonControlClient`), `ingress` (`httparse` <= 16 KiB dispatch + shared-host proxy), `registry` (`WorldRegistry`, `IngressRoute`), `tunnel` (`UnavailableTunnelRegistrar` / `H-006`-gated `WeavedRegistrar`), `unit` (`UnitSpec` `.socket`/`.service` renderer).
   - vcs: `ports` (`RepoRootResolver` / `VcsWorkspaceFactory` / `VcsWorkspace`, zero `std::process`), `repo_root`, `git` (`GitWorktreeAdapter`), `jj` (`JjWorkspaceAdapter`), `remote_helper` (pure line protocol; zero `src/bin/*`).
   - ospp: `client` (`OsppClient` / `ospp-v1`), `device_key` (`DeviceKeySigner` port + `KeyId`/`DeviceSignature` newtypes + `DeviceKeyStore`), `software_key` (`SoftwareP256Signer` and its `SoftwareP256Key`, the single home of the software-key arc), `tpmd` (`TpmdDeviceKeySigner`, private `current_thread` runtime), `pairing` (`PairingUrl` + `BrowserOpener`), `secret_provider` (`OsppSecretProvider`), `resources` (`RemoteResourceBundle` / `resource_offer` mapper), `dump` (`SeedDumpStore` / `DumpStreamer`).
2. **Single owner of the M2 dependency wedge.** `Cargo.toml` / `Cargo.lock` now declare every dependency M2 needs, so no later leaf touches a manifest or the lockfile. crates.io versions were resolved with `cargo add --dry-run -p slipfork-world` (never hand-written): `httparse 1.10.1`, `listenfd 1.0.2`, `p256 0.14.0`, `rustls 0.23.46`, `socket2 0.6.5`, `tungstenite 0.30.0`, `webpki-roots 1.0.12`, `zstd 0.14.4`. The pinned upstream git crates are written verbatim as the node specifies (`tpmd-client`, `tpmd-ipc`, `rev = "90cb2db45b3ea6bc065918f11c7e0fd23b4ce837"`; resolved to `tpmd-proto` at the same rev), and `tokio` is verbatim `{ version = "1", default-features = false, features = ["rt", "net", "time"] }`.
   - `tungstenite` carries `features = ["rustls-tls-webpki-roots"]` (the crate's documented, non-`__`-prefixed feature) because `Connector::Rustls` is feature-gated; without it `wss://` is unreachable and `M2.13` would have had to edit `Cargo.toml`, breaking this node's single-owner contract. `rustls`/`webpki-roots` stay direct `slipfork-ospp` deps so `M2.13` can build the `ClientConfig` explicitly.
3. **Crate-level edges exactly as listed.** `slipfork-world` gained `sha2` + `hex`; `slipfork-engine` gained the `slipfork-world` path dep (closes M1.6 #12); `slipfork-edge` gained `slipfork-world` + `slipfork-engine` + `serde`/`serde_json`/`thiserror`/`listenfd`/`socket2`/`httparse`; `slipfork-vcs` gained `slipfork-world` + `thiserror` only (see Round-1 remediation item 4: `serde`/`serde_json` were dropped — the `git-remote-slipfork` protocol is plain text and no M2.11/M2.12 value type is named as serialized; both stay in `[workspace.dependencies]`); `slipfork-ospp` gained its full list; `crates/slipfork` gained `slipfork-edge`/`slipfork-vcs`/`slipfork-ospp` and `slipfork-dag` with `features = ["fs"]`.
4. **Feature seam + cruft.** `crates/slipfork-dag/Cargo.toml` now declares `[features] default = []` and `fs = []` (the only `[features]` table in `crates/*`), matching the `ARCHITECTURE.md` contract: the feature is inert until `M2.5` moves `FsMerkleHasher`/`FsDigestCache` behind it, so a domain crate consuming dag at defaults never inherits filesystem capability. The lone `publish = false` is deleted (`rg 'publish' crates/*/Cargo.toml` → no hits).
5. **Dependency → leaf mapping** (Design #5) is recorded in `plan/nodes/M2/M2.2/LOG.md`: `tungstenite`/`rustls`/`webpki-roots`/`p256`/`tpmd-*`/`tokio` → `M2.13`; `zstd` → `M2.14`; `listenfd`/`socket2`/`httparse` → `M2.8`–`M2.9`; world `sha2`/`hex` → `M2.3`; engine → world → `M2.6`; edge/vcs/ospp edges → `M2.8`–`M2.14`; `crates/slipfork` edges → `M2.16`–`M2.17`; dag `fs` seam → `M2.5`.

## Round-1 Quad Gate remediation (attempt 2)

1. **BLOCKER — dag default feature.** `crates/slipfork-dag/Cargo.toml` is now `default = []` + `fs = []`; `crates/slipfork` keeps `features = ["fs"]`. The feature stays inert until `M2.5` gates the adapters, so dag consumed at defaults never grants filesystem capability (`ARCHITECTURE.md` contract). Verified both `cargo check -p slipfork-dag --all-targets` and `--no-default-features`.
2. **MAJOR — published MSRV.** `[workspace.package] rust-version` is now `"1.94"` (the pinned `tpmd-*` crates' real floor), so the manifest no longer publishes a false `1.88` promise. `docs/getting-started.md`'s "Rust 1.88+" sentence is **not** touched here; `M2.19` reconciles the published MSRV sentence (recorded in `LOG.md`).
3. **MINOR — one home for the software-key arc.** `crates/slipfork-ospp/src/device_key.rs` now documents only the `DeviceKeySigner` port, `KeyId`/`DeviceSignature` newtypes and `DeviceKeyStore`; `software_key.rs` owns `SoftwareP256Signer` **and** `SoftwareP256Key` (zeroize-on-drop, redacted `Debug`) as the single home `M2.13` implements. Docs/module boundaries only, no behaviour.
4. **MINOR — vcs serialization deps.** `serde` and `serde_json` are dropped from `crates/slipfork-vcs/Cargo.toml` (no M2.11/M2.12 type is named as serialized; `remote_helper` is a plain-text line protocol). The crate now depends on `slipfork-world` + `thiserror` only; both dropped crates remain in `[workspace.dependencies]` for their real consumers.
5. **MINOR — stub rustdoc tense.** `crates/slipfork-ospp/src/tpmd.rs` and `crates/slipfork-edge/src/activation.rs` are reworded to future tense ("will talk to `tpmd`", "will be the only module allowed to read…"), since no behaviour exists yet.

## Files

- `Cargo.toml` (modified: 3 members, 11 workspace dependencies, `rust-version = "1.94"`)
- `Cargo.lock` (modified: +1039/−10 lines, no existing package version changed)
- `crates/slipfork-dag/Cargo.toml` (modified: `[features] default = []` + `fs = []`, `publish = false` removed)
- `crates/slipfork-world/Cargo.toml` (modified: `sha2`, `hex`)
- `crates/slipfork-engine/Cargo.toml` (modified: `slipfork-world`)
- `crates/slipfork/Cargo.toml` (modified: 3 new path deps, dag `features = ["fs"]`)
- `crates/slipfork-edge/{Cargo.toml,src/*.rs}` (new)
- `crates/slipfork-vcs/{Cargo.toml,src/*.rs}` (new)
- `crates/slipfork-ospp/{Cargo.toml,src/*.rs}` (new)
- `plan/nodes/M2/M2.2/LOG.md` (dependency map + observations, via `plan.py log`)

No `docs/**`, `support/**`, `poc/**`, `packages/**`, or test file was touched.

## Acceptance evidence (exact commands / results — every command below was re-run after the Round-1 remediation)

1. `test -f crates/slipfork-edge/Cargo.toml -a -f crates/slipfork-vcs/Cargo.toml -a -f crates/slipfork-ospp/Cargo.toml` → exit 0;
   `cargo metadata --no-deps --format-version 1 > /dev/null` → exit 0.
2. `rg -q 'forbid\(unsafe_code\)' crates/slipfork-edge/src/lib.rs crates/slipfork-vcs/src/lib.rs crates/slipfork-ospp/src/lib.rs` → exit 0 (matches at `lib.rs:28`, `:17`, `:22`);
   `cargo check --workspace --all-targets` → `Finished \`dev\` profile ...`, exit 0;
   `cargo clippy --workspace --all-targets -- -D warnings` → `Finished \`dev\` profile ...`, exit 0.
3. `rg -q 'slipfork-world' crates/slipfork-engine/Cargo.toml` → exit 0;
   `rg -q 'features = \["fs"\]' crates/slipfork/Cargo.toml` → exit 0;
   `rg -q 'publish = false' crates/*/Cargo.toml` → exit 1 (no match, as required by `!`);
   `rg -q '90cb2db45b3ea6bc065918f11c7e0fd23b4ce837' Cargo.toml` → exit 0;
   `rg -q 'httparse' crates/slipfork-edge/Cargo.toml` → exit 0.
4. `rg -c "^fs = " crates/slipfork-dag/Cargo.toml` → `1`; `rg -c "^default" crates/slipfork-dag/Cargo.toml` → `1` (`default = []`);
   `cargo check -p slipfork-dag --all-targets` → exit 0 (extra: `--all-targets --no-default-features` → exit 0; both sides of the seam compile, since `crates/slipfork` builds dag with `features = ["fs"]`).
5. `uv run --no-project tools/plan.py validate` → `plan OK`, exit 0;
   `make check` → exit 0 (plan-check, guard, ruff check/format, pytest, bun test, `cargo fmt --check`, clippy + `RUSTDOCFLAGS="-D warnings" cargo doc`, `cargo test --all-targets` + `--doc`, `zensical build --strict` → `No issues found`, `check_docs_honesty: OK (8 published doc(s) match real CLI subcommands)`).

Additional gates:
- `cargo fmt --all -- --check` → exit 0 (no output).
- `cargo check --workspace --all-targets --locked` → exit 0 (lockfile complete/consistent).
- `uv run --no-project tools/guard.py` → `guard: OK (237 file(s) checked)`.
- `uv run --no-project tools/critical_guard.py --base aa4c5246451c1cba78a9cd431f59406638cefc76 --node M2.2` → `no change to a protected critical test`.
- `cargo tree -p slipfork-ospp` → `tpmd-client v0.1.0 (https://github.com/OffworldNexus/tpmd?rev=90cb2db45b3ea6bc065918f11c7e0fd23b4ce837#90cb2db4)`, `tpmd-ipc v0.1.0 (... same rev ...)`, `tpmd-proto v0.1.0 (... same rev ...)`, plus `tungstenite v0.30.0 → rustls v0.23.46 → aws-lc-rs v1.19.0`, `tokio v1.54.1`, `zstd v0.14.4`.

## Observed (recorded in `LOG.md`)

- **MSRV:** resolved in this attempt — `rust-version = "1.94"` now matches the pinned `tpmd` crates' declared floor. `docs/getting-started.md` still publishes "Rust 1.88+"; `M2.19` owns that published-docs sentence (this node leaves `docs/**` untouched). Nothing was dropped or substituted.
- **`cargo::unused_dependencies` (cargo 1.100 manifest lint):** warns for the dependencies this node pre-declares for a later leaf (17 warnings; zero rustc/clippy warnings). It is the intended state of a declare-before-use wedge and disappears as each consuming leaf lands, so it is **not** suppressed — an `allow` would have blinded `M2.20`'s unused-dependency audit. `cargo clippy ... -D warnings`, `make check`, and every `# Acceptance` command still exit 0.
- **`webpki-roots` versions:** `tungstenite 0.30` requires `webpki-roots 0.26.11`, which is a thin re-export of `webpki-roots 1.0.12`; the workspace declares `1.0.12`, so there is a single real root table, not two.
