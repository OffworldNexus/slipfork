---
id: M3.2.4
title: "SocketRunnerAdapter: AF_UNIX NDJSON over runner.sock"
kind: task
status: superseded
owner: implementer
priority: 2
depends_on: [M3.2.3]
paths:
  - "crates/slipfork-test/src/socket_runner.rs"
  - "crates/slipfork-test/src/lib.rs"
  - "crates/slipfork-test/tests/critical_socket_runner.rs"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-16T16:59"
updated: "2026-11-18T22:19"
---

# Goal

Implement `SocketRunnerAdapter`: dial the plugin's `AF_UNIX` NDJSON socket at `.slipfork/runs/<run_id>/<slot>/runner.sock`, drive `discover`/`run`, and stream `TestOutcome`s — passing the shared `TestAdapter` conformance suite.

# Context

- `M3.2.3` locked the frames and the conformance suite. Language plugins (`pytest-slipfork`, future Playwright/PHPUnit/nextest runners) speak only this protocol, so all socket quirks live here once.

# Design / Invariants

1. `src/socket_runner.rs`: `SocketRunnerAdapter { socket: PathBuf, coverage: Option<Box<dyn CoverageDriver>> }` (or equivalent injected shape) implementing `TestAdapter`. Connect with a bounded timeout, write one frame per line, read frames with the shared bounded reader, and map every transport failure to `TestError::Protocol { .. }` naming the socket path and operation.
2. Socket path is validated (parent exists under the run tree, `sun_path` length <= 107 bytes) before dialling; overlong paths fail closed with the measured length.
3. `discover` sends filters verbatim; `run_items` sends exactly the items an earlier `discover` produced (ids are the adapter's contract), streams outcomes to the callback in order, and requires exactly one `run_complete` — a closed socket before it is a hard error carrying the last received id.
4. Environment for the plugin (`BASE_URL`/world env, `SLIPFORK_OUTPUT_DIR`, coverage env from `CoverageDriver::world_env`) is applied by the caller via `WorldSlotContext`; this adapter never reads `std::env`.
5. Tests: `tests/critical_socket_runner.rs` boots an in-test `AF_UNIX` NDJSON server implementing the protocol (including a slow/partial-frame case and a mid-run disconnect) and runs `assert_test_adapter_conformance` against the adapter.

# Acceptance

```bash
cargo test -p slipfork-test --all-targets
cargo clippy -p slipfork-test --all-targets -- -D warnings
grep -n 'runner.sock' crates/slipfork-test/src/socket_runner.rs
grep -n 'UnixStream' crates/slipfork-test/src/socket_runner.rs
grep -n '107' crates/slipfork-test/src/socket_runner.rs
grep -n 'assert_test_adapter_conformance' crates/slipfork-test/tests/critical_socket_runner.rs
! grep -rn 'std::env::var' crates/slipfork-test/src/
make rs-test
```

Expected: tests and clippy exit 0; the adapter names `runner.sock`, uses `UnixStream`, validates the 107-byte limit, and the critical test calls the shared conformance suite; the inverted `grep` prints nothing; `make rs-test` green.
