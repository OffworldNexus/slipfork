---
id: M3.32.2.2.2.1
title: "slipfork-engine ForkfileRunner: sequential execve lifecycle, [1/N] progress, Forkfile:<line> fail-fast"
kind: task
status: done
owner: implementer
priority: 2
depends_on: []
paths: ["crates/slipfork-engine/**", "crates/slipfork-forkfile/src/**", Cargo.toml, Cargo.lock]
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-19T18:45"
updated: "2026-11-19T18:52"
---

# Goal

Add the engine lifecycle runner as a pure, injected-executor library surface.

# Context

Split from `M3.32.2.2.2`. There is no lifecycle surface in the engine today (hooks are daemon-side `sh -c` over `snow.yml`), so this is an addition, not an extension.

- New `crates/slipfork-engine/src/lifecycle.rs`: a `ForkfileRunner` borrowed from an `EvaluatedFork` (`HookRun { phase, argv }`, `EvaluatedTask { commands, multi }`).
- Sequential argv via an injected `FnMut(&[String]) -> Result<ExecOutput, EngineError>`; the caller supplies the world/pod executor — no ambient state, no `std::env`, no self-owned `ServiceHandle`.
- `[1/N]` plus elapsed progress through an event callback; fail-fast citing `Forkfile:<line>` plus the last 15 output lines; tainted argv slots redact as `[REDACTED]`.
- Declare `slipfork-forkfile` as a workspace dep here.
- Lab test with real processes proving a failing hook stops the sequence and reports `Forkfile:<line>`.
- Do NOT touch the daemon, the PTY entry shim, or `stream.rs` test probes (later slices).

# Acceptance

- `cargo test -p slipfork-engine` exits 0; `cargo test --workspace --all-targets --no-run` links.
- `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all -- --check` exit 0.
- Write NO `RESULT.md`.
