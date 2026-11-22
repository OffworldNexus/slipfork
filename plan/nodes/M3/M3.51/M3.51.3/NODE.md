---
id: M3.51.3
title: "Positional [target] on up/down/restart (delete --world) + honest ps with stopped .forks discovery"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.51.1, M3.51.2]
paths:
  - "crates/slipfork-world/src/lib.rs"
  - "crates/slipfork/src/cli/**"
  - "crates/slipfork/src/world/**"
  - crates/slipfork/src/daemon/lifecycle/up.rs
  - crates/slipfork/src/daemon/lifecycle/down.rs
  - crates/slipfork/src/daemon/lifecycle/ps.rs
  - crates/slipfork/src/daemon/lifecycle/stream.rs
  - crates/slipfork/src/daemon/lifecycle/core.rs
  - crates/slipfork/src/daemon/ps.rs
  - crates/slipfork/src/daemon/mod.rs
  - crates/slipfork/src/ps.rs
  - "crates/slipfork/src/status/**"
  - crates/slipfork/src/surface.rs
  - "crates/slipfork/tests/**"
  - "packaging/man/**"
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-22T17:24"
updated: "2026-11-22T17:58"
---

# Goal

Replace the silently ignored `--world` flag with an optional positional `[target]` on `up`/`down`/`restart`, give `restart [target]` real service-vs-world semantics, and make `ps`/`status` honest: no fake orphans, `Forkfile invalid` / `checkout missing` rows, and stopped forks discovered from `<project>/.forks/*`.

# Context

- CLI: `crates/slipfork/src/cli/verbs.rs:104-120` (`Up { world }`, bare `Down`, `Restart { service }`) becomes `Up { target: Option<String> }`, `Down { target: Option<String> }`, `Restart { target: Option<String>, #[arg(short = 's', long)] service: Option<String> }`. Delete `--world` outright — zero alias, zero `#[deprecated]`. Update `crates/slipfork/src/cli/dispatch.rs:64-66` and every `UpArgs {` / `DownArgs {` / `RestartArgs {` site (`grep -rn` across `crates/`).
- Target resolution: owner helper `crates/slipfork/src/world/target.rs` (or extend `world/up.rs`): `<cwd>/.forks/<target>` → that fork's world name; else a known world/fork name from the daemon registry; else a `1 line + next:` remedy error. Pure `(project, world)` resolver, unit-tested.
- Daemon: `crates/slipfork/src/daemon/lifecycle/up.rs:48` `up_plan` ignored `--world` when `cwd` had a live world — key `self.worlds` on the *resolved* project. `lifecycle/down.rs` stops the target's world, not `cwd`'s. `restart`: `target` matching a service id of `cwd`'s live world restarts that service; matching a fork/world restarts that world; `-s/--service` always means a service.
- `ps` honesty: `crates/slipfork/src/daemon/lifecycle/ps.rs:83-100` `WorldView::new` does `fs::read_to_string(project/Forkfile).ok().and_then(compile().ok())`, silently yielding empty services (`orphaned`). Missing project dir → reap stale registry entry + dead supervisor, render one `checkout missing — run slipfork drop <name>` row; `Forkfile` compile failure → one `Forkfile invalid` row while live pods stay `running`.
- Stopped forks: scan `<project>/.forks/*`, append dim `○ <fork>  ~/demo/.forks/<fork>  stopped` rows (dynamic widths, ANSI-stripped measuring) so `slipfork up <fork>` / `drop <fork>` works from `~/demo`. Row types `crates/slipfork/src/ps.rs`, `crates/slipfork/src/daemon/ps.rs:16`; renderers `crates/slipfork/src/status/rows.rs`, `status/model.rs`, `surface.rs`; JSON gains kind/state keys (`stream.rs:201,439` row selection).
- `depends_on M3.51.1 + M3.51.2`: `.1` rewrote `world/up.rs` restart rows and `.2` moved `daemon/mod.rs` discard to `&mut Core`. Re-read both before editing; this slice also touches `daemon/mod.rs`, hence serialization.

# Acceptance

- `cargo fmt --all -- --check` and `cargo clippy -p slipfork --all-targets -- -D warnings`
- `cargo test -p slipfork --test critical_cli_surface` — no `--world` in any help, `[target]` present on `up`/`down`/`restart`
- `cargo test -p slipfork --test status_surface` — stopped-fork rows, `Forkfile invalid`, `checkout missing`
- `cargo test -p slipfork --test critical_host_verbs`
- `grep -rn -- "--world" crates/` prints nothing; `grep -rn "orphaned" crates/slipfork/src/` shows no fake-orphan fabrication path
