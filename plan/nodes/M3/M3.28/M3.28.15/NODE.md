---
id: M3.28.15
title: "Delete the unimplemented tunnel-sharing surface (slipfork share, TunnelRegistrar, WorldOrigin::Shared, support tier)"
kind: task
status: ready
owner: implementer
priority: 2
depends_on: [M3.28.11]
paths:
  - crates/slipfork/src/cli.rs
  - crates/slipfork/src/main.rs
  - crates/slipfork/src/daemon.rs
  - crates/slipfork/src/bin/slipforkd.rs
  - crates/slipfork/src/lib.rs
  - "crates/slipfork/tests/**"
  - "crates/slipfork-edge/src/**"
  - "crates/slipfork-edge/tests/**"
  - "crates/slipfork-world/src/**"
  - "crates/slipfork-world/tests/**"
  - "crates/slipfork-dag/src/support.rs"
  - support/integrations.toml
  - docs/reference/cli.md
  - docs/index.md
  - "docs/snow-modes.md"
  - README.md
  - docs/reference/manifest.md
  - "docs/design/target-dx.md"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-17T17:40"
updated: "2026-11-17T17:40"
---

# Goal

`slipfork share` is unimplemented vaporware: it reaches `UnavailableTunnelRegistrar`, whose `register()` unconditionally returns `Err(TunnelError::DaemonUnavailable)`. A command that can never succeed must not exist in `0.1.0`. Delete the entire tunnel-sharing surface — command, `DaemonControl` methods, registrar capability, conformance doubles, wire types, the `Shared` origin kind, support-registry tier, and every published claim — in one sweep. No rename, no alias, no `#[deprecated]`, no "coming in 0.2.0" teaser.

# Context

Verified by the conductor (re-check each file before you delete it; the line numbers are the starting point, not the authority):

- `crates/slipfork/src/cli.rs:126-133` — `Share { stop, world }`; dispatch in `crates/slipfork/src/main.rs:124` + `fn share(...)` near `main.rs:540` (calls `client.share` / `client.unshare`).
- `crates/slipfork/src/daemon.rs:489` — `Arc::new(UnavailableTunnelRegistrar::new())`; `crates/slipfork/src/bin/slipforkd.rs:10` documents it.
- `crates/slipfork-edge/src/tunnel.rs` defines `UnavailableTunnelRegistrar`; `crates/slipfork-edge/src/lib.rs:24,70` docs + re-export it.
- `crates/slipfork-world/src/lib.rs` owns `TunnelRegistrar`, `TunnelError`, `TunnelLease`, `TunnelRequest`, and `WorldOrigin::Shared`.
- `crates/slipfork-world/tests/support/mod.rs` owns `InMemoryTunnelRegistrar` + `assert_tunnel_registrar_conformance`; `crates/slipfork-world/tests/critical_world.rs:292` uses it.
- `crates/slipfork-dag/src/support.rs` declares the `IntegrationCategory::Tunnel` ("outbound tunnel registrar (e.g. `weaved`)") tier; `support/integrations.toml` currently has **no** tunnel entry (confirmed), so the tier is the residue.

`M4.1` (blocked on `H-006`) is the re-entry point: real tunnel sharing returns only with a real `weaved` IPC spec + adapter + a restored conformance suite.

# Design / Invariants

1. **CLI:** delete the `Share` clap variant, `fn share`, `--stop`, its `--world` plumbing, and every `critical_cli` case exercising share. `slipfork share` must exit `2` as an unrecognized subcommand.
2. **Daemon seam:** delete `DaemonControl::share` / `unshare` (daemon side in `crates/slipfork/src/daemon.rs` and client side) and their wire request/response types; delete the `UnavailableTunnelRegistrar` construction and the `slipforkd.rs` doc line that mentions it.
3. **Edge:** delete `crates/slipfork-edge/src/tunnel.rs` (whole module), its `mod`/`pub use` lines and doc references in `crates/slipfork-edge/src/lib.rs`, and any edge test that only exists for it.
4. **World capability:** delete `TunnelRegistrar`, `TunnelLease`, `TunnelError`, `TunnelRequest`, `InMemoryTunnelRegistrar`, `assert_tunnel_registrar_conformance` and the tests that only cover them. Do not leave an empty trait or an unused error module behind.
5. **`WorldOrigin::Shared` is deleted; `0.1.0` has exactly one origin kind, `Local`.** Collapse the enum/type so a `Shared` case is unrepresentable, and delete every branch that exists only for it: the `308` redirect for shared `*.localhost`, the `421` on HTTP `Upgrade`, and `shared_hosts` in `crates/slipfork-edge/src/**`. Update every `match origin` site; a `match` arm that only handled `Shared` goes away, and no `_ =>` catch-all may be left to swallow it.
6. **Support registry:** delete `IntegrationCategory::Tunnel` and the doc sentences that advertise an outbound registrar tier in `crates/slipfork-dag/src/support.rs`. `support/integrations.toml` has no tunnel entry to remove — verify and say so in `RESULT.md`; do not invent one to delete.
7. **Docs truth in the same commit** (Zero Vaporware; `tools/check_docs_honesty.py` fails on any published subcommand the binary lacks): delete the `share` rows/sections from `docs/reference/cli.md`, `README.md`, `docs/index.md`, `docs/snow-modes.md`, `docs/reference/manifest.md`, and the sharing recipes from `docs/design/target-dx.md`. `docs/VISION.md` and `ADR-0005` keep the future intent but must read as **future**, not as a shipped `0.1.0` capability.
8. **Delete the tests of a deleted behaviour in the same commit.** Never weaken an unrelated assertion, never route around a failing protected critical test, never leave a `#[ignore]`.
9. **Keep untouched** (all real, all have consumers): OSPP `auth`/`tpmd` pairing, `ResourceStrategy` fail-closed `not_offered`, `forward`, `secret`, `hook`, `db`, `redis`, `logs`.

# Acceptance

```console
$ cargo run -q -p slipfork -- share; echo "exit=$?"
error: unrecognized subcommand 'share'
exit=2
$ cargo run -q -p slipfork -- --help | grep -q '^  share' && echo STILL-SHARE || echo share-gone
share-gone
$ ! grep -rn 'TunnelRegistrar\|UnavailableTunnelRegistrar\|WorldOrigin::Shared\|slipfork share\|shared_hosts' crates/ docs/reference/ README.md support/ && echo no-tunnel-residue
no-tunnel-residue
$ ! grep -rn 'slipfork share\|tunnel shar' docs/ README.md && echo no-share-docs
no-share-docs
$ cargo test -p slipfork-world --test critical_world
... test result: ok
$ cargo test -p slipfork-edge --all-targets
... test result: ok
$ cargo test -p slipfork-dag --test critical_support
... test result: ok
$ cargo test -p slipfork --test critical_cli -- --test-threads=1
... test result: ok
$ uv run --no-project tools/check_docs_honesty.py
check_docs_honesty: OK
$ cargo clippy --workspace --all-targets -- -D warnings
    Finished
$ make check && make docs-check
... exit 0
$ git diff --shortstat <base_sha>
... deletion count recorded in RESULT.md
```

- `no-tunnel-residue` and `no-share-docs` are the inverted-grep proofs: zero survivors outside `docs/VISION.md`, `docs/decisions/` and `plan/`.
- `docs/VISION.md` and `ADR-0005` are allowed to keep the word "tunnel" **only** framed as future `0.2.0+` intent; a reader must not conclude `0.1.0` can share.
- `RESULT.md` records: the exact `git diff --shortstat` / deleted-line count, the removed symbol list, and the confirmation that `support/integrations.toml` carried no tunnel tier.
