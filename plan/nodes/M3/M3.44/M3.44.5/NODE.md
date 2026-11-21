---
id: M3.44.5
title: "clap 4.6 derive CLI: completions, manpage, logs -a/--all, UiMode & --json schemas"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.44.4]
paths:
  - Cargo.toml
  - Cargo.lock
  - "crates/slipfork/src/**"
  - "crates/slipfork-forkfile/src/dns.rs"
  - "crates/slipfork-forkfile/src/lib.rs"
  - "packaging/**"
  - "scripts/**"
  - "docs/**"
  - README.md
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-21T20:17"
updated: "2026-11-21T20:27"
---

# Goal
Make `clap` 4.6 the only argv parser, flag validator, help/version renderer, completion, and manpage source; delete every hand-rolled parser/help template; ship service-only default `slipfork logs [-f] [-a|--all]` and the exact `--json` schemas behind a single `UiMode`.

# Context
- Delete 100%: `crates/slipfork/src/cli/help.rs` (424), `crates/slipfork/src/cli/parse/{mod,host,lifecycle,meta}.rs` (653 total), any manual verb table/completion string/manpage roff, and hand-rolled help glue in `crates/slipfork/src/render.rs` (11). Replace `crates/slipfork/src/cli/mod.rs` (146) with a derive hierarchy: `#[derive(Parser)] struct Cli` (global `-v`, `--json`, `--color`), `#[derive(Subcommand)] enum Command`, split into `cli/mod.rs` (root + globals <= 200), `cli/verbs.rs`, `cli/logs.rs` (each <= 300).
- Completions/manpage from the same source: `slipfork completions <bash|zsh|fish>` via `clap_complete 4.5.61` off `Cli::command()`; `slipfork(1)` via `clap_mangen 0.2.29`, generated in `packaging/**` (build step in `scripts/**`) and installed by `.deb`/`.rpm`/pacman/APK recipes.
- `slipfork logs [-f] [-a|--all] [target]` (specs/02-cli-ux.md:126,153): app `service` pods only by default; `-a`/`--all` (or explicit target such as `pg`) includes resources. Filter in `crates/slipfork/src/commands/streams.rs` + `daemon/{ps,rpc,frames}.rs`; stay attached across `restart`.
- `UiMode` (TTY vs non-TTY vs `--json`) resolved once in `crates/slipfork/src/main.rs` (249 → <= 200) and passed down; the daemon/command layer never re-probes stdio. `crates/slipfork/src/surface.rs` (293) becomes a thin `clap` → command dispatcher.
- `--json` payloads exactly per specs/02-cli-ux.md:172: `up`/`restart` `{world,url,elapsed_ms,nodes:[{kind,name,state,duration_ms}]}`; `down` `{world,elapsed_ms}`; `fork` `{fork,project,url,elapsed_ms}`; `drop` `{dropped,elapsed_ms}`; `doctor` `{ready,checks:[{name,ok,detail,next?}]}`; `status`/`ps` `{worlds:[...]}`; `logs` `{pod,stream,line,kind}` lines + `{"exit":0}`; `env` `{env:{...}}`; failures `{"error":{code,message,next}}` with zero ANSI.
- Deps (cached, offline-safe): `clap 4.6.8` + `clap_derive` + `clap_complete 4.5.61` + `clap_mangen 0.2.29` in root `Cargo.toml` `[workspace.dependencies]`, inherited by `crates/slipfork` via `workspace = true`.
- Sync `docs/**` + `README.md` help/examples to clap output; `tools/check_docs_honesty.py` must stay green (no unshipped UX). Write no `RESULT.md`.
- `<= 500 LOC`: every `cli/**` file <= 300, `main.rs` <= 200, `verbs.rs` <= 450 (split by verb family if it grows).
- Out of scope: the frozen-pane renderer (`M3.44.6`) — here `UiMode` only gates output shape/ANSI. Zero mocks; tests drive the real binary.

# Acceptance
- `cargo test -p slipfork --test critical_cli_surface` green: non-empty `completions bash|zsh|fish`, `clap_mangen` roff for `slipfork(1)`, clap `--help`/`--version`, exact `--json` schemas including structured error JSON, non-TTY `0 \x1b[` and `0 \r`.
- `cargo test -p slipfork --test critical_host_verbs` green (`logs` service-only default, `-a/--all`, explicit target, `exec` exit-code forwarding, `doctor` rows) and `cargo test -p slipfork --test critical_daemon --test critical_fmt_migrate` green.
- `rg -l 'usage: slipfork|fn parse_args' crates/slipfork/src` → no hand-rolled parser survivors.
- `uv run --no-project tools/check_docs_honesty.py`, `cargo clippy --workspace --all-targets -- -D warnings`, `uv run --no-project tools/guard.py` all exit 0.
