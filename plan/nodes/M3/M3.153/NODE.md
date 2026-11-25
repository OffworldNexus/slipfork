---
id: M3.153
title: "Service pods must run as the image's USER (default root) through the same single-owner userns mapping and Docker caps as build steps"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - "crates/slipfork-world/src/pod.rs"
  - "crates/slipfork-world/src/pod/**"
  - "crates/slipfork-world/src/identity.rs"
  - "crates/slipfork-world/src/pod_env.rs"
  - "crates/slipfork-world/src/lib.rs"
  - "crates/slipfork-world/src/resources/**"
  - "crates/slipfork-image/src/identity.rs"
  - "crates/slipfork-image/src/runtime.rs"
  - "crates/slipfork-image/src/dockerfile/**"
  - crates/slipfork/src/world/runtime.rs
  - "crates/slipfork/src/world/runtime/**"
  - "crates/slipfork-world/tests/pod_run_live.rs"
  - "crates/slipfork-world/tests/**"
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-25T10:10"
updated: "2026-11-25T11:14"
---

# Owner report (dev feature removed, so the image's CMD runs)

```
✗ [2/2] start web failed
  │ init runs as uid 1000 and is missing cap_chown, cap_dac_override, cap_fowner, cap_kill, cap_setgid, cap_setuid.
  │   A rootless deployment gives the capabilities to a *dedicated copy* of the binary, never to the shared one:
  │   setcap 'cap_chown,cap_dac_override,cap_fowner,cap_kill,cap_setgid,cap_setuid,cap_setpcap+ep' <the copy>
```

Owner: "the build steps can be run as root but the run is automatically as the user? … It should be running as whatever user the Dockerfile suggests following the same mapping logic that we already have, shouldn't it?"

# Diagnosis

`crates/slipfork-world/src/pod.rs:353,370-371` runs every service as `{uid: host.uid, gid: host.gid}` with a 1-entry identity mapping (`host.uid→host.uid`) and **no capabilities**. The build path already has the right model (`M3.110`: per-step mapping effective uid → invoking uid, default container root, Docker default caps inside the userns, `noNewPrivileges`, no synthetic `/etc/passwd`). The run path never learned it: an image with no `USER` must run as **root inside the userns** with Docker's default caps (chown, dac_override, fowner, fsetid, kill, setgid, setuid, setpcap, net_bind_service, sys_chroot, mknod, audit_write, setfcap) and map container `0` → invoking host uid, so blastit-style inits work and host files stay user-owned.

# Contract

- Resolve the run identity from the **final image stage**: `USER <name|uid>[:<group>]`, name→uid/gid via the image's `/etc/passwd`/`/etc/group`, default `0:0`. Missing image or pulled image: `0:0` (Docker semantics).
- Map exactly the resolved container uid/gid → invoking host uid/gid (single-owner, size 1), keep the existing `noNewPrivileges`, and apply Docker's default capability set when the resolved uid is 0; for a non-root `USER`, drop caps (Docker behaviour) except what the process needs (none by default).
- Env: `HOME`/`USER`/`LOGNAME` from the resolved identity (root → `/root`), and the image's `ENV` still applies.
- `slipfork exec`/`shell` use the same identity as the service (no separate path), and files created in a bind mount stay owned by the invoking uid on the host.
- Fail closed with one line + `next:` if the image's `USER` cannot be resolved (missing passwd entry) or if the identity needs a subordinate-id range we do not support.
- Resources (postgres/redis) keep their current identity unless the spec already names one; do not change them silently.

# Acceptance

- Live on the VM (Btrfs, as project user, `XDG_RUNTIME_DIR`, never `do_vm.py sync`, never `~/demo`): an image whose CMD is a cap-needing init (the blastit shape, or a tiny `capsh --print`/`chown` probe) boots; `exec -- id` reports `uid=0` (root-in-userns) while a file it creates in the project is owned by the invoking host uid; a Dockerfile `USER 1000:1000` still boots and `id` shows that uid; a `USER nobody` with no passwd entry fails closed with one remedy.
- `make check`, Triad (arch: one identity owner shared with the build mapping), commit `[M3.153] ...`, done, no push.
