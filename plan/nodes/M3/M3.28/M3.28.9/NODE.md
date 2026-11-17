---
id: M3.28.9
title: "slipfork ps [--json] and slipfork open [--remote] [--json]"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.28.8]
paths:
  - crates/slipfork/src/cli.rs
  - crates/slipfork/src/ps.rs
  - crates/slipfork/src/open.rs
  - crates/slipfork/src/lib.rs
  - crates/slipfork/src/main.rs
  - crates/slipfork/src/user.rs
  - crates/slipfork/tests/critical_cli.rs
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-17T16:33"
updated: "2026-11-17T16:33"
---

# Goal

Add `slipfork ps [--json]` (the world's component tree) and `slipfork open [--remote] [--json]` (put the developer in front of the world on the host or from their laptop).

# Context

Facts 4 (`ps` typed at the host shell), 7 (no `ps` in the app rootfs) and 9 (the owner hand-wrote `ssh -L 40645:localhost:40645`; no recipe existed). Sources: `M3.28.3`'s `DaemonControl::world_processes` rows, `M3.28.4`'s filled pid/uptime/port, `DaemonStatus` ingress addrs, and the existing `trait BrowserOpener` (`crates/slipfork-ospp/src/pairing.rs:301`).

# Design / Invariants

1. `ps` renders one row per component and `crun` backing service: kind, id, state, pid, port, uptime. `--world` selects like every other command. `--json`: `[{"world","id","kind","state","pid","port","uptime_secs"}]`, stdout only.
2. Zero `/proc`, `/sys`, or log-directory reads in the CLI — every field comes from the daemon. A `None` pid/port renders `-`, never `0`.
3. `open` resolves the world, takes its single origin, and hands it to the existing `BrowserOpener`; headless (no browser capability) fails closed naming `slipfork open --remote`.
4. `open --remote` prints the verified recipe from the daemon's ingress port: `ssh -N -L 8080:127.0.0.1:<ingress> <account>@<this-host>` then `http://<route>.localhost:8080`. `--json`: `{"world","origin","recipe":{"ssh","url"}}`.
5. A unit test must round-trip `Host: <route>.localhost:8080` through `LocalHostRoute::parse` (which strips the port) and show it resolves to the same world — the recipe may never be one the edge rejects. Account and host come from the injected identity/hostname probes wired in `main.rs`; never `$USER`/`$HOSTNAME`.

# Acceptance

```console
$ cargo run -q -p slipfork -- ps --help | grep -q -- '--world' && echo ps-world
ps-world
$ cargo run -q -p slipfork -- open --help | grep -q -- '--remote' && echo remote-flag
remote-flag
$ cargo test -p slipfork --lib ps
... test result: ok
$ cargo test -p slipfork --lib open
... test result: ok
$ cargo test -p slipfork --test critical_cli -- --test-threads=1
... test result: ok
$ ! grep -rn '/proc\|/sys/\|std::env::var("USER")' crates/slipfork/src && echo no-ambient
no-ambient
$ cargo clippy --workspace --all-targets -- -D warnings
    Finished
```

- The `LocalHostRoute::parse` forwarded-port round-trip test is mandatory.
- `ps --json` parses as JSON with no surrounding human text (asserted in `critical_cli`).
