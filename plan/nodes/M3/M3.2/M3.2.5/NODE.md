---
id: M3.2.5
title: "CommandTestAdapter: zero-plugin generic polyglot executable adapter"
kind: task
status: superseded
owner: implementer
priority: 2
depends_on: [M3.2.4]
paths:
  - "crates/slipfork-test/src/command_adapter.rs"
  - "crates/slipfork-test/src/lib.rs"
  - "crates/slipfork-test/tests/critical_command_adapter.rs"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-16T16:59"
updated: "2026-11-18T22:19"
---

# Goal

Implement `CommandTestAdapter`: run any polyglot test CLI (`pytest`, `vitest`, `phpunit`, `cargo nextest`) as a zero-plugin sharded command inside its world, mapping exit codes/parseable output into typed `TestOutcome`s and passing the shared conformance suite.

# Context

- Not every project can ship a protocol plugin; `ADR-0006` §3 keeps `CommandTestAdapter` as the zero-plugin path. It must still honour `SLIPFORK_OUTPUT_DIR`, world env, and per-slot isolation chosen by `isolation: batch|world`.

# Design / Invariants

1. `src/command_adapter.rs`: `CommandTestAdapter` holds a typed `Vec<String>` argv template plus a declared `IsolationTier`; it executes through the injected world-exec capability (never a host `Command`, never a shell), with `cwd`, env overlay, and a bounded timeout. Placement of `{item}`/file arguments is a typed, documented substitution — never `format!` into a shell string.
2. `discover` returns one `DiscoveredTest` per configured shard (file glob expansion happens through the injected filesystem/workspace capability, not the current directory), with stable ids derived from the relative path.
3. `run_items` maps: exit 0 with expected summary → `Passed`; non-zero → `Failed` with the captured tail (bounded) as `message`; a killed/timeout run → `Failed` naming the timeout. It never swallows a non-zero exit.
4. `SLIPFORK_OUTPUT_DIR` and world env come from `WorldSlotContext`; the adapter writes nothing outside them.
5. Tests: `tests/critical_command_adapter.rs` uses a fake world-exec capability (argv echo + scripted exit codes) to assert argv construction, one-to-one outcome mapping, timeout handling, and a full conformance-suite pass.

# Acceptance

```bash
cargo test -p slipfork-test --all-targets
cargo clippy -p slipfork-test --all-targets -- -D warnings
grep -n 'assert_test_adapter_conformance' crates/slipfork-test/tests/critical_command_adapter.rs
grep -n 'SLIPFORK_OUTPUT_DIR' crates/slipfork-test/src/command_adapter.rs
! grep -rn 'sh -c\|Command::new("sh")\|Command::new("bash")' crates/slipfork-test/src/
! grep -rn 'std::env::var' crates/slipfork-test/src/
make rs-test
```

Expected: tests and clippy exit 0; the conformance suite is invoked; the output-dir contract is honoured; both inverted `grep`s print nothing; `make rs-test` green.
