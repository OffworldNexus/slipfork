---
id: M3.32.4
title: "Clean-sheet 0.1.0 CLI surface (dx-cli-0.1.0.md §1-§3) & @slipfork/opencode status --json sync"
kind: epic
status: done
priority: 3
depends_on: [M3.32.3]
paths: ["crates/slipfork/**", "packages/opencode-slipfork/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-19T13:02"
updated: "2026-11-19T21:48"
---

# Goal

Implement every command, flag, visual rule, and `--json` document in `docs/design/dx-cli-0.1.0.md` §1–§3, deleting `slipfork list` and the pre-`0.1.0` surface, and move `@slipfork/opencode` onto `status --json`.

# Context

- Command enum: `crates/slipfork/src/cli.rs:32` (`about`), `:100` (`Commands`), `:180` (`List` — **delete**), `:297` (`Resolve` — replace with `env`), plus `Up:114`, `Down:128`, `Restart:135`, `Ps:141`, `Open:147`, `Fork:156`, `Drop:174`, `Prune:195`, `Exec:207`, `Shell:219`, `Logs:225`, `Db:248`, `Redis:256`, `Run:282`, `Doctor:105`. Add `-F/--features`, `--no-default-features`, and the standard short flags (`-w`, `-f`, `-n`, `-y`, `-F`, `-s`, `-r`, `-p`, `-v`, `-q`); keep `--json`/`-v`/`--state-dir` global.
- Presenters to rewrite: `crates/slipfork/src/status/mod.rs`, `status/table.rs`, `status/lineage_table.rs` (lineage hashes only behind `-v`, zero `slipforkd <pid>` header, `○ No worlds running` exit `0`), `crates/slipfork/src/ps.rs` (scope to `cwd`'s world), `src/ui.rs` (dynamic ANSI-stripped widths, bold/cyan URL, green `running`, red `failed`), `src/commands/{inspect.rs,control.rs,process.rs,boot.rs,doctor.rs,secret.rs,forward.rs,daemon.rs}`, `src/commands/snow.rs` -> `src/commands/env.rs` (delete `snow resolve`), `src/logs.rs`, `src/open.rs` (`$SSH_CONNECTION` auto-remote + `ssh -N -L` line), `src/restart.rs`, `src/daemon/*` (`up`/`fork` auto-spawn `slipforkd`; `restart` keeps the Btrfs subvolume; `down --all`; failing closed on unknown `-w`).
- Behaviours with no current implementation: `slipfork run <task> [args...]`, per-service `shell [svc]` with `(slipfork:<world>/<svc>)` prompt and zero stderr lecture, `exec [-s <svc>] <cmd...>` (cwd-aware service auto-selection, `--` optional), `env [-s] [--reveal]` (`[REDACTED]` / `null`), piped non-TTY `db`/`redis`, hidden TTY prompt on `secret set`, 3-row verdict-first `doctor`, `drop` dirty-worktree guard + `-f`, inline last-15-lines output on `setup`/`pre-deploy` failure citing `Forkfile:<line>`.
- Plugin: `packages/opencode-slipfork/src/cli.ts:16` (`list: "list"`), `:42-68` (`buildListArgs`), `:119-138` (list payload decode) -> `status --json` `{"ok":true,"worlds":[...]}`; update `coordinator.ts:69,143,227` and `index.ts:76-77`; keep `tests/coordinator.critical.test.ts` green (protected).
- `--json` documents must match the §3 table byte-for-byte in field names; failures are `{"ok":false,"code","error","next"}` and stderr is `error: <1 line>\n  next: <remedy>`.

# Acceptance

- `cargo test -p slipfork -p slipfork-wire --all-targets` exits `0`.
- `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all -- --check` exit `0`.
- `! grep -rn 'List,\|snow resolve\|slipfork list' crates/slipfork/src packages/opencode-slipfork/src` exits `0`.
- PTY/UI tests assert zero `\x1b[` under `NO_COLOR`/piped/`--json`, and `--json` shape tests cover bare `slipfork`/`status`, `ps`, `up`, `logs`, `env`, `open`, `doctor`, `secret list`, `prune`.
- `cd packages/opencode-slipfork && bun test` exits `0`.
- Write NO `RESULT.md`.
