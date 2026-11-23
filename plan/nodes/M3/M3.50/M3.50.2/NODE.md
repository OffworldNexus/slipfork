---
id: M3.50.2
title: "CLI+daemon: required shell <target>, delete db/redis, logs <image>"
kind: task
status: done
owner: implementer
priority: 1
depends_on: [M3.50.1]
paths:
  - "crates/slipfork-forkfile/src/visit.rs"
  - "crates/slipfork/src/cli/**"
  - "crates/slipfork/src/commands/**"
  - crates/slipfork/src/surface.rs
  - crates/slipfork/src/daemon/mod.rs
  - crates/slipfork/src/daemon/client.rs
  - crates/slipfork/src/daemon/frames.rs
  - crates/slipfork/src/daemon/wire.rs
  - crates/slipfork/src/daemon/rpc.rs
  - crates/slipfork/src/daemon/ps.rs
  - crates/slipfork/src/daemon/view.rs
  - crates/slipfork/src/daemon/lifecycle/stream.rs
  - crates/slipfork/src/daemon/lifecycle/logs.rs
  - crates/slipfork/tests/critical_cli_surface.rs
  - crates/slipfork/tests/critical_world_journey.rs
  - crates/slipfork/tests/critical_daemon.rs
  - crates/slipfork/tests/manpage_drift.rs
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-22T12:41"
updated: "2026-11-22T13:18"
---

# Goal
`slipfork shell <target>` is the only interactive entry point: required `<target>`, raw-mode real-PTY attach with `SIGWINCH` forwarding, kind dispatch (`service`|`image` -> bash then sh with a `(<world>:<target>)` PS1, `postgres`|`database` -> `psql`, `redis` -> `redis-cli`, `s3`/unknown -> exact 1-line `next:` refusal on stderr exit 1), `slipfork db` and `slipfork redis` deleted, and `slipfork logs <image>` works with zero pods running.

# Context
- Surface: `crates/slipfork/src/cli/verbs.rs:135-159` — `Shell { target: Option<String> }` becomes required (`target: String`, drop the `conflicts_with`/optional docs), and the `Db`/`Redis` variants at `:149-159` are deleted.
- Dispatch: `crates/slipfork/src/cli/dispatch.rs:78-93` — delete the `Command::Db`/`Command::Redis` arms; `Shell` builds the new args.
- Commands: `crates/slipfork/src/commands/streams.rs:102-175` — today `shell`/`exec`/`db`/`redis` all funnel `exec_like` -> daemon method `exec`. Keep `exec` there; add a `shell()` that resolves the `<target>` kind server-side (never re-parse `Forkfile` in the CLI), uses daemon method `shell` on a TTY and writes raw bytes, and uses piped `psql -X -q -v ON_ERROR_STOP=1` / `redis-cli` / `bash -lc` on a pipe. Delete `db()`/`redis()` at `:127-150`.
- Raw mode: new `crates/slipfork/src/commands/pty.rs` with `RawModeGuard` (`enable_raw_mode`, `Drop` -> `disable_raw_mode` + show cursor, panic-safe) and `SIGWINCH` -> `TIOCSWINSZ` forwarding of `(cols, rows)`. Do NOT edit `crates/slipfork/src/main.rs` (owned by M3.50.4): construct the guard from `commands/streams.rs`.
- Daemon: `crates/slipfork/src/daemon/mod.rs:241,252` has `exec`/`logs`; add `shell` with `SCM_RIGHTS` fd delivery. `crates/slipfork/src/daemon/client.rs:146` `call_duplex` base64-pumps stdin; add a PTY variant that `recvmsg`s the master fd and `poll`s it against `stdout`. Frames in `crates/slipfork/src/daemon/frames.rs`, wire in `wire.rs`.
- `PS1`: set inside the pod via the tmpfs process spec env (`PS1=(<world>:<target>) \\w$ `), never by rewriting user bytes downstream.
- Hygiene: zero `\\x1b[` under `NO_COLOR`/non-TTY/`--json`; PTY output must bypass the gutter `Renderer` entirely (raw pass-through); never print daemon `log` frames to the terminal.
- `logs <image>`: `LogsArgs`/`TargetArgs` must resolve image + resource ids and read `<store>/logs/<world>/<id>.log` when `<pods_dir>` is absent (daemon `logs` already reaches `core()` at `crates/slipfork/src/daemon/mod.rs:257-266`).
- M3.49 explicitly supersedes `crates/slipfork/tests/critical_cli_surface.rs:125-140,242-265` (`db`, `redis`, bare `shell`): rewrite those assertions to the new contract. Extend, never delete, the shell/exec/logs journey in `crates/slipfork/tests/critical_world_journey.rs`.
- `packaging/man/slipfork.1` and `docs/**` belong to M3.50.5; `manpage_drift.rs` must pass once that slice regenerates the man page.

# Acceptance
- `cargo test -p slipfork --test critical_cli_surface`: missing `<target>` exits `2` on stderr; no `db`/`redis` subcommands; `completions` non-empty.
- `cargo test -p slipfork --test critical_world_journey`: real PTY `shell api`/`shell pg`/`shell cache` -> `tty` exits `0`, zero `can't access tty`, `(<world>:<target>)` prompt, resize applied, cooked mode restored; `shell uploads` and `shell apii` print the exact 1-line refusals on stderr with exit `1`.
- Piped: `printf 'select 1;' | slipfork shell app_db` exits `0`; same for `cache`.
- `cargo test -p slipfork`, `cargo clippy --workspace --all-targets -- -D warnings`, `uv run --no-project tools/guard.py`.
