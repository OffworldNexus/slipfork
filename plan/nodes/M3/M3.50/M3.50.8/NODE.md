---
id: M3.50.8
title: "Timestamped log index (<id>.idx) + slipfork logs multi-target --since/--until/-t"
kind: task
status: done
owner: implementer
priority: 5
depends_on: [M3.50.1, M3.50.2]
paths:
  - "crates/slipfork-world/src/pod_log.rs"
  - "crates/slipfork-world/src/control/**"
  - "crates/slipfork-world/src/bin/world_init/stream/tail.rs"
  - "crates/slipfork-world/src/bin/world_init/stream.rs"
  - "crates/slipfork-world/tests/pod_control_lab.rs"
  - crates/slipfork/src/daemon/lifecycle/logs.rs
  - crates/slipfork/src/world/mod.rs
  - crates/slipfork/src/cli/verbs.rs
  - crates/slipfork/src/commands/streams.rs
  - crates/slipfork/tests/critical_world_journey.rs
  - crates/slipfork/tests/critical_cli_surface.rs
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-22T13:21"
updated: "2026-11-22T13:35"
---

# Goal
Every captured log line is timestamped and indexed; `slipfork logs` filters by service(s) AND/OR time, seeking straight to the first matching byte instead of scanning from 0.

# Context
- `crates/slipfork-world/src/pod_log.rs` already writes `<epoch_ms>\tL|D\t<text>` via `LogSink`/`append` and parses with `LogReader`/`LogLine::parse`; reuse that as the single owner.
- `LogSink` + `pod_log::append` must maintain a companion `<store>/logs/<world>/<id>.idx`, mode `0600`, append-only, fixed 16-byte big-endian `(ts_ms: u64, byte_offset: u64)`, one entry on the first line then every ~16 KiB or 64 lines.
- New indexed reader `read_range(dir, id, since_ms, until_ms)` + follow-from-`since_ms`: binary-search `.idx` (`O(log N)`), `seek` to `byte_offset`, filter strictly within `[since, until]`; never scan from byte 0 when `since_ms` is set. Missing/corrupt `.idx` must rebuild-or-fall-back explicitly (one documented behaviour, never a silent wrong answer).
- CLI `crates/slipfork/src/cli/verbs.rs` `LogsArgs` + `crates/slipfork/src/commands/streams.rs`: accept 0, 1 or many `TARGETS` (`slipfork logs api`, `slipfork logs api worker`; empty = app services, `-a/--all` = services + resources), add `--since <TIME>`, `--until <TIME>` (relative `30s|5m|2h|1d` and RFC 3339/ISO `2026-11-22T12:00:00Z`) and `-t/--timestamps` (human timestamp gutter; `ts_ms` always present in `--json`).
- Daemon `crates/slipfork/src/daemon/lifecycle/logs.rs` passes the window to the world `Logs` request (`crates/slipfork-world/src/control/**` already owned by M3.50.1 — extend the request with `since_ms`/`until_ms` there via coordination if needed; prefer reusing the existing fields) and merges multi-target readers.

# Acceptance
- `pod_log` unit test: N lines spanning timestamps, `.idx` entries every <=64 lines/16 KiB, `read_range(Some(t), None)` seeks (assert it does not read earlier bytes — e.g. by truncating the prefix or counting `seek`s) and returns exactly `[t, until]`.
- CLI: `slipfork logs api worker --since 5m -t` merges both, shows timestamps, and `--json` keeps `ts_ms`; `--since 2026-11-22T12:00:00Z --until ...` narrows; invalid time values exit 2 with `1 line + next:`.
- `cargo test -p slipfork-world`, `cargo test -p slipfork --test critical_cli_surface --test critical_world_journey` (live on the lab host), `guard.py`.,
