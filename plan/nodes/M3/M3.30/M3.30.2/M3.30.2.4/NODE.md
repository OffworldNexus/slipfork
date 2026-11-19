---
id: M3.30.2.4
title: "Composition root <250 LOC + CliApp/commands split + VcsCli struct"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.30.2.3]
paths: [Cargo.toml, Cargo.lock, "crates/slipfork/**", "crates/slipfork-vcs/**"]
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-19T00:03"
updated: "2026-11-19T01:23"
---

# Goal
Section 3 (`main.rs` 1,676 LOC / 54 free fns + `slipfork-vcs::tool_command`) of `plan/nodes/M3/M3.30/M3.30.2/NODE.md`.

# Work
- Reduce `crates/slipfork/src/main.rs` to a `< 250`-LOC Composition Root (only place reading env/proc/sockets) plus a cohesive `CliApp { output_mode, layout, ui }` struct and `crates/slipfork/src/commands/*.rs` modules.
- Collapse `crates/slipfork-vcs/src/tool_command.rs` (12 free fns) into a cohesive `VcsCli` struct.
- Preserve every command behavior, diagnostic, `next:` remedy, and exit code.

# Constraints
No new traits/globals; no CLI tables leaking into library crates; no aliases. No RESULT.md.

# Acceptance
- `test $(wc -l < crates/slipfork/src/main.rs) -lt 250`
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test -p slipfork --lib`
- `cargo test -p slipfork --test daemon_unit_install`
- `cargo test -p slipfork-vcs`
