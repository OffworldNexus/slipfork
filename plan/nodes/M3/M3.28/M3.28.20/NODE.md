---
id: M3.28.20
title: "Sandbox fix: RestrictAddressFamilies must allow AF_NETLINK so a world boots through the installed unit (+ ensure_daemon on Btrfs hosts)"
kind: task
status: done
owner: implementer
priority: 1
depends_on: [M3.28.6]
paths:
  - "crates/slipfork-edge/src/unit.rs"
  - "crates/slipfork-edge/tests/golden/slipforkd.service"
  - "crates/slipfork-edge/tests/**"
  - crates/slipfork/tests/support/mod.rs
  - crates/slipfork/tests/critical_slug.rs
attempts: 3
max_attempts: 4
escalation: none
created: "2026-11-18T00:08"
updated: "2026-11-18T00:36"
---

# Goal

Two harness-blocking defects that stop a world from booting through the **installed** unit and stop the CLI suite from starting a daemon where it needs one. Both were proven on the DO VM by `M3.28.12`, both are product bugs, not harness bugs.

# Context

1. **Sandbox too tight (`M3.28.6` regression).** `crates/slipfork-edge/src/unit.rs` sets `RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX`. `slipfork-world-init` runs `ip link set lo up`, which needs **`AF_NETLINK`**, so inside the installed `slipforkd.service` every world boot dies with `EAFNOSUPPORT`. Controlled proof from `M3.28.12`'s RESULT: the same engine with the same unshare flags works outside the unit, fails with the restriction, and returns `rc 0` once `AF_NETLINK` is added.
2. **`critical_slug` on a Btrfs host.** `crates/slipfork/tests/support/mod.rs::ensure_daemon` skips starting a daemon on Btrfs hosts, but `snow resolve` is daemon-served, so 6/7 `critical_slug` tests fail with `slipfork: no slipforkd on … — run \`slipfork up\``. Pre-existing at `19658f8`; it is the harness disagreeing with the `M3.28.6` no-auto-spawn rule.

# Design / Invariants

1. Add `AF_NETLINK` to `RestrictAddressFamilies` for **both** `slipforkd.service` and (if it carries one) any world-init unit; keep the list explicit and minimal — no wildcard, no `~`. Update `crates/slipfork-edge/tests/golden/slipforkd.service` in the same commit and assert the exact line in `critical_edge`.
2. Add a regression test that would have caught it: a unit-rendered-service boot (or the closest honest equivalent in `crates/slipfork-edge/tests/**`) proving the sandbox permits the netlink call `ip link set lo up` needs, or an explicit assertion that the rendered `RestrictAddressFamilies` contains `AF_NETLINK` with a doc comment naming the `ip link` requirement.
3. `ensure_daemon` must start a daemon whenever a test invokes a daemon-served command, **regardless of the host filesystem** — the "Btrfs host runs a real `slipforkd` child" case must not silently skip it. If a host genuinely cannot run the daemon, fail closed naming the command that needed it; never let the test proceed and fail later with a misleading `no slipforkd` line.
4. No behaviour change to the product's no-auto-spawn rule (`M3.28.1` R5): the *test harness* starts the daemon explicitly, the CLI still does not.
5. No `#[ignore]`, no skipped assertion, no `#[allow]`.

# Acceptance

```console
$ grep -n 'RestrictAddressFamilies' crates/slipfork-edge/src/unit.rs crates/slipfork-edge/tests/golden/slipforkd.service
... AF_INET AF_INET6 AF_UNIX AF_NETLINK
$ cargo test -p slipfork-edge --test critical_edge
... test result: ok
$ cargo test -p slipfork --test critical_slug -- --test-threads=1
... test result: ok (7 passed)
$ cargo run -q -p slipfork -- daemon unit --only service | grep -q 'AF_NETLINK' && echo netlink-in-unit
netlink-in-unit
$ cargo clippy --workspace --all-targets -- -D warnings
    Finished
```

- `critical_slug` passes on this Btrfs-capable host (7/7) with no `no slipforkd` failures.
- Live proof is `M3.28.13`'s job: a world must boot **through the installed unit** and serve `curl -H 'Host: demo.localhost' http://127.0.0.1/` → `200`.
- `RESULT.md` records the rendered line, the test output, and the `M3.28.12` reproduction this fixes.
