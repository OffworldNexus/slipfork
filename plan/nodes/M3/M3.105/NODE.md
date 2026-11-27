---
id: M3.105
title: "Skew remedy names the stale daemon's pid (systemctl stop is not always the answer)"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - crates/slipfork/src/daemon/identity.rs
  - crates/slipfork/src/daemon/client.rs
  - crates/slipfork/tests/daemon_skew_live.rs
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-24T11:43"
updated: "2026-11-24T11:48"
---

# Problem

After `systemctl --user stop slipfork`, an older auto-spawned `slipfork __daemon` still held `/run/user/1000/daemon.sock`. The new handshake correctly refused it, but its remedy said `next: run systemctl --user stop slipfork` — which the user had just done. The stale daemon is not (only) a unit.

# Change

When the skew is against a daemon with no version (or a different exe/version), include the ping's `pid` and make the single next actionable for that process:
`next: kill <pid> (the stale daemon), then run slipfork up`
If the ping has no pid, keep `next: systemctl --user stop slipfork` (or `pkill -f 'slipfork __daemon'`). Exactly one `next:`; no embedded remedy duplication (M3.104 handles the JSON split).

Extend the real stale-daemon fixture (`daemon_skew_live.rs`): boot an old-style daemon (no `version` in ping), assert the message carries its pid and the `kill <pid>` next; killing it lets the CLI spawn and connect.

# Acceptance

- Real-process test green; `make check`.
- Commit `[M3.105] ...`, done, no push.
