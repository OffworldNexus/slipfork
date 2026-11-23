---
id: M3.50.9
title: "CLI design system: multi-tone palette, clap Styles, plain-English 4-row doctor (zero jargon)"
kind: task
status: done
owner: implementer
priority: 5
depends_on: [M3.50.2, M3.50.8]
paths:
  - "crates/slipfork/src/ui/**"
  - "crates/slipfork/src/doctor/**"
  - crates/slipfork/src/cli/verbs.rs
  - "crates/slipfork/src/commands/**"
  - crates/slipfork/tests/doctor_surface.rs
  - crates/slipfork/tests/status_surface.rs
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-22T13:27"
updated: "2026-11-22T13:40"
---

# Goal
One palette + token API for every CLI surface: vibrant, scannable, zero kernel jargon; `NO_COLOR`/`TERM=dumb`/pipes/`--json` still emit 0 `\x1b[` bytes.

# Context
- Today `crates/slipfork/src/ui/paint.rs` only tints a string; `doctor/mod.rs` colors just the leading check mark and prints developer jargon (`unprivileged user + netns available`, `crun 1.14.1, :80 .localhost ingress active`); `clap` has no `#[command(styles = ...)]` so `--help` is monochrome.
- Introduce `ui::Palette { color: bool }` (or a `Role` enum) with methods: `label` (bold), `path` (bold cyan), `url` (bold cyan underlined), `value` (bold white), `dim`, `kind` (cyan), `status(state)` (green running/ready, cyan cached/up-to-date, yellow built/pulled/orphaned, bold red failed/stopped), `command` (bold green), `error` (bold red), `icon_ok/bad/info`. Every call is a no-op passthrough returning plain text when `!color`.
- `doctor`: 4 plain-English rows and zero jargon — header bold green `✓ Ready` / bold red `✗ <N> issue(s) found`; rows keyed Storage (`~/demo` bold cyan path, `on` dim, `Btrfs` bold, `instant copy-on-write forks ready` dim), Containers (`crun <ver>` bold, `rootless container sandboxing enabled` dim, AppArmor-blocked case says `rootless container sandboxing blocked by AppArmor` + `next:`), Daemon (`running` green, `background user service active` dim), Domains (`http://*.localhost` bold cyan, `local domain routing active` dim). `--json` keeps `{ready,checks}` and adds the structured fields without breaking `name/ok/detail/next`.
- `clap`: `#[command(styles = Styles::styled() ...)]` — bold cyan headers, bold green usage/literal, yellow placeholders, cyan valid values, on the root `Cli` so every verb inherits.
- Palette also lands `env`/`secret` (`KEY` bold cyan, `=` dim, value white, `[REDACTED]` dim yellow) and `logs` gutter (distinct bold per-service colors, dim `│`, dim `-t` stamps, dim cyan dividers) — M3.50.8 owns the log plumbing; consume its renderer.

# Acceptance
- `doctor` on a colour PTY: 4 rows exactly named Storage/Containers/Daemon/Domains, `~/demo` and `Btrfs` and `crun <ver>` and `http://*.localhost` visibly bold/cyan; strings `netns`, `:80`, `ingress`, `unprivileged user` absent.
- `slipfork --help` and `slipfork shell --help` on a colour PTY contain SGR styling; under `NO_COLOR=1`/pipe/`--json` zero `\x1b[`.
- New `crates/slipfork/tests/doctor_surface.rs`: real PTY (`openpty`/`script`) asserts the 4 rows, the highlighted roles (SGR present), zero jargon, and the plain path emits none.
- `cargo test -p slipfork --test doctor_surface`, `--test critical_cli_surface`, `--test ansi_matrix`, `guard.py`.,
