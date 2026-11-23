---
id: M3.32.3.3.3
title: "Remove snow resolve verb, delete slipfork-snow, port protected fixtures to Forkfile"
kind: task
status: done
owner: implementer
priority: 2
depends_on: [M3.32.3.3.2]
paths:
  - "crates/slipfork-snow/**"
  - "crates/slipfork-edge/**"
  - "crates/slipfork-wire/**"
  - "crates/slipfork/**"
  - "crates/slipfork-dag/**"
  - "crates/slipfork-engine/**"
  - "crates/slipfork-world/**"
  - "crates/slipfork-forkfile/tests/critical_evaluated_world.rs"
  - Makefile
  - tools/tests/test_tools.py
  - Cargo.toml
  - Cargo.lock
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-19T20:34"
updated: "2026-11-19T21:01"
---

# Goal
Remove the `snow resolve` product verb from `slipfork-edge`/`slipfork-wire`/`slipforkd`, delete `crates/slipfork-snow/` and every workspace/Makefile/tool reference to it, and port the protected live fixtures onto real `Forkfile` manifests — additive porting only, never weakening an assertion.

# Context
- `snow resolve` surface: required method on `trait DaemonControl` in `crates/slipfork-edge/src/` (ARCHITECTURE.md:28,134 lists `snow resolve` as non-existent after this epic), request/summary frames `SnowResolveRequest`/`SnowResolveSummary` in `crates/slipfork-wire/src/control/request.rs`, dispatch in `crates/slipfork/src/daemon/protocol.rs`, adapter body left by M3.32.3.3.2 marked `// removed in M3.32.3.3.3`. Delete all three together in this slice so the trait impl stays complete at every commit.
- `crates/slipfork-snow/` contents to delete: `Cargo.toml`, `src/{lib,model,provider,faceless.rs,faceless/claimed.rs}`, `tests/{claimed_eval.rs,critical_snow.rs}`. Its assertions were already ported to `crates/slipfork-forkfile/tests/critical_evaluated_world.rs` in M3.32.3.3.1 — the protected `critical_snow.rs` deletion is authorized by this node's `paths` and must be reported through `tools/critical_guard.py`.
- Workspace/harness refs: `Cargo.toml` member line `"crates/slipfork-snow"` (line 4), `Makefile:73-74` (`cargo test -p slipfork-snow --test claimed_eval|critical_snow`), `tools/tests/test_tools.py:51,55,77,92,177,211` (snow paths used by glob-overlap / critical-authorization self-tests) — retarget those fixtures to `crates/slipfork-forkfile/**` so the tests keep proving the same behaviour.
- Protected live fixtures to rewrite onto Forkfile manifests (real files, real git, no mocks): `crates/slipfork/tests/support/mod.rs`, `crates/slipfork/tests/support/install_tool.rs`, `crates/slipfork/tests/critical_cli.rs`, `critical_world_journey.rs`, `critical_slug.rs`, `critical_ospp_daemon.rs`. Keep every existing assertion; change only manifest text, fixture filenames (`snow.yml`/`slipfork.toml` -> `Forkfile`), and the labels they assert on.
- Do not touch `crates/slipfork-forkfile/src/**` (M3.32.3.3.1 owns the projection) or daemon boot logic beyond removing the verb (M3.32.3.3.2).
- Laws: zero `[guessed]`/fallback paths, no legacy aliases; every file <=500 LOC; docs honesty (`snow resolve` must not survive in `docs/reference/cli.md` examples either — report if outside your paths).

# Acceptance
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test -p slipfork-forkfile -p slipfork-dag -p slipfork-edge -p slipfork-wire -p slipfork-vcs -p slipfork-ospp`
- `cargo test -p slipfork --test critical_cli --test critical_slug --test critical_ospp_daemon --test critical_world_journey`
- `uv run --no-project tools/guard.py --base 8ef3a5e --node M3.32.3.3.3`
- `uv run --no-project tools/critical_guard.py --base 8ef3a5e --node M3.32.3.3.3` (authorizes the protected fixture rewrite and `critical_snow.rs` removal)
- `grep -rn "slipfork_snow\|slipfork-snow\|snow\.yml\|snow resolve" crates/*/src crates/*/tests Cargo.toml Makefile tools/tests` returns nothing for product code and for the crate-level test suites; the retained `slipfork-forkfile::migrate` converter is the supported migration path.
- The protected `crates/slipfork/tests/**` fixture port (`snow resolve` -> shipped verbs) is owned by `M3.32.3.3.4`, which depends on `M3.32.8`; it is a precondition of the `M3.32.6` VM gate, not of this node.
- Btrfs live suites (`critical_btrfs_live`) and `tools/do_vm.py sync/test` are deferred to the `M3.32.6` gate.
