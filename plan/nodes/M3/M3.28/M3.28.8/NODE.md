---
id: M3.28.8
title: "slipfork restart [--watch] and the actionable up block with the loud :80 line"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.28.7]
paths:
  - crates/slipfork/src/cli.rs
  - crates/slipfork/src/restart.rs
  - crates/slipfork/src/lib.rs
  - crates/slipfork/src/main.rs
  - crates/slipfork/tests/critical_cli.rs
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-17T16:33"
updated: "2026-11-17T16:33"
---

# Goal

Add `slipfork restart [--watch]` (the edit-and-apply reflex the owner typed) and turn `up`/`fork` output into one actionable block that also says, loudly, when `:80` is not bound.

# Context

Fact 3 (`restart` was `unrecognized subcommand`) and fact 12 (a re-run `up` printed the same URL with no verdict). Fact 1: with only an ephemeral ingress, `http://demo.localhost` is refused and nothing said why. `M3.28.4` locks the canonical semantics (`restart` re-runs hooks + app process + containers; `up` stays idempotent and reports `changed`), `M3.28.7` ships the status renderer and the daemon payload.

# Design / Invariants

1. `restart [--world <WORLD>] [--watch] [--json]` makes one `DaemonControl::restart` round trip (never `down` then `up` composed in the CLI). `--watch` re-runs after a debounce when the worktree changes; if a watcher cannot be injected, it fails closed rather than silently polling once.
2. Output names what changed: `[restarted] api, postgres` or `[no change]`; then the same block `up` prints. `--json` carries the daemon summary including `"changed"`.
3. `up`/`fork` block: `world`, `state` (running/changed), `app` (origin, port-less at 80), `db` (`slipfork db`), `logs` (`slipfork logs`), `next` (`slipfork open` / `restart`), then **one** compact lineage line built only from the daemon's `WorldLineage`: `lineage  rootfs <digest12> cached · postgres:18 hit · seed <digest12> rebuilt 2.1s · base <key12> sealed 10ms` — `-` for any unknown fact, never `0`, never an invented duration, and the same facts in `--json`. `-v/--verbose` adds the per-node detail (every service's cache verdict, digest and millis); without `-v` the line stays single. No bare `[origin]` line alone, and no part of this is computed in the CLI.
4. When the daemon reports an ingress port other than 80, print exactly ONE line to stderr (never stdout, never under `--json`): `slipfork: ingress is :<port>, not :80 — \`slipfork daemon install\` binds 127.0.0.1:80 + [::1]:80 via systemd, so <project>.localhost works without a port`.
5. `restart` on a never-booted world fails closed naming `slipfork up`; a `Failed` world must restart (rescue path). Rendering lives in `crates/slipfork/src/restart.rs` as pure functions.

# Acceptance

```console
$ cargo run -q -p slipfork -- restart --help | grep -q -- '--watch' && echo watch-flag
watch-flag
$ cargo test -p slipfork --lib restart
... test result: ok
$ cargo test -p slipfork --test critical_cli -- --test-threads=1
... test result: ok
$ ! grep -rn '/proc\|read_to_string("/proc' crates/slipfork/src && echo daemon-only
daemon-only
$ cargo clippy --workspace --all-targets -- -D warnings
    Finished
```

- `critical_cli` proves `restart` applies an edited `snow.yml` env and that the block prints `[restarted]`/`[no change]` truthfully.
- The loud ingress line appears exactly once on a `:0` daemon and never under `--json` (asserted).
