---
id: M3.188
title: "fork from a fork fails 'root world has no ledger row' even after up: Idle up never registers the row"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - crates/slipfork/src/daemon/lifecycle/fork.rs
  - "crates/slipfork/src/daemon/lifecycle/fork/**"
  - "crates/slipfork/src/daemon/lifecycle/up/**"
  - crates/slipfork/src/daemon/lifecycle/core.rs
  - "crates/slipfork/src/daemon/reconcile/**"
  - "crates/slipfork-world/src/ledger/**"
  - "crates/slipfork-world/src/layout/**"
  - "crates/slipfork-world/src/lib.rs"
  - "crates/slipfork-world/tests/ledger_lab.rs"
  - crates/slipfork/tests/critical_fork_live.rs
  - crates/slipfork/Cargo.toml
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-25T23:33"
updated: "2026-11-25T23:54"
---

# Owner repro (0.1.1 branch build, `~/slproj/test1/.forks/sloop`)

```
$ slipfork up
• [1/1] world  sloop--test1  up to date  <1ms
✓ Ready -> http://sloop--test1.test1.localhost

$ slipfork fork jello
✓ [1/5] fork   snapshot  done
✓ [2/5] git    branch jello  done
✓ [3/5] image  api  cached
error: the fork's root world has no ledger row
next: run `slipfork up` in the root checkout, then retry `slipfork fork`
```

`up` reports the world as **up to date** (Idle fast path) and exits before touching the ledger, so the root world (`test1`) never gets a row. `fork` then requires it and fails — and the suggested remedy (`up` in the root checkout) is the very thing that just short-circuited.

# Fix

1. **Registration is not work**: every `up` (including the Idle/up-to-date path) ensures the ledger row for the current world *and its ancestor chain* (root + parents, derived from the checkout path/`ForkPaths`) exists and is current. One cheap transaction, no rebuild, no restart; `--json` unchanged.
2. **`fork` resolves, never demands**: the root is found via the ledger or, when absent, registered from the on-disk root checkout (it is an ancestor of the current path); fail closed only if that checkout is genuinely missing. The error must name the world and path, with a `next:` that actually works.
3. Add the regression to `critical_fork_live`: `up` in a fork (Idle) → `fork` succeeds; and `fork` from a fork whose root row was deleted → re-registers from disk.

# Acceptance

- Live VM (private Btrfs tree, `do_vm.py sync` never, `~/demo` never): the owner's exact sequence works twice in a row (no leftovers, no `checkout missing`); a root that is truly absent fails closed with one actionable remedy.
- `make check`; Triad (arch: one registration owner; tests); commit `[M3.188] ...`, done, no push.
