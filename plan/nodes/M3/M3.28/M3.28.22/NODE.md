---
id: M3.28.22
title: "Quiet world entry: the entry argv must start inside the world so the shell's first getcwd succeeds"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.28.10]
paths:
  - "crates/slipfork-engine/src/**"
  - "crates/slipfork-engine/tests/**"
  - "crates/slipfork-edge/tests/critical_edge.rs"
  - crates/slipfork/src/daemon.rs
  - crates/slipfork/src/main.rs
  - "crates/slipfork/tests/**"
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-18T04:15"
updated: "2026-11-18T04:45"
---

# Goal

Kill `sh: 0: getcwd() failed: No such file or directory` on world entry. The entry command's `cwd` is a host path that only exists inside the world's mount namespace, so the shell complains before doing anything. Fix it at the source: the entry argv must start in a directory that is valid **inside** the world.

# Context

Owner fact 7: `slipfork shell` printed `sh: 0: getcwd() failed: No such file or directory` before the prompt. `M3.28.11`'s attempt 1 traced it: the failing argv is built in `crates/slipfork-engine/src/btrfs_engine.rs:434-446` + `service.rs:183-195`, and `WorldEntryCommand.cwd` (`crates/slipfork/src/daemon.rs:1975-1991`) is pinned by the conformance suite (`conformance.rs:163-169`). No `chdir` in `main.rs` can silence it.

# Design / Invariants

1. The entry command starts the shell **inside the world's rootfs** at a directory that exists there (the app stage's working directory, e.g. `/` or the component's `cwd` mapped inside), so the shell's first `getcwd(2)` succeeds. Implement it where the argv is built (engine/service), not by wrapping the user's shell in a `cd`.
2. `shell`, `exec`, `db`, `redis`, and `hook run` all inherit the fix — one entry-command builder, no per-verb special case.
3. Interactive `shell` prints exactly one honest line: `world rootfs is the app stage (no host tools; use slipfork ps / logs / open)`; non-interactive `exec` stays silent.
4. The conformance suite pins the new contract (entry `cwd` exists in-world) and the old host-path expectation is deleted, not kept as an alternative.
5. No new dependency, no `unsafe`, no global state; the engine still owns the argv.

# Acceptance

```console
$ cargo test -p slipfork-engine --test critical_engine -- --test-threads=1
... test result: ok
$ cargo test -p slipfork --test critical_ospp_daemon -- --test-threads=1
... test result: ok
$ ! grep -rn 'getcwd() failed' crates/ --include=*.rs && echo no-getcwd-noise
no-getcwd-noise
$ cargo clippy --workspace --all-targets -- -D warnings
    Finished
```

- `critical_engine`'s entry assertion proves the command's `cwd` resolves **inside** the world (a path that exists in the rootfs), and `critical_ospp_daemon`'s `shell`/`exec` transcript shows no `getcwd` line.
- `RESULT.md` records the before/after argv and the live note that `M3.28.13` re-checks it on the droplet.
