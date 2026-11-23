---
id: M3.32.6.1
title: "Port slipfork-vcs critical suite off the deleted slipfork.toml loader"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths: ["crates/slipfork-vcs/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-19T22:41"
updated: "2026-11-19T22:41"
---

# Goal
Remove the last `slipfork.toml` residue from the protected `slipfork-vcs` suite: the manifest override now lives in the `Forkfile` `project <slug>` statement, so these assertions must name/port to `Forkfile`.

# Context
`crates/slipfork-vcs/tests/critical_vcs.rs:107,450,459,532-561,656,776` still write/expect `slipfork.toml` (pre-1.0 leftover from the deleted loader). `crates/slipfork-vcs/src/repo_root.rs` already renders `Forkfile` hints. Port each case to a `Forkfile` fixture (the override is `project <slug>` in the root `Forkfile`), keeping every assertion at the same strength — the protected file must not be weakened.

# Acceptance
- `! grep -rn "slipfork\.toml" crates/` exits 0.
- `cargo test -p slipfork-vcs --lib --bins --test critical_vcs` exits 0 (Btrfs not required).
- `uv run --no-project tools/critical_guard.py --base <base> --node M3.32.6.1` authorizes the port.
- Write NO `RESULT.md`.
