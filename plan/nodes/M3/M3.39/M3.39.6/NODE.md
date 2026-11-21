---
id: M3.39.6
title: "Live Btrfs/OCI/crun lab gate: consolidated real E2E, VM target docs, zero-mock sweep, green make check on XFS"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.39.5]
paths: ["crates/**", Makefile, tools/do_vm.py, TESTING_PLAN.md]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-21T01:53"
updated: "2026-11-21T01:53"
---

# Goal
Close the epic with one honest live gate: consolidated real Btrfs + OCI + `crun` E2E runnable on the dedicated VM / loop-backed Btrfs, `make check` green on the non-Btrfs XFS dev host, a zero-mock sweep, and `TESTING_PLAN.md` documenting the live target and fail-closed rule.

# Context
- Testing philosophy: real Btrfs, real `crun`, real registries, real pg/redis/s3, real binary; zero `Mock*`/`Fake*`/`Stub*`/`Recording*`/`TestTreeDriver`; only stand-in is a `127.0.0.1:0` loopback reference server (`specs/04-testing-plan.md:1-9`).
- Live-host rule: static checks stay green and fast on a non-Btrfs XFS host, live suites run through the dedicated VM target, and non-Btrfs must fail closed rather than skip silently (`.opencode/LESSONS.md:27`), with short scratch roots to respect the 107-byte `sun_path` limit (`.opencode/LESSONS.md:20`).
- Existing targets: `Makefile:15-22` (`check` = fmt --check + clippy -D warnings + test; `guard`), `tools/do_vm.py` is the VM harness entry point. Protected suites added by M3.39.1-5 are `crates/slipfork-world/tests/critical_btrfs_live.rs`, `crates/slipfork-world/tests/critical_pod_runtime.rs`, `crates/slipfork-image/tests/critical_oci_live.rs`, `crates/slipfork-image/tests/critical_dockerfile_live.rs`, `crates/slipfork/tests/critical_world_journey.rs`; they are guarded by `tools/critical_guard.py` (`crates/*/tests/critical_*.rs`) and must not be weakened.
- Document the concrete live target `165.232.86.174` and the loop-backed Btrfs runner path in `TESTING_PLAN.md`, including the fail-closed contract and the exact command to run the live gate; keep it user-story-first, no `ADR-*`/`M3.*` lawyer prose.

# Acceptance
- `make check` exits 0 on the XFS dev host (fmt, clippy `-D warnings`, full workspace tests; live suites compile and fail closed).
- Live gate on `165.232.86.174` or a loop-backed Btrfs runner: every `critical_*` suite above runs and passes against real Btrfs + real `crun` + real registry.
- `grep -rIn 'Mock\|Fake\|Stub\|Recording\|TestTreeDriver' crates/ --include=*.rs` returns zero test stand-in definitions.
- `uv run --no-project tools/guard.py`
- `uv run --no-project tools/plan.py validate`
