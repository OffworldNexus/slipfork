---
id: M3.40.8
title: "Host injection + slipfork fork/drop (daemon CoW snapshot, git switch, fork boot, live journey)"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.40.5]
paths:
  - "crates/slipfork/src/**"
  - "crates/slipfork/tests/**"
  - crates/slipfork/Cargo.toml
  - Cargo.toml
  - Cargo.lock
  - Makefile
  - "specs/02-cli-ux.md"
  - "specs/03-under-the-hood.md"
  - "specs/05-ospp-v1.md"
  - TESTING_PLAN.md
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-21T14:02"
updated: "2026-11-21T14:37"
---

# Goal
Finish the `specs/02-cli-ux.md` daily surface on top of the `M3.40.5` control plane: `fork`, `drop`, `env`, `secret`, `open`, `auth`, `completions`, plus the streamed `up`/`restart`/`down` UX and the ANSI-purity matrix.

# Context
- `M3.40.4/.5` ship `daemon.sock` (typed NDJSON + `event` frames), world `Dial`/`Exec`/`Logs`, and the renderer/daemon client. Add the new daemon methods there rather than a second socket.
- `fork [name]`: `WorldEngine::fork` CoW snapshot of project + world data, printed as one summary with elapsed (`< 50ms` target), git branch switched, URL `<fork>--<world>.localhost`.
- `drop <name> [-f]`: `WorldEngine::drop_fork`; refuse dirty work without `-f`, with a one-line `next:`.
- `env [service] [--reveal]`: resolved runtime vars from the evaluated `Forkfile` + the `0600` `SecretVault`; secrets print `[REDACTED]` unless `--reveal` on a real TTY.
- `secret list|set|unset`: read/write the same vault at `<state_dir>/secrets.json` (0600), never echo a value on `set`.
- `open [service]`: `$DISPLAY`/`$WAYLAND_DISPLAY` + `SSH_CONNECTION`-aware; on a headless shell print the URL (bold cyan) instead of failing.
- `auth [<url>]`: precedence `--ospp-endpoint` > positional > `<state_dir>/ospp.json`; drives `slipfork-ospp` pairing with a live phase spinner (solving / awaiting approval), prints the settled pair + `next: slipfork up`, fails closed with `this daemon has no OSPP endpoint`.
- `completions <bash|zsh|fish>`: complete every shipped verb and flag.
- `up`/`restart`/`down`: `[step/total]` phases, elapsed, `(up to date)` badges, inline red tail with `Forkfile:<line>` + `next:`; a failed service keeps the world running.
- Terminal hygiene: zero `\x1b[` under `NO_COLOR`, `TERM=dumb`, piped stdout, or `--json`; `--json` emits the same fields as structured objects.

# Acceptance
- `cargo test -p slipfork` + `cargo test -p slipfork --test critical_cli_surface`: every verb has help text, its `--json` shape where the spec defines one, and a real error path with exactly one `next:`.
- `fork`/`drop` live journey against a real daemon + CoW world (`required-features = ["live"]`, fails closed without `SLIPFORK_TEST_ROOT`) reports elapsed and leaves no subvolume.
- The ANSI matrix runs every new verb with `NO_COLOR=1`, `TERM=dumb`, a pipe, and `--json`, asserting zero `\x1b[`.
- `cargo clippy -p slipfork --all-targets -- -D warnings`; `uv run --no-project tools/critical_guard.py`; `uv run --no-project tools/guard.py`.
