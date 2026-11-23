---
id: M3.41.6
title: "Real OpenRC :80 listen mode and non-root handoff guard"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - crates/slipfork/src/main.rs
  - "crates/slipfork/src/daemon/**"
  - crates/slipfork/src/install.rs
  - crates/slipfork/src/world/mod.rs
  - "crates/slipfork/tests/**"
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-21T17:07"
updated: "2026-11-21T17:12"
---

# Goal
Make the Alpine OpenRC `:80` ingress real and make the handoff refuse to run as root, so packaged units cannot silently proxy every user.

# Context
- `docs/install.sh`/`packaging/alpine/slipfork-ingress.initd` call `slipfork __ingress --listen 127.0.0.1:80`, but `main.rs:126` ignores argv and `daemon/handoff.rs:forward` only reads fd 0 (systemd `StandardInput=socket`). The OpenRC service therefore never accepts.
- `packaging/systemd/slipfork-ingress@.service` is static and cannot know the user uid; `slipfork daemon install --ingress` rewrites it with `User=<uid>` + `XDG_RUNTIME_DIR=/run/user/<uid>` (`install.rs`). A root-run handoff would connect as root to a user socket.
- Keep one `IngressHandoff` path: the daemon still makes every routing decision; this mode only accepts and forwards one fd per connection.

# Acceptance
- `slipfork __ingress --listen 127.0.0.1:80` binds, accepts loopback connections, and forwards each accepted descriptor to `<runtime_dir>/daemon.sock`; unknown flags fail with one `error:` + `next:` line.
- `__ingress` refuses `geteuid()==0` with a clear `error:`/`next: sudo slipfork daemon install --ingress` (the templated unit then runs as the invoking user).
- A real loopback test (disk tempdir, UnixListener + TCP connect) proves one accepted connection is handed off; no mocks.
- `cargo test -p slipfork`, `cargo clippy -p slipfork --all-targets -- -D warnings`, `cargo fmt --all -- --check` green.
