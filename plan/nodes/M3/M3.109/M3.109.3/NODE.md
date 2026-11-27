---
id: M3.109.3
title: "Loopback OCI registry lab: login 0600, no secret leak, private FROM/COPY --from build"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.109.2]
paths:
  - "crates/slipfork/tests/**"
  - crates/slipfork/Cargo.toml
  - "crates/slipfork/src/commands/docker/**"
  - Makefile
  - TESTING_PLAN.md
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-24T15:59"
updated: "2026-11-24T16:28"
---

# Goal

Prove the shipped binary against a real loopback OCI Distribution v2 registry that issues a 401 bearer challenge.

# Pre-baked context

- Registry fixture shape already proven in `crates/slipfork-image/src/registry.rs:479-612`: `TcpListener::bind(("127.0.0.1",0))`, `/v2/` 401 with `WWW-Authenticate: Bearer realm="http://ADDR/token",service="lab",scope="repository:private/app:pull"`, `/token` 200 `{"token":"t"}`, manifest/blob with `Authorization: Bearer t`. Reuse the same wire shapes, never a mock trait.
- Live tests are `required-features = ["live"]` in `crates/slipfork/Cargo.toml:16-29`; `make check` does not run them. Add a non-live CLI test for login/logout/list + a `live` journey for pull/build.
- Tests spawn the real binary (`env!("CARGO_BIN_EXE_slipfork")`) with an isolated `XDG_CONFIG_HOME`, `HOME`, `XDG_STATE_HOME`, `XDG_RUNTIME_DIR` under `/var/tmp` (disk, never RAM `/tmp`), and `SLIPFORK_INSTALL_ROOT` where needed. Existing harness: `crates/slipfork/tests/support/*.rs` (daemon reap, PTY) and `crates/slipfork/tests/critical_world_journey.rs`.
- Btrfs is unavailable on this host; the live test must fail closed with a remedy when `is_btrfs_path` is false (project pattern), never silently pass.

# Change

1. Non-live `crates/slipfork/tests/docker_cli_surface.rs`: real loopback bearer registry with real Basic-checked `/token`; `docker login --password-stdin --username u`, exit 0; `<config>/slipfork/registries.json` mode 0600; `docker list` shows host + username; wrong password -> exactly one line + one `next:`; `grep -R` the captured stdout/stderr/json for the secret -> none; `docker logout` removes; `--json` valid objects with zero `\x1b[`.
2. Live `crates/slipfork/tests/critical_registry_live.rs` (`required-features = ["live"]`, protected name `critical_*`): loopback registry serves a tiny real image (config + one tar layer) behind `scope=repository:private/base:pull` and `repository:private/tools:pull`; a Forkfile image has `FROM 127.0.0.1:PORT/private/base:latest` and `COPY --from=127.0.0.1:PORT/private/tools:latest /marker /marker`; run `docker login`, `docker pull`, `docker build app`, assert digest printed, marker copied, secret absent from every captured stream and from the stored rootfs (`grep -R` the rootfs).
3. No mocks/fakes/stubs; the registry is a real listener thread.

# Acceptance

- `cargo test -p slipfork --test docker_cli_surface` green on this host.
- `cargo test -p slipfork --features live --test critical_registry_live` is the documented Btrfs lab proof.
- `make check` stays green.
