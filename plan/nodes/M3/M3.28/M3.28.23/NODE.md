---
id: M3.28.23
title: "Attributed logs: the engine tags every chunk with its source and the CLI prefixes multi-source lines"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.28.22]
paths:
  - "crates/slipfork-engine/src/**"
  - "crates/slipfork-engine/tests/**"
  - "crates/slipfork-edge/src/**"
  - "crates/slipfork-edge/tests/critical_edge.rs"
  - crates/slipfork/src/daemon.rs
  - crates/slipfork/src/logs.rs
  - crates/slipfork/src/lib.rs
  - crates/slipfork/src/main.rs
  - "crates/slipfork/tests/**"
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-18T04:15"
updated: "2026-11-18T05:03"
---

# Goal

Attribute `slipfork logs` output: when more than one source is selected, every line is prefixed with `<component|hook>:` so a developer can tell which service produced it. A single named target stays unprefixed.

# Context

Owner transcript: `slipfork logs` interleaved sources with no attribution. `M3.28.11`'s attempt 1 traced it: engine log chunks carry no target (`crates/slipfork-engine/src/lib.rs:423-428`), the edge wire drops it (`crates/slipfork-edge/src/daemon/wire.rs:39-50`), and the daemon fans out chunks without a label (`crates/slipfork/src/daemon.rs:2835-2862`).

# Design / Invariants

1. The engine's log chunk carries its **source id** (component or hook id) on the wire; the edge passes it through unchanged; the daemon fans out `(source, bytes)`.
2. `crates/slipfork/src/logs.rs` prefixes every line with `<source>:` **iff** more than one source is selected. A single explicitly named target is printed raw (byte-exact, no prefix), and `--json` is unchanged (it already carries the structured source).
3. A line that spans chunks is not split mid-prefix: buffer per source until a newline, prefix whole lines, flush the trailing partial line at EOF.
4. No new dependency, no global state; the label is data on the wire, not a renderer guess.
5. The old unprefixed fan-out is deleted, not kept behind a flag.

# Acceptance

```console
$ cargo test -p slipfork --lib logs
... test result: ok
$ cargo test -p slipfork-engine --lib
... test result: ok
$ cargo test -p slipfork-edge --test critical_edge
... test result: ok
$ cargo test -p slipfork --test critical_cli -- --test-threads=1
... test result: ok
$ cargo clippy --workspace --all-targets -- -D warnings
    Finished
```

- Unit tests prove: two sources → every line prefixed and attributed correctly; one named source → raw bytes unchanged; a line split across chunks is prefixed exactly once; `--json` unchanged.
- `RESULT.md` records the wire change and the transcript.
