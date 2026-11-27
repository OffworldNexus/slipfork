---
id: M3.166
title: Fold resolved build args into image_inputs/plan_epoch so an arg edit invalidates sealed heads
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.167]
paths:
  - "crates/slipfork/src/daemon/reconcile/**"
  - "crates/slipfork/src/world/**"
  - crates/slipfork/src/daemon/lease.rs
  - "crates/slipfork-image/src/inputs.rs"
  - "crates/slipfork-image/tests/**"
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-25T12:38"
updated: "2026-11-25T17:04"
---

# Problem (found while landing M3.154)

`image_inputs` (`crates/slipfork/src/world/mod.rs:40-63`) folds only `Dockerfile` bytes + build-context hash into `build_inputs_digest`, so `plan_epoch` (`crates/slipfork-world/src/boot/stamp/epoch.rs:284-320`) and `WorldHeads::validate` cannot see a Forkfile `arg` value change. Edit `arg PORT = web.port` (e.g. the service `port 3000` to `5173`) and restart without `up`: `heads.validate` passes and `__supervise` boots the previous image instead of asking for `slipfork up`.

M3.154 made `build_inputs_digest` arg-aware in the image crate but could not land it: the only production caller lives in `crates/slipfork/src/world/**`, locked by M3.158 while it rewrites that file.

# Change

1. `build_inputs_digest(context, dockerfile, target, args: &BuildArgs)` folds `args.fingerprint()` (`crates/slipfork-image/src/dockerfile/variables.rs:44`) before the context hash.
2. `image_inputs` resolves each build image args (`doc.eval_image_args_with(world, scheme, &image.id, features)`) and passes them; thread the world name it already has at every call site (`daemon/reconcile/mod.rs:75`, `daemon/lease.rs:149`, `world/heads.rs:121`).
3. Test: two heads maps differing only in a service port must fail `validate`, and re-sealing must accept them.

# Acceptance

- A changed `arg` value (service port) makes `WorldHeads::validate` fail with its existing one-line remedy.
- `cargo test -p slipfork-image` + the heads/epoch tests green; `make check`; Triad; commit; no push.
