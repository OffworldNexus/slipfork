---
id: M3.128
title: "Daemon identity includes the binary build (mtime+size), so a rebuilt exe invalidates a running daemon"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - crates/slipfork/src/daemon/identity.rs
  - crates/slipfork/src/daemon/client.rs
  - crates/slipfork/src/daemon/mod.rs
  - crates/slipfork/tests/daemon_skew_live.rs
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-24T20:40"
updated: "2026-11-24T20:44"
---

# Goal

A `slipfork __daemon` started before `make install` keeps serving the old code when the binary at the same path is rebuilt; `M3.102`'s ping compares path+version only, so it accepts it. `up --no-cache` then silently serves the old cache.

# Change

- The ping identity adds a **build stamp** of the daemon's exe: `mtime_ns` + `len` (cheap, no hashing), plus path/version.
- The client compares its own exe build stamp; a mismatch (or a daemon with no build stamp) is `daemon-skew` with the pid remedy (`kill <pid>` / `systemctl --user stop slipfork`), exactly one `next:`.
- Keep the auto-spawn path passing (the spawned daemon is the client exe).
- `--json` typed error; no secrets.

# Acceptance

- Real processes: start a daemon from binary A, replace the file at the same path with binary B (same version), run B → typed skew with A's pid; kill A → B spawns and connects. Then `up --no-cache` rebuilds.
- `make check`; commit `[M3.129] ...`, done, no push.
