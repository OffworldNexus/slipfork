---
id: M3.32.2.2.3
title: "slipforkd pod boot cut-over in crates/slipfork (PodBoot/ServicePod, per-service exec targets, live suites)"
kind: epic
status: active
priority: 3
depends_on: [M3.32.2.2.2]
paths: ["crates/slipfork/**", Cargo.toml, Cargo.lock]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-19T13:24"
updated: "2026-11-19T19:03"
---

# Goal

Boot the per-service pod engine from `slipforkd`, remove the single merged rootfs and the `slipfork.toml` loader, and migrate the live suites.

# Context

Split from `M3.32.2.2`. Today `crates/slipfork/src/daemon/world.rs:316-330` boots one `pivot_root` world and spells component commands as `sh -eu -c`.

- Rewrite `daemon/{world.rs,boot.rs,backing.rs,base.rs}` into cohesive `PodBoot`/`ServicePod` structs (<= 500 LOC each; split rather than grow `world.rs`), wired only from `main.rs`.
- Per-service `volume` submounts over the live `MS_BIND` host worktree (inotify HMR intact); CoW volume clone on `fork` stays ~20 ms.
- `exec`/`shell`/`db`/`redis`/`logs` target a named service container instead of the merged world root.
- Delete the `sh -eu -c` component command and the `slipfork.toml` loader in `daemon/{project,resolve}.rs`; project identity comes from `Forkfile` or VCS.
- Migrate `crates/slipfork/src/daemon/tests/*` and any live suite keyed to pivot-root semantics; do not weaken assertions, do not delete protected tests.
- Do not delete `slipfork-snow` (that is `M3.32.3`).

# Acceptance

- `cargo test -p slipfork` exits 0.
- `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all -- --check` exit 0.
- No `sh -eu -c`/`/bin/sh -c` under `crates/slipfork/`; no `slipfork.toml` string under `crates/`.
- Write NO `RESULT.md`.
