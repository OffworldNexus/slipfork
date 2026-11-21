---
id: M3.42.5
title: "Fold live-demo multi-S3/CDN/OCI-entrypoint/runtime-env fixes with §2 E2E assertions"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.42.2]
paths: ["crates/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-21T18:06"
updated: "2026-11-21T18:06"
---

# Goal

Fold the 4 real multi-resource runtime fixes found while booting the live 8-component demo on `165.232.86.174` into `crates/`, each with its `TESTING_PLAN.md` §2 story assertion. Patch of record: `/tmp/opencode/demo-runtime-fixes.patch` (touching zero repo files; 1448+/62- across 15 files).

# Context

- New `slipfork-forkfile/src/eval/{mod,values}.rs` (622 LOC): `env()`/variable evaluation used by resources; keep it inside the Forkfile compiler (canonical owner, no re-scanning `$` downstream) and under 500 LOC per file.
- `slipfork-ingress/src/route/{extract,mod,rules}.rs`: CDN ingress host extraction/rules for `s3 { cdn <id> }` (`.domain`/`.url`, public-read vs default-private `403`).
- `slipfork-world/src/boot/{mod,plan}.rs` + `slipfork/src/world/runtime/{mod,helpers}.rs`: per-resource runtime env, multi-S3 bucket boot ordering, OCI baked-in entrypoint handling, `daemon/host.rs` + `lifecycle/ps.rs` wiring.
- The patch was authored directly on the VM; it is a hypothesis, not a contract. Validate every hunk against `specs/01-forkfile.md`, `specs/03-under-the-hood.md` and the current crate APIs; drop anything that duplicates existing code (this epic just finished a coverage sweep).

# Acceptance

- `git apply --3way /tmp/opencode/demo-runtime-fixes.patch` (or a clean manual fold) leaves no duplicate logic and no `> 500` LOC file.
- Each of the 4 fixes has a real lab assertion in the §2 suites (`crates/slipfork-ingress/tests/route_table_journey.rs`, `crates/slipfork-image/tests/image_core_lab.rs`, `crates/slipfork/tests/*` or the world lab): multi-S3 (`uploads` private `403`, `assets` public-read `cdn static_cdn` `200`), CDN host routing, OCI baked-in entrypoint exec, per-resource runtime env.
- `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` exit 0; `uv run --no-project tools/critical_guard.py --base M3.42.2 --node M3.42.5` clean.
