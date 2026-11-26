---
id: M3.102
title: "Client/daemon identity handshake: refuse a stale daemon with one remedy"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths: ["crates/slipfork/src/daemon/**", crates/slipfork/tests/daemon_skew_live.rs]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-24T11:34"
updated: "2026-11-24T11:41"
---

# Problem

After `make install` (dev CLI in `~/.local/bin`) the packaged `/usr/bin/slipfork __daemon` keeps serving old code; the CLI silently talks to it, so brand-new daemon-side features (store root, run catalog) appear broken and internal errors leak.

# Change

- `daemon` `ping` reply (`crates/slipfork/src/daemon/mod.rs:322`) adds `version` (`CARGO_PKG_VERSION`) and `exe` (canonical `std::env::current_exe()`).
- `DaemonClient::connect` and `connect_existing` perform a ping after connecting; if `exe` differs from the client's canonical exe (or `version`/`exe` is missing from an older daemon), return a typed `daemon-skew` failure:
  `the running daemon is \`<exe>\` (<version>), this CLI is \`<client>\` (<version>)`
  with one next: `systemctl --user stop slipfork` (use this binary's daemon) or run the daemon's binary; if the daemon is older and has no version, say it predates this CLI and `next: systemctl --user stop slipfork` / reinstall.
- Do not break the auto-spawn path: after `spawn_daemon`, the first ping must match.
- Keep the handshake cheap; no new mock traits. `--json` carries the typed error.

# Acceptance

- Integration test with two real binaries? If not feasible, a real lab: start daemon A, run client B built with a different name/path, assert `daemon-skew`; same binary passes. Use real processes, no mocks.
- `make check`; commit `[M3.102] ...`, done, no push.
