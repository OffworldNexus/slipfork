---
id: M3.2.3
title: "RunnerProtocol v1 wire types, TestAdapter/CoverageDriver traits, shared conformance suite"
kind: task
status: ready
owner: implementer
priority: 2
depends_on: [M3.2.2]
paths:
  - "crates/slipfork-test/src/protocol.rs"
  - "crates/slipfork-test/src/adapter.rs"
  - "crates/slipfork-test/src/conformance.rs"
  - "crates/slipfork-test/src/lib.rs"
  - "crates/slipfork-test/tests/critical_protocol.rs"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-16T16:59"
updated: "2026-11-16T16:59"
---

# Goal

Implement `RunnerProtocol v1` (typed `AF_UNIX` NDJSON frames), the `TestAdapter`/`CoverageDriver` traits exactly as locked in `M3.2.1`, and the shared conformance suite every adapter must pass.

# Context

- `ADR-0006` §3 is normative for the wire grammar: `{"version":1,"op":"discover",...}` → `{"op":"discovered","items":[...]}`, then `{"version":1,"op":"run","world_id":...,"base_url":...,"output_dir":...,"items":[...]}` → streamed `item_result` frames → `run_complete`. Zygote-capable plugins hide their fork behind these same frames, with zero Zygote-specific messages.
- Every later adapter leaf (`M3.2.4`, `M3.2.5`, `M3.2.10`, `M3.2.11`) is verified by this suite, so it is the contract that prevents five divergent adapters.

# Design / Invariants

1. `src/protocol.rs`: serde types for both directions with `version: u16 = 1`, `deny_unknown_fields`, `IsolationTier { Batch, DedicatedWorld }`, `TestOutcome { id, status: Passed|Failed|Skipped, duration_ms, message: Option<String> }`, plus a typed error frame. Unknown `version`/`op`, oversized frames (> 64 KiB), and results for undiscovered ids are hard `TestError::Protocol` failures — never ignored.
2. NDJSON framing helpers: one JSON object per `\n`-terminated line, buffered reader with a bounded read, `flush` per frame on the writer side; no `read_to_end`, no unbounded memory.
3. `src/adapter.rs`: `trait TestAdapter { name; coverage() -> Option<&dyn CoverageDriver>; discover; run_items }` and `trait CoverageDriver { world_env; collect }` with `WorldSlotContext` carrying `target`, `endpoint` (origin + env), `output_dir` (`SLIPFORK_OUTPUT_DIR`), and optional slot-stable `coverage_dir` — names/types matching `ARCHITECTURE.md` byte-for-byte.
4. `src/conformance.rs`: `pub fn assert_test_adapter_conformance(adapter: &dyn TestAdapter, scratch: &Path)` asserting discover determinism, id stability, filter handling, ordered outcome streaming, `run_complete` exactly once, unknown-version rejection, and `CoverageUnsupported` surfacing when `[test].coverage` is on but `coverage()` is `None`. Panics with precise diagnostics (the repo's existing conformance style).
5. Tests: `tests/critical_protocol.rs` exercises the conformance suite against an in-file fake adapter plus raw-socket framing tests (partial frames, oversized frame, unknown op, duplicate `run_complete`).

# Acceptance

```bash
cargo test -p slipfork-test --all-targets
cargo clippy -p slipfork-test --all-targets -- -D warnings
grep -n 'version: u16' crates/slipfork-test/src/protocol.rs
grep -n 'deny_unknown_fields' crates/slipfork-test/src/protocol.rs
grep -n 'pub fn assert_test_adapter_conformance' crates/slipfork-test/src/conformance.rs
grep -n '64' crates/slipfork-test/src/protocol.rs
! grep -rn 'sh -c\|Command::new("sh")\|Command::new("bash")' crates/slipfork-test/src/
make rs-test
```

Expected: tests and clippy exit 0; the protocol declares `version: u16` and `deny_unknown_fields`; the conformance entry point exists; the frame cap is present; the inverted `grep` prints nothing (no shells anywhere); `make rs-test` green.
