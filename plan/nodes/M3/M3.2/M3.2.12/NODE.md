---
id: M3.2.12
title: "CLI slipfork test + DaemonControl::test streaming seam (typed exit codes, --json purity)"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.2.9, M3.2.10, M3.2.11]
paths:
  - "crates/slipfork/src/**"
  - crates/slipfork/Cargo.toml
  - "crates/slipfork-edge/src/**"
  - crates/slipfork/tests/critical_test_cmd.rs
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-16T16:59"
updated: "2026-11-16T16:59"
---

# Goal

Ship the `slipfork test` CLI end-to-end: `clap` surface, typed `DaemonControl::test` request/streaming response, and `slipforkd` wiring of `TestCoordinator` — with zero engine access from the CLI and zero shell interpolation anywhere in the path.

# Context

- `M3.2.1` locked the single daemon seam; `M3.2.9` implemented `TestCoordinator`. The CLI is a thin client (`H-010`): it parses flags, sends one typed request over `<runtime_dir>/daemon.sock`, and renders typed `TestEvent`s.
- `slipforkd` resolves `snow.yml`/`slipfork.toml`, `ResourcePlan::resolve(ResourceMode::Test)`, adapters, and the pool; the CLI must not read manifests.

# Design / Invariants

1. `slipfork test [-j N] [--host-caches] [--json] [-- <runner...>]` (plus typed adapter selection the blueprint locks): `clap`-only parsing, `cwd` sent explicitly, `--json` emits only JSON on stdout with no spinners/ANSI, and human output follows the existing `[resources]`/`[ok]`/`[fail]` line style with a final summary and coverage table.
2. `DaemonControl::test` (in `crates/slipfork-edge`) carries a typed `TestRequest` (repo root, filters, `-j`, adapter selection, host-caches, coverage overrides) and a `dyn FnMut(TestEvent)` sink; `slipforkd` auto-spawn stays exactly as today. No second socket, no `SCM_RIGHTS`.
3. Exit codes: `0` all passed and coverage satisfied; `1` any failed/skipped-by-request item or coverage below `coverage_fail_under`; `2` usage/config errors; `3` daemon/engine failures (including `BtrfsRequired` with the mount hint). Never report success on a partially booted run.
4. The daemon holds `TestCoordinator` per run (no global singleton), surfaces adapter conformance problems as `CoverageUnsupported`/`Protocol` errors before leasing worlds, and keeps `.slipfork/runs/<run_id>/` for inspection.
5. Tests: `crates/slipfork/tests/critical_test_cmd.rs` asserts `--help` surface, `--json` purity, exit codes 0/1/2, the `BtrfsRequired` exit-3 diagnostic on a non-Btrfs host, and that a fake adapter (test-only) produces one event per item.

# Acceptance

```bash
cargo test -p slipfork --all-targets
cargo clippy -p slipfork --all-targets -- -D warnings
cargo run -q -p slipfork -- test --help
grep -n 'fn test' crates/slipfork-edge/src/*.rs | head
grep -n 'TestRequest\|TestEvent' crates/slipfork/src/*.rs crates/slipfork-edge/src/*.rs | head
! grep -rn 'sh -c\|Command::new("sh")\|Command::new("bash")' crates/slipfork/src/
make rs-test
```

Expected: tests and clippy exit 0; `slipfork test --help` exits 0 and lists `-j`, `--json`, `--host-caches`; `DaemonControl::test`/`TestRequest`/`TestEvent` exist across CLI and edge; the inverted `grep` prints nothing; `make rs-test` green.
