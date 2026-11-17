---
id: M3.28.11
title: "Polish: dag plan never silent, quiet world entry, per-component log prefixes, [resources] behind -v"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.28.10]
paths:
  - crates/slipfork/src/cli.rs
  - crates/slipfork/src/main.rs
  - crates/slipfork/src/logs.rs
  - "crates/slipfork-dag/src/**"
  - crates/slipfork/tests/critical_cli.rs
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-17T16:33"
updated: "2026-11-17T16:33"
---

# Goal

Kill the four small lies the owner hit: silent `dag plan`, `getcwd() failed` noise on world entry, unattributed log lines, and internal `[resources]` jargon on the happy path.

# Context

- Fact 6: `slipfork dag plan` printed **nothing** for a project that declares no artifacts (`crates/slipfork/src/main.rs` prints only non-empty loops).
- Fact 7: `slipfork shell` printed `sh: 0: getcwd() failed: No such file or directory` before the prompt; the entry wrapper starts on a host path that only exists inside the world's mount namespace.
- `slipfork logs` interleaves sources with no attribution.
- Fact 8: `[resources] demo: prefer_remote -> local (not_offered)` is a developer diagnostic on a healthy boot.

# Design / Invariants

1. `dag plan` is never silent: zero declared artifacts → one line `no artifacts declared in snow.yml`; a non-empty plan prints the wave table as today. `--json` keeps its empty-array payload.
2. World entry is quiet: the wrapper `chdir`s to a path valid inside the world before `exec`, so `getcwd() failed` never appears in `shell`, `exec`, `db`, or `hook run`. Interactive `shell` prints one honest line: `world rootfs is the app stage (no host tools; use slipfork ps / logs / open)`.
3. `logs` prefixes every line with `<component|hook>:` by default when more than one source is selected; a single named target stays unprefixed; `--json` unchanged.
4. `[resources] …` lines print only with `-v/--verbose` (global flag) or via `--json`. A healthy boot prints the actionable block only.
5. Delete, never deprecate: no env switch, no second spelling, no dead rendering branch left behind.

# Acceptance

```console
$ printf 'project = "ux"\n' > /tmp/opencode/ux-plan.toml && cargo run -q -p slipfork -- dag plan --file /tmp/opencode/ux-plan.toml
no artifacts declared in snow.yml
$ cargo run -q -p slipfork -- dag plan --file /tmp/opencode/ux-plan.toml --json | grep -q '"artifacts": \[\]' && echo json-empty
json-empty
$ cargo run -q -p slipfork -- --help | grep -q -- '--verbose' && echo verbose-flag
verbose-flag
$ cargo test -p slipfork --lib logs
... test result: ok
$ cargo test -p slipfork-dag --test critical_dag
... test result: ok
$ ! grep -rn 'getcwd() failed' crates/ --include=*.rs && echo no-getcwd-noise
no-getcwd-noise
$ cargo test -p slipfork --test critical_cli -- --test-threads=1
... test result: ok
$ make check
... exit 0
```

- Both the human line and the `--json` empty arrays are asserted; neither may regress to silence.
- `make check` exits 0 on a non-Btrfs dev host; world-entry assertions are the live node's job.
