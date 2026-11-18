---
id: M3.28.7
title: "Bare slipfork = status dashboard plus slipfork status (world tree, port-less origin, next-step hints)"
kind: task
status: done
owner: implementer
priority: 2
depends_on: [M3.28.6]
paths:
  - crates/slipfork/src/cli.rs
  - crates/slipfork/src/status.rs
  - crates/slipfork/src/lib.rs
  - crates/slipfork/src/main.rs
  - crates/slipfork/tests/critical_cli.rs
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-17T16:33"
updated: "2026-11-18T01:26"
---

# Goal

Bare `slipfork` becomes the status dashboard the owner typed four times, and `slipfork status` is the same command: project/world tree, state, port-less origin, and `logs`/`restart`/`open`/`down` hints. `--json` is pure.

# Context

Fact 2 (four bare invocations → the 25-command help wall; `status` was `unrecognized subcommand`) and fact 11 (`list` was read-only). `DaemonStatus` already carries pid, ingress addrs/port and every world with one origin (`M3.28.3`); `WorldOrigin::local` already renders port-less at 80 (`M3.28.4`). This node is dispatch + rendering only.

# Design / Invariants

1. `Commands.command: Option<Commands>`; `None` runs the same function as `Commands::Status`. `slipfork --help` still lists every command (tools/check_docs_honesty.py parses it) and `slipfork help` still works. No alias shim, no second help mode.
2. Human render: header `slipforkd <pid>, ingress <addr>:<port>`; per project the canonical world then indented `:fork` children, each with state, origin URL, worktree; then one hint line per world from the **one** `next` membership table (`M3.28.1` §10: running → `open`, `db`, `logs`, `restart`, `down`). Zero worlds → one line plus `slipfork up`.
3. **No auto-spawn** (`M3.28.1` R5): when nothing answers on `<state_dir>/run`, `status` prints exactly one line to stderr — `slipfork: no slipforkd on <path> — run \`slipfork up\` (installed? \`sudo systemctl start slipforkd.socket\`)` — and exits `1`. Only `up`/`fork` may start a daemon; read verbs never do.
4. `--json`: `{"ok": true, "ingress": {"addrs": [...], "port": 80}, "worlds": [{"world", "parent", "state", "origin", "worktree", "lineage": {"rootfs", "base", "seed"}}]}` on stdout only — no human text, no ANSI, every key always present with `null` for unknown. `parent` is `null` for the canonical world, `lineage` is the daemon's per-world `WorldLineage`; a `null` fact is `null`, never `0`/`""`. The process rows live in `ps --json`, not here.
5. The running world's lineage digests (`rootfs`, `base` key, `seed`) render on its line, short (`<digest12>` / `<key12>`), with `-` for an unknown fact; `up -v` owns the per-node detail, not this dashboard.
6. When the ingress port is not 80, `status` prints exactly one actionable line to **stderr** (never stdout, never under `--json`) naming `slipfork daemon install`. Never a silent mystery port.
7. Everything comes from `DaemonControl`; zero `/proc`, `/sys`, `std::env`, or log-directory reads. New rendering lives in `crates/slipfork/src/status.rs` (pure functions over the daemon payload), tested without a daemon. The CLI has no `--runtime-dir` global (`M3.28.1` R4): the socket is always `<state_dir>/run`.

# Acceptance

```console
$ cargo run -q -p slipfork -- --help | grep -q '^  status' && echo status-listed
status-listed
$ cargo run -q -p slipfork -- --help | grep -q '^Commands:' && echo help-intact
help-intact
$ cargo test -p slipfork --lib status
... test result: ok
$ cargo test -p slipfork --test critical_cli -- --test-threads=1
... test result: ok
$ ! grep -rn '/proc\|/sys/' crates/slipfork/src/status.rs crates/slipfork/src/cli.rs crates/slipfork/src/main.rs crates/slipfork/src/lib.rs && echo no-host-proc
no-host-proc
$ cargo clippy --workspace --all-targets -- -D warnings
    Finished
```

- Renderer tests: canonical + two forks (indentation), zero worlds, `:80` vs `:<port>`, the exact stderr line, lineage digests with `-` for a missing fact, and `--json` producing zero human text.
