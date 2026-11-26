---
id: M3.129
title: "fork rebuilds every image: build-context digest includes VCS metadata, so a branch switch invalidates the parent stamp"
kind: epic
status: done
priority: 3
depends_on: []
paths:
  - "crates/slipfork-image/src/context.rs"
  - "crates/slipfork-image/src/inputs.rs"
  - "crates/slipfork-world/src/boot/stamp/epoch.rs"
  - crates/slipfork/src/daemon/lifecycle/fork.rs
  - "crates/slipfork-image/tests/image_core_lab.rs"
  - crates/slipfork/tests/critical_fork_live.rs
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-24T20:44"
updated: "2026-11-24T21:12"
---

# Problem

Owner: "apparently when I fork it's re-building everything from scratch." Fork should inherit the parent world's sealed images (the `M3.123` capture also showed `fork salut` doing `built 3 steps` while the parent image was `cached`).

Fork boots through the normal `up` path (`prepare` snapshots project+world, then `vcs.switch`es the fork checkout to the target branch, then `begin_up`). The world stamp adoption (`WorldStampStore::trusted_stamp`) requires the fork's `plan_epoch(plan, features, image_inputs)` to equal the epoch stored by the parent boot. `image_inputs` (`slipfork_image::build_inputs_digest`) folds the `Dockerfile` bytes and the `.dockerignore`-filtered `BuildContext` content hash.

`BuildContext` reserves `.slipfork`, `.slipfork-store`, `.forks`, the Dockerfile and `.dockerignore` files — but **not `.git`**. The fork's branch switch rewrites `.git/HEAD`, index, refs and logs, so the fork's context hash differs from the parent's even when no application file changed, the epoch mismatches, and every image rebuilds.

# Required outcome

1. **Prove which seed component differs** (parent stored epoch vs the fork's recomputed epoch; diff the `plan_epoch` seed lines and the context entries) before changing anything. Record the evidence in the node LOG.
2. **VCS metadata is not application build input.** Exclude `.git` from the canonical build context (walk, content hash, and the `COPY` payload) so a branch switch alone can never invalidate an image. Keep `.dockerignore` authoritative for everything else; no new ignore file, no second digest layer.
3. **Fork reuses the parent's sealed images** when the Dockerfile and the real copied files are unchanged: `fork` shows `image  <id>  cached` (no `built N steps`), then boots. A genuine deps/app change on the fork branch still rebuilds (Docker-correct for `-F -dev`/Docker mode).
4. **Deps change still rebuilds:** editing `pyproject.toml`/`uv.lock` (or a copied source file) on the fork branch rebuilds only the affected steps, not "everything from scratch" when the earlier steps are unchanged.

# Acceptance

- Live on a real Btrfs VM private tree (never `do_vm.py sync`, never `~/demo`): owner-shaped project (`Dockerfile` with `COPY . .`, `WORKDIR /code`, a `dev` service); `up` builds; `fork <name>` on a different branch prints `image … cached` and boots; then a deps edit rebuilds and stays cached afterwards.
- `critical_fork_live` covers the cached-image fork case (currently it only proves the happy path); `.git` is absent from the built image (`exec … ls -a /code` shows no `.git`).
- `make check` green; Triad arch/tests/ux; commit `[M3.130] ...`, done, no push.
