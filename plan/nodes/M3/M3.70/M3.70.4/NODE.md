---
id: M3.70.4
title: "Prod-shape runtime: rootfs symlink exec, feature-aware stamp trust & stale crun cleanup"
kind: task
status: done
owner: expert
priority: 3
depends_on: []
paths: ["crates/slipfork/**", "crates/slipfork-world/**", "crates/slipfork-image/**"]
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-23T14:21"
updated: "2026-11-23T15:40"
---

# Goal

Fix the remaining prod-shape runtime defects exposed by M3.70.2 live proof (demo ./api + ./front, -F -dev). Evidence:

1. pod exec resolution follows absolute symlinks on the HOST rootfs: crates/slipfork/src/world/runtime/helpers.rs:244 resolve_in_rootfs uses rootfs.join(...).is_file(); a venv /app/.venv/bin/python -> /usr/local/bin/python3.13 symlink is judged missing when the host lacks that exact host path, so argv falls back to /usr/local/bin/python (base image) and the venv PATH is lost. Fix with a rootfs-aware existence check (symlink target resolved inside the rootfs, or accept a symlink itself) so /app/.venv/bin/python is exec'd. Add a unit test with a real venv-shaped symlink in a /var/tmp rootfs.
2. fork -F -dev / --no-default-features still boots with the parent's dev image workdir (/project/api) and dev-only bind, i.e. feature overrides do not survive into the fork supervisor, or the parent world stamp is adopted across feature sets. Reproduce on the VM (see M3.70.2 LOG), prove with .slipfork/features.json + plan_epoch, and fix: either persist the request before boot reliably, and/or include the resolved active feature names in plan_epoch/trusted_stamp so a different feature set can never adopt another mode's images. Add tests.
3. Rebuilding a failed prod image can hit a stale crun container ('container sf-... already exists'); make the step runtime remove/reuse its own stale container name before create.

# Acceptance
- cargo fmt/clippy/test workspace green; new unit tests for rootfs symlink resolution and feature-aware stamp trust.
- Live: /usr/bin/slipfork fork prod-check -F -dev from /home/slipfork/demo boots front node build + api uvicorn from image CMD, 200 on / and /api/status, no mailpit, exec front/api -- test ! -e /project, then drop; demo/feat-auth remain dev 200, tralala stopped.
