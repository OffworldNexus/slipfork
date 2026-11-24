---
id: M3.109.1
title: "Per-host registry credentials, 0600 store, Docker fallback, login probe"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.109]
paths:
  - "crates/slipfork-image/src/**"
  - "crates/slipfork-image/tests/**"
  - crates/slipfork/src/compose.rs
  - crates/slipfork/src/world/mod.rs
  - crates/slipfork/src/world/runtime.rs
  - crates/slipfork/src/world/runtime/images.rs
  - crates/slipfork/src/world/runtime/images/tests.rs
  - "crates/slipfork-world/tests/critical_pod_runtime.rs"
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-24T15:59"
updated: "2026-11-24T16:08"
---

# Goal

Give the puller per-host registry credentials from a 0600 store injected at the composition root, and expose a login probe the CLI can call.

# Pre-baked context

- `crates/slipfork-image/src/registry.rs:37` `Credentials` enum already redacts via Debug; `:243` `probe()` is unauthenticated discovery (ghcr placeholder-scope safe); `:336` `fetch_token` sends Basic to the realm.
- `crates/slipfork-image/src/puller.rs:114` `OciPuller::new(... credentials: Credentials ...)` and `RegistryClient::new` at `registry.rs:97` hold exactly one credential.
- `crates/slipfork/src/world/runtime/images.rs:85` hardcodes `Credentials::Anonymous` before constructing both pullers.
- `crates/slipfork/src/compose.rs:67` `compose::host` is the only env reader; `crates/slipfork/src/world/mod.rs:32` is the injected `HostEnv`.
- Existing atomic 0600 writer pattern: `crates/slipfork/src/daemon/registry.rs:70-105` (`create_dir_all`, temp, chmod 0600, rename).

# Change

1. New `crates/slipfork-image/src/credentials.rs` (export from `lib.rs`):
   - `RegistryCredentials` plain value struct: `BTreeMap<String, Credentials>` keyed by registry host; canonical host normalization (`docker.io` for `index.docker.io`, strip scheme/trailing slash); `credentials_for(host)`; redacted `Debug`; `Clone`.
   - `CredentialStore` (or the same struct) with `load(config_path, docker_config_path)`: read `<config>/slipfork/registries.json` (shape host -> `{username, secret}`), fall back per-host to Docker's `~/.docker/config.json` `auths[host].auth` base64 `user:pass` (never write it); missing files = empty.
   - `set(host, username, secret)`, `remove(host)`, `hosts()` (host+username only), `save(config_path)`: create dir, temp file, chmod 0600, atomic rename; secret never in logs/Debug.
2. `RegistryClient` resolves credentials per `reference.registry()` instead of one blob; `OciPuller::new` takes `RegistryCredentials`. Add loopback scheme selection (`127.0.0.1[:port]`, `localhost`) so the lab registry can speak HTTP, keeping HTTPS otherwise.
3. Add `RegistryClient`/`RegistrySession` login probe: unauthenticated `/v2/` discovery, then mint a token with Basic auth for realm+service only (do NOT mint ghcr's placeholder scope), or accept `GET /v2/` 200 with Basic where the registry does Basic. Wrong credentials -> typed error with one `next:`.
4. `compose::host` resolves `config_dir` (XDG_CONFIG_HOME or ~/.config), `docker_config` (~/.docker/config.json) and loads `RegistryCredentials` once; add fields to `HostEnv`, replacing the hardcoded `Anonymous` at `images.rs:85`. Keep library crates env-free.
5. Unit tests (real files under `/var/tmp`, zero mocks): store roundtrip + mode 0600, docker fallback parsing, host normalization, Debug redaction, loopback scheme; extend registry loopback test for Basic-token mint.

# Acceptance

- `cargo test -p slipfork-image`, `cargo clippy -p slipfork --all-targets -- -D warnings`.
- `<config>/slipfork/registries.json` is 0600 and secret never appears in Debug/errors.
- No new single-impl mock traits; no library `std::env`.
