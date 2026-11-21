---
id: M3.40.4
title: "Single per-user daemon (daemon.sock 0600, NDJSON), status/ps tree, doctor, daemon install"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.40.3]
paths:
  - "crates/slipfork/src/daemon/**"
  - "crates/slipfork/src/cli/**"
  - "crates/slipfork/src/commands/**"
  - crates/slipfork/src/render.rs
  - crates/slipfork/src/main.rs
  - crates/slipfork/Cargo.toml
  - "crates/slipfork/tests/**"
  - Cargo.toml
  - Cargo.lock
  - Makefile
  - "specs/03-under-the-hood.md"
  - "specs/02-cli-ux.md"
  - TESTING_PLAN.md
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-21T12:50"
updated: "2026-11-21T12:50"
---

# Goal
One per-user daemon owning every world under the current UID, plus `status`/`ps`, `doctor`, and `daemon install [--ingress]`.

# Context
- `crates/slipfork/src/main.rs:36-52` is the only composition root: it builds `HostEnv` (`runtime_dir` from `XDG_RUNTIME_DIR` else `/run/user/<uid>`, `exe`, `PATH`, uid/gid, arch, resolv.conf); fields at `crates/slipfork/src/world/mod.rs:29`. `main.rs` emits `exit 2` for usage errors.
- No `daemon.sock` client, no daemon loop, no ingress wiring exists today; verbs are only `parse.rs:41-48` (help/version/up/down/restart/fmt/migrate/__supervise after `.1` removes `lsp`).
- Control plane: `<runtime_dir>/daemon.sock`, mode `0600`, versioned NDJSON frames per `specs/03` §5; accept ingress connection fds via `SCM_RIGHTS` `recvmsg` from the `LISTEN_FDS` socket using the API shipped by `.3` in `slipfork-ingress` (do not edit `crates/slipfork-ingress/**`).
- Tree rendering: `crates/slipfork/src/render.rs` (`paint`) plus a new dynamic-width tree module - compute widths after stripping ANSI, never `{:<16}`-style shears; `running` green, `failed` red, 12-char digests dimmed; `○ No worlds running` exits 0.
- `packaging/` is outside epic paths: embed the systemd user unit and `/etc/apparmor.d/slipfork` profile as constants in `crates/slipfork/src/daemon/install.rs`; `daemon install` warns on stderr when ingress (`:80`, AppArmor, sudo) is not installed.
- Daemon state must be reaped cleanly: no orphaned `-pgid` process groups on shutdown.

# Acceptance
- `cargo test -p slipfork --test critical_cli_surface` plus a daemon journey test using a real `XDG_RUNTIME_DIR` under `/var/tmp`: socket mode is `0600`, two clients get coherent NDJSON, shutdown leaves no child processes.
- `cargo test -p slipfork`.
- `cargo clippy -p slipfork --all-targets -- -D warnings`.
- ANSI purity: `NO_COLOR=1 slipfork` and `slipfork | cat` contain zero `\x1b[`; `slipfork --json` parses; empty state prints `○ No worlds running` with exit 0.
- `uv run --no-project tools/critical_guard.py`.
