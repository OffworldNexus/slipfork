---
id: M3.40.9
title: "env/secret verbs over the 0600 vault, headless-safe open, and OSPP auth pairing"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.40.8]
paths:
  - "crates/slipfork/src/**"
  - "crates/slipfork/tests/**"
  - crates/slipfork/Cargo.toml
  - Cargo.toml
  - Cargo.lock
  - "specs/02-cli-ux.md"
  - "specs/03-under-the-hood.md"
  - "specs/05-ospp-v1.md"
  - TESTING_PLAN.md
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-21T14:29"
updated: "2026-11-21T14:29"
---

# Goal
Ship `slipfork env [service] [--reveal]`, `slipfork secret list|set|unset`, headless-safe `slipfork open [service]`, and `slipfork auth [<url>]` on the injected host values and the `M3.40.8` daemon.

# Context
- `HostEnv` (extended by `M3.40.8` with `hostname`, `display`, `ssh`, `opener`, `state_dir`) is the only ambient surface; nothing below reads env or `/proc`.
- `env`: resolve runtime variables from the evaluated `Forkfile` (`world_plan`/visitors) plus `slipfork_ospp::FileSecretVault` at `<state_dir>/secrets.json` (0600). Secrets render `[REDACTED]` unless `--reveal` **and** stdout is a real TTY; `--json` keeps them redacted under the same gate.
- `secret list|set|unset`: same vault; `set` never echoes the value; `unset` removes exactly one key; components/keys come from the evaluated `Forkfile`.
- `open [service]`: when `!display || ssh`, print the bold-cyan URL plus `next: open it on a machine with a browser`; otherwise spawn the injected `opener` and report failure as one line + one `next:`.
- `auth [<url>]`: precedence `--ospp-endpoint` > positional > `<state_dir>/ospp.json`; `FileDeviceKeyStore` at `<state_dir>/device-key.json` (0600); `OsppClient::pair(hostname, phase, on_handle)` with a live phase renderer; open the `#handle=` URL through the injected opener or print it when headless; print the settled `{account, machine, endpoint}` + `next: slipfork up`; with no endpoint and no record, fail with `this daemon has no OSPP endpoint` and name the flag.

# Acceptance
- `cargo test -p slipfork` + `--test critical_cli_surface`: help text, `--json` shape, and exactly one `next:` per error for all four verbs.
- A world-free daemon test proves `env`/`secret` round-trip through a real `0600` vault at an injected `<state_dir>` (list → set → list → unset → list) and that `--reveal` is refused on a pipe.
- `auth` is tested against the crate's real `127.0.0.1:0` loopback OSPP reference server (no mocks) for the success path (writes `ospp.json` public record + device key `0600`) and a typed failure path with no endpoint.
- `open` on a headless (`ssh`/no display) host prints the URL and exits 0; with an injected fake-free `opener` (a real executable script in `/var/tmp`) it is invoked with the URL.
- ANSI purity: zero `\x1b[` for `NO_COLOR`, `TERM=dumb`, pipe, and `--json`.
- `cargo clippy -p slipfork --all-targets -- -D warnings`; `critical_guard.py`; `guard.py`.
