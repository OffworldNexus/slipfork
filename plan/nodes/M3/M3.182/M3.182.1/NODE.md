---
id: M3.182.1
title: "Split slipfork-world pod_log/puller and route supervisor teardown failures to the journal"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.180]
paths:
  - "crates/slipfork-world/src/pod_log**"
  - "crates/slipfork-world/src/supervisor/**"
  - "crates/slipfork-image/src/puller**"
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-25T21:45"
updated: "2026-11-25T21:53"
---

# Goal

Split the two over-budget library files along their existing seams and delete both `eprintln!` leaks in the `slipfork-world` supervisor, keeping public behaviour byte-identical.

# Context (pre-baked, verified at HEAD 4173bd7)

- `crates/slipfork-world/src/pod_log.rs` = 645 lines. Seams: index maintenance `append_index` 323, `ensure_index` 334, `read_valid_index` 348, `rebuild_index` 374, `raw_ts` 406, `write_index` 414, `load_index` 436, `index_start` 591; reader `LogReader` 446, `LogRange` 501, `read_range` 563, `follow_range` 587, `LogMerge` 611; `LogSink`/`append_stream` 169-300 stay. Target dir module `pod_log/{mod,index,reader}.rs`; preserve the exact `pub` surface used by `crates/slipfork-world/src/lib.rs` and `pod_log::*` callers.
- `crates/slipfork-world/src/supervisor/mod.rs:252` and `supervisor/egress.rs:241` call `eprintln!` inside `Drop` (Law 1). Both owners hold a world identity; `WorldHost.logs_root` (`supervisor/mod.rs:43`) is the crate's durable sink. Route teardown failures through the existing `pod_log::append_stream(&logs_root, <world-id>, Stream::Stderr, …)` owner — no new single-impl trait, no new global, no `Box<dyn Write>` parameter bag.
- `crates/slipfork-image/src/puller.rs` = 524 lines: move `PullGuard` 68-105 and `await_flight` 487-502 into a dir module `puller/flight.rs` to clear 500.

# Acceptance

- `awk '/^#\[cfg\(test\)\]/{f=1} !f' <file> | wc -l` <= 500 for every file under `crates/slipfork-world/src/**` and `crates/slipfork-image/src/**`.
- `grep -rn "eprintln!" crates/slipfork-world/src` is empty.
- `cargo test -p slipfork-world -p slipfork-image` green; `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings`.
- Do **not** touch `crates/slipfork/src/**` (other slices own it), `specs/`, `TESTING_PLAN.md`, `CHANGELOG.md`, `docs/`, or any `critical_*.rs`.
