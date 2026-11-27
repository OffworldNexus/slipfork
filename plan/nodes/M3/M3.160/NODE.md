---
id: M3.160
title: "Containers inherit an unknown umask: set 0022 explicitly for build steps and pods (blastit refuses group-writable .venv)"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - "crates/slipfork-world/src/pod.rs"
  - "crates/slipfork-world/src/pod_env.rs"
  - "crates/slipfork-world/src/identity.rs"
  - "crates/slipfork-image/src/runtime.rs"
  - "crates/slipfork-image/src/identity.rs"
  - "crates/slipfork-world/tests/pod_run_live.rs"
  - "crates/slipfork-image/tests/critical_dockerfile_live.rs"
attempts: 4
max_attempts: 4
escalation: none
created: "2026-11-25T11:32"
updated: "2026-11-25T12:28"
---

# Owner report

With the image `USER` (root) now honoured, the app's init refuses to serve:

```
blastit serve: the project virtualenv (/code/.venv) could be replaced by another uid: it is
group- or other-writable. chown it to root (or to init's uid, 0), remove the group and other
write bits (chmod 0644 for a file, 0755 for a directory), and do the same to every directory above it
```

`.venv` was created by `uv sync` inside a build step. Nothing in the repo sets a umask (`grep -rn umask crates` → empty), so the build step and the pod inherit whatever the daemon has — commonly `002` (group-writable) on desktops — which yields `0775` dirs and trips the app's safety check. Docker/podman give containers `0022` deterministically.

# Fix

- Set **`umask: 0o022` explicitly** in every OCI process config we generate: build-step containers (`slipfork-image/src/runtime.rs`) and service/resource pods (`pod_env`/`pod.rs`). Do not rely on inheritance.
- One owner for the default (a single constant next to the identity defaults), used by both.
- Keep it out of user-visible `env`; it is process state, not an env var.
- Tests: a build step creating a dir + file yields `0755`/`0644`; the same for a pod. Ensure no existing mode/ownership expectation regresses (the user-owned-files rule still holds through the userns).

# Acceptance

- Live on the VM: build an image whose `RUN` does `mkdir d && touch f` and `uv sync` (or `python -m venv`) → `d` is `0755`, files `0644`, `.venv` not group/other-writable; the blastit-shaped init passes its writability check and serves; `exec -- stat -c %a /code/.venv` confirms.
- `make check`; Triad; commit `[M3.160] ...`, done, no push.
