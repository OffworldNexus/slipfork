---
id: M3.50.1
title: "world_init: real-PTY ExecPty + persistent per-id logs"
kind: task
status: done
owner: implementer
priority: 1
depends_on: [M3.49]
paths:
  - Makefile
  - "crates/slipfork-world/src/fdpass.rs"
  - "crates/slipfork-world/src/pod.rs"
  - "crates/slipfork-world/src/control/**"
  - "crates/slipfork-world/src/pod_log.rs"
  - "crates/slipfork-world/src/layout.rs"
  - "crates/slipfork-world/src/bin/world_init/shared.rs"
  - "crates/slipfork-world/src/bin/world_init/stream/**"
  - "crates/slipfork-world/src/bin/world_init/control.rs"
  - "crates/slipfork-world/src/bin/world_init/respond.rs"
  - "crates/slipfork-world/src/bin/world_init/pod/launch.rs"
  - "crates/slipfork-world/tests/pod_control_lab.rs"
  - "crates/slipfork-world/Cargo.toml"
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-22T12:41"
updated: "2026-11-22T12:58"
---

# Goal
`slipfork-world-init` hands a live kernel PTY (`/dev/pts/N` master) to the caller over `SCM_RIGHTS` and resolves every `logs <target>` request from a persistent `<store>/logs/<world>/<id>.log`, including when zero pods are running (failed cold `up`, after `down`, cloned `fork`).

# Context
- Wire types: `crates/slipfork-world/src/control.rs` / `control/**` — add `Request::ExecPty { id, pod, argv, cols, rows }` plus a resize frame (`InitFrame::Resize` or `{"method":"resize","cols":N,"rows":N}`), with `FrameLines` support. Keep `Request::Exec` for piped `slipfork exec`.
- Dispatch: `crates/slipfork-world/src/bin/world_init/control.rs:117-130` (`Request::Exec -> serve_exec`); add the `ExecPty` arm as a thread spawn modelled on `Logs` at `:102-116`.
- Exec today: `crates/slipfork-world/src/bin/world_init/stream/exec.rs:22-90` spawns `crun exec --process <spec>` with piped stdio; `crates/slipfork-world/src/bin/world_init/stream/lines.rs` builds that mode-`0600` tmpfs `process.json` (`TempProcessSpec::write`). For PTY: emit `"terminal": true` in the spec and spawn `crun exec --tty --console-socket <sock>` (verify on 165.232.86.174 whether `--process` may be combined with `--console-socket`; if it is rejected, use `--tty --console-socket` and carry identity/env via the spec only when supported — never lose identity uid/gid). Receive the master fd via `SCM_RIGHTS` and pump it both ways.
- Identity rationale that must survive: comment at `exec.rs:49-53` (crun exec defaults to uid 0 in the identity-mapped rootless userns -> `setresgid(0): EINVAL`, and drops `config.json` env).
- FD passing already exists and is shared with `slipfork-ingress`: `crates/slipfork-world/src/fdpass.rs:60,123` (`send_fd`/`recv_fd`). Reuse, never fork a copy.
- Logs today: `crates/slipfork-world/src/pod_log.rs` writes `<pods>/logs/<id>.log`; `crates/slipfork-world/src/layout.rs` owns layout paths. Make `<store>/logs/<world>/<id>.log` canonical (spec 03 §1/§4), resolving `<store>` from the world layout value held by the owning struct (no `std::env` in this crate), and keep `<pods>/logs/*` only as a compatibility symlink or delete it. Expose `pod_log::append(&logs_root, id, bytes)` so the image builder (M3.50.3, `crates/slipfork/src/world/runtime/images.rs`) can write build logs.
- Log pumps to preserve: `crates/slipfork-world/src/bin/world_init/pod/launch.rs`. Reply plumbing: `crates/slipfork-world/src/bin/world_init/respond.rs:170-200`.

# Acceptance
- `cargo test -p slipfork-world` and `cargo test -p slipfork-world --test pod_control_lab` stay green (do not weaken `pod_control_lab.rs:196-210`).
- New lab test in `crates/slipfork-world/tests/pod_control_lab.rs`: boot a real pod, `ExecPty` a `tty` command, assert the returned fd is real and the device is `/dev/pts/*`; apply `TIOCSWINSZ` and assert `stty size` changes through the same fd.
- Log durability: write a line, stop the pod, assert `<store>/logs/<world>/<id>.log` is still served by a `Logs` request.
- `cargo test -p slipfork-world`, `cargo clippy -p slipfork-world --all-targets -- -D warnings`, `uv run --no-project tools/guard.py`.
