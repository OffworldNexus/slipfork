---
id: M3.32.3
title: "slipforkd Forkfile/OSPP EvaluatedFork cut-over, port-agnostic .localhost ingress & multiplexed logs -f"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.32.2, M3.32.7]
paths: ["crates/**", Cargo.toml, Cargo.lock]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-19T13:02"
updated: "2026-11-19T16:00"
---

# Goal

Cut `slipforkd`, `slipfork-ospp`, `slipfork-ospp-testkit`, and the `slipfork` CLI daemon layer over to `crates/slipfork-forkfile` (deleting `crates/slipfork-snow` and every `snow.yml` loader), make `.localhost` ingress prefix/subdomain/port-agnostic, and make `logs -f` a true multiplexer that survives `restart`.

# Context

- Delete `crates/slipfork-snow` (`model.rs:1-985`, `faceless.rs:1-432`, `provider.rs:1-456`, `tests/critical_snow.rs`, `tests/claimed_eval.rs`) together with its `Cargo.toml` member entry; migrate surviving assertions into `crates/slipfork-forkfile/tests/`.
- Cut-over consumers (currently `use slipfork_snow::...`): `crates/slipfork/src/daemon/{world.rs:15, resolve.rs:15, env.rs:12, base.rs:4, postgres.rs:14, backing.rs:6, protocol.rs:9, vault.rs:7, ospp_supervisor.rs:19, types.rs:6, service.rs:9, mod.rs:33}`, `crates/slipfork/src/{vaults.rs:16, commands/process.rs:9}`, `crates/slipfork-ospp/src/{model.rs:11, client.rs:20, device_key.rs:16, keys.rs:14, error.rs:9}`, `crates/slipfork-ospp-testkit/src/{signers.rs:6, reference_server.rs:36-37}`. Wire frame `subscribe.forkfile: EvaluatedFork` through `crates/slipfork-wire/src/control/` (bump the protocol shape; no dual field).
- Ingress today: `crates/slipfork-edge/src/registry.rs:33` (`IngressRoute { prefix }`), `:265` (`resolve(host, path)`), `:386-397` (`prefix_matches`), and `crates/slipfork-edge/src/ingress.rs:378-385` (`host_header` then resolve). Required: strip `:<any_port>` from a `.localhost` `Host` before matching (`http://demo.localhost:8080` over `ssh -L` must hit the same world), and support `route mail.*` -> `mail.<project>.localhost` / `mail.<fork>.<project>.localhost` in addition to `route /` and `route /api`. Keep `slipfork-edge` free of CLI tables; the caller supplies the route table just as it does for prefixes.
- Logs today: `crates/slipfork/src/logs.rs:47-95` (`Renderer::new/chunk/finish`, prefixing + NDJSON), `crates/slipfork-wire/src/control/request.rs:125` (`LogsRequest`), `frame.rs:58` (`write_log`). Required per `dx-cli-0.1.0.md` §2: merge stdout+stderr chronologically by default, multiplex **all** services/hooks concurrently (never serially block on `postgres`), keep `-f` alive across `restart`/`down`+`up` with a dimmed `── restarted <time> ──` divider, and keep the NDJSON contract `{"ts_ms","target","stream","text"}`.
- Daemon-side fan-in: `crates/slipfork/src/daemon/{world.rs, service.rs, control.rs}` own the per-service children; expose one merged stream rather than N sequential readers.

# Acceptance

- `! grep -rn 'slipfork_snow\|slipfork-snow\|snow.yml' crates/ Cargo.toml` exits `0`.
- `cargo test -p slipfork-edge -p slipfork-wire -p slipfork-ospp -p slipfork -p slipfork-dag` exits `0`, including new unit tests: `Host: demo.localhost:8080` resolves identically to `demo.localhost`; `mail.demo.localhost` hits the `mail.*` route; `/api/...` beats `/`; `../` and `demo.localhost.evil.com` are refused.
- A logs test proves two targets interleave concurrently and a restart emits the dimmed divider, and one-named-target output stays raw/unprefixed.
- `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check` exit `0`; `make check` exits `0`.
- Write NO `RESULT.md`.
