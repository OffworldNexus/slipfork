---
id: M3.32.3.3.4
title: Port protected live fixtures from snow resolve onto the shipped env/migrate verbs
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.32.3.3.3, M3.32.8]
paths: ["crates/slipfork/tests/**", "crates/slipfork-vcs/src/repo_root.rs", Makefile]
attempts: 3
max_attempts: 3
escalation: none
created: "2026-11-19T20:58"
updated: "2026-11-19T22:04"
---

# Goal
Rewrite the protected live fixtures that still shell `slipfork snow resolve` onto the shipped `slipfork env` / `slipfork migrate` verbs: `crates/slipfork/tests/support/mod.rs` (write a `Forkfile` + `project` override), `critical_slug.rs` (17 call sites), `critical_cli.rs`, `critical_world_journey.rs`, `critical_ospp_daemon.rs`.

# Context
M3.32.3.3.3 deletes `slipfork-snow` and the `snow resolve` verb; `slipfork env` ships in M3.32.4 and `slipfork migrate` in M3.32.8, so the port must land after both. Port, never weaken: refusals still fail closed, secrets still redacted, `-pgid` process groups still reaped, and `RepoRootResolver`/vault-keying coverage must be preserved by the replacement verb.

# Acceptance
- `grep -rn "snow resolve\|snow\.yml\|slipfork\.toml" crates/slipfork/tests` is empty.
- `cargo test -p slipfork --tests --no-run` links every suite; `tools/critical_guard.py --base <base> --node M3.32.3.3.4` authorizes the ported assertions (never weaken).
- Full live proof runs at the `M3.32.6` VM gate.
