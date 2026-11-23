---
id: M3.30.2.3
title: Split btrfs_engine.rs + service.rs into BtrfsWorldEngine/OciContainerRuntime/SupervisedProcess
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.30.2.2]
paths: [Cargo.toml, Cargo.lock, "crates/slipfork-engine/**", "crates/slipfork-testkit/**"]
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-19T00:03"
updated: "2026-11-19T01:14"
---

# Goal
Section 3 (`btrfs_engine.rs` 4.1k + `service.rs`) of `plan/nodes/M3/M3.30/M3.30.2/NODE.md`: cohesive structs, files `<= ~560` LOC.

# Work
- `BtrfsWorldEngine`: subvolume orchestration/materialization.
- `OciContainerRuntime`: `crun` execution + container bundle.
- `SupervisedProcess`: owns `setsid` + `-pgid` process-group stop & reap, replacing the 22 loose PID free functions in `service.rs`.
- Split along `worlds/` materialization, supervision, container bundle, teardown; keep all fail-closed diagnostics in the engine.

# Constraints
No behavior regressions; `-pgid` reaping semantics preserved; no globals/static mutable; no new traits. Btrfs live suites fail closed off-Btrfs as today. No RESULT.md.

# Acceptance
- `for f in crates/slipfork-engine/src/btrfs_engine.rs crates/slipfork-engine/src/service.rs; do test $(wc -l < "$f") -le 560 || echo "TOO LONG $f"; done`
- `! grep -rn "fn .*(" crates/slipfork-engine/src/service.rs | wc -l` sanity: no 22 loose PID fns (report count)
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test -p slipfork-engine` (report fail-closed Btrfs legs)
