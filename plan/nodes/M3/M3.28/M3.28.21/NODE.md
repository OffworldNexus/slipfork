---
id: M3.28.21
title: "daemon install owns the socket handover for any holder (version-agnostic, ownership-proven), so the harness keeps no daemon lifecycle"
kind: task
status: review
owner: implementer
priority: 1
depends_on: [M3.28.6]
paths:
  - crates/slipfork/src/daemon_unit_install.rs
  - crates/slipfork/src/main.rs
  - crates/slipfork/src/lib.rs
  - "crates/slipfork/tests/**"
  - "crates/slipfork-engine/src/linux_cow.rs"
  - "crates/slipfork-engine/tests/**"
attempts: 3
max_attempts: 3
escalation: none
created: "2026-11-18T00:37"
updated: "2026-11-18T01:05"
---

# Goal

Make `slipfork daemon install`'s handover **own the socket regardless of who holds it**, so provisioning never needs a second daemon-lifecycle implementation. `daemon install` must stop the units it owns, shut the daemon down when it answers, and — when the holder speaks a different/older protocol — still converge by proving ownership of the socket file, with one actionable line in every failure.

# Context

`M3.28.12` proved on the DO VM that installing over a **stale, auto-spawned** `slipforkd` fails:

```
slipfork: cannot read the running slipforkd's ingress before installing:
daemon control protocol error: missing field `lineage` — re-run to converge
```

The harness worked around it with `render_daemon_handover` (`systemctl show/stop`, `ss -xlpnH` LISTEN-holder discovery, SIGTERM, bounded wait) — a **second daemon lifecycle** beside `DaemonInstaller::release_running_daemon`, which is a `review-ioc` `SPEC_BLIND_SPOT`. Pre-`1.0`, upgrading from a pre-release binary is our own upgrade path and it must converge.

# Design / Invariants

1. `release_running_daemon` becomes version-agnostic, in this order, each step fail-closed:
   a. stop `slipforkd.socket` / `slipforkd.service` when those units exist and are active (the installer owns the names it writes — reuse them, do not hardcode a second copy);
   b. if `<runtime_dir>/daemon.sock` still answers, send `DaemonRequest::Shutdown` and await the socket's disappearance (bounded);
   c. if the socket answers but the protocol is unrecognised (foreign/older build), verify the socket file's owner via `SO_PEERCRED` on a connected socket and that the peer is a `slipforkd` (compare `/proc/<pid>/exe` basename — this read lives in the `UnitInstaller`/process adapter, never in dispatch logic), then `SIGTERM` it, await, and `SIGKILL` only after the bounded grace;
   d. if it cannot prove ownership, fail closed with one line naming the socket path, the pid, and the exact manual command — **never** kill an unowned process, never proceed with a held socket.
2. The whole sequence is one function behind the existing narrow `UnitInstaller` verbs (`stop_units`, `signal_pid`); the install flow stays pure over the injected installer + clock, and `slipforkd`'s own lifecycle is not re-implemented in `src/main.rs` dispatch arms.
3. Idempotent: no holder → zero effects; second run prints `[ok] unchanged`.
4. `provision`/harness side: `tools/do_vm.py` drops `render_daemon_handover` entirely and calls `slipfork daemon install …` (that cleanup is `M3.28.12`'s, dependent on this node).
5. Tests (recording installer + a fake socket): shutdown path, foreign-protocol path, unowned-holder refusal, and "no holder ⇒ no signals". No real `kill` in tests.

# Acceptance

```console
$ cargo test -p slipfork --lib daemon_unit_install
... test result: ok
$ cargo test -p slipfork --test critical_cli -- --test-threads=1
... test result: ok
$ ! grep -rn 'ss -xlpnH\|render_daemon_handover' tools/ && echo harness-has-no-lifecycle
harness-has-no-lifecycle
$ cargo clippy --workspace --all-targets -- -D warnings
    Finished
```

- The unit tests prove: a holder that answers `Shutdown` is released without a signal; a foreign-protocol holder owned by `slipforkd` is terminated after the grace (`Terminate` then `Kill`, via a `pidfd` so a recycled pid is never signalled); an unowned/unprovable holder is refused with one line naming socket, pid and `sudo kill` and **zero** signals; no holder ⇒ zero writes and zero signals (one no-op `systemctl stop`). The runtime dir is always absolute (`Layout` guarantees it), so a relative path is not a case this sequence defends.
- `RESULT.md` records the recording-installer transcripts and the VM convergence that `M3.28.12` could only achieve with harness logic.
