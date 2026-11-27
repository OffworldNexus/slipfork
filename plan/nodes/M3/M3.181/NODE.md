---
id: M3.181
title: "Wire critical_snapshot_graph + logs_last_live into LIVE_SUITES, isolate opencode e2e runtime dir, and bump 0.1.1 package defaults"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.180]
paths:
  - Makefile
  - packaging/rpm/slipfork.spec
  - packaging/arch/PKGBUILD
  - packaging/alpine/APKBUILD
  - "packages/opencode-slipfork/tests/**"
  - "packages/opencode-slipfork/src/index.ts"
  - tools/critical_guard.py
  - "tools/tests/**"
  - TESTING_PLAN.md
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-25T19:22"
updated: "2026-11-25T19:48"
---

# Goal

Close test-gate blind spots where two live Btrfs suites never run in CI, `packages-check` collides with an active host daemon, and static package defaults lag `0.1.1`.

# Evidence

1. **Dead live suites in `Makefile:12-26` (`LIVE_SUITES`):**
   - `crates/slipfork-world/Cargo.toml:60` sets `required-features = ["live"]` on `critical_snapshot_graph`, and `crates/slipfork/tests/logs_last_live.rs:12` has `#![cfg(feature = "live")]` — neither runs in `cargo test --workspace` and neither is listed in `Makefile` `LIVE_SUITES` (14 entries vs 16 real live suites).
2. **Ambient `XDG_RUNTIME_DIR` leak & unprotected plugin tests (`packages/opencode-slipfork/tests/e2e.test.ts:300-361`):**
   - The `composition root` test invokes `plugin.effect` with ambient `process.env.XDG_RUNTIME_DIR`, failing `make js-test` with daemon-identity skew whenever a host `slipfork` daemon from another path/mtime is running.
   - `packages/opencode-slipfork/tests/{e2e,contract}.test.ts` replaced `*.critical.test.ts` and are no longer matched by `tools/critical_guard.py`.
3. **Version default drift:** `Makefile:240` (`VERSION ?= 0.1.0`), `packaging/rpm/slipfork.spec:3`, `packaging/arch/PKGBUILD:6`, and `packaging/alpine/APKBUILD:5` still default to `0.1.0` while `Cargo.toml:15` is `0.1.1`.

# Acceptance

- `Makefile` `LIVE_SUITES` runs all 16 live suites (including `slipfork-world/critical_snapshot_graph` and `slipfork/logs_last_live`).
- `packages/opencode-slipfork` tests isolate `XDG_RUNTIME_DIR`/`XDG_STATE_HOME` under `/var/tmp` even when a host daemon is running, and `tools/critical_guard.py` guards the plugin critical suite.
- `Makefile` and `packaging/` defaults match `0.1.1`; `make check` and `tools/guard.py` exit `0`.
