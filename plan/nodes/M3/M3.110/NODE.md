---
id: M3.110
title: "Build containers run as container root in a 0->user userns; context is never mounted"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.108]
paths: ["crates/slipfork-image/**", "crates/slipfork-image/tests/**", "specs/03-under-the-hood.md"]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-24T12:58"
updated: "2026-11-24T15:57"
---

# Problem

Build `RUN` processes run as the **invoking uid** (`process.user = {host.uid,host.gid}`, single identity mapping `uid→uid`), so `apt-get install` fails ("run as root") and Dockerfiles that need system packages cannot build. There is no uid 0 inside, so `USER root` is meaningless.

Current facts (verified):
- `oci_config` mounts `/proc`, `/dev` (tmpfs), `/etc/resolv.conf`, a **synthetic `/etc/passwd` for the invoking uid**, and `RUN --mount` caches. The **build context is not mounted**; `COPY`/`ADD` read it on the host and write into the build rootfs. `WORKDIR` is just `cwd` inside the rootfs.
- `uidMappings: [containerID: host.uid, hostID: host.uid, size: 1]`.

# Contract

- Map **container 0 → invoking uid** (single mapping, size 1) and run build steps as container **root (0:0)** by default, i.e. Docker's default for a stage with no `USER`. Root inside the user namespace has no host-root power; every file created in the build rootfs is owned by the invoking user on the host.
- The build context stays **unmounted** (read-only source for `COPY`/`ADD` only). `RUN` writes only to the rootfs and the explicit cache mounts. Never a writable host bind of the checkout.
- Mount a `/etc/passwd` that contains `root:x:0:0` (and the image's users when present) instead of the synthetic invoking-uid file; `useradd`/`su` in a step must see the image's accounts.
- `COPY --chown` / `USER`: with a single mapping, only container 0 is representable. Document that builds currently run as root and non-zero `USER`/`--chown` ids are a known limitation (a subordinate-uid range + idmapped mounts is a later design); fail closed with a clear message rather than silently mis-owning.
- Prove with a real build: `RUN whoami` → `root`, `RUN apt-get update && apt-get install -y <tiny>` succeeds, files created in the rootfs are host-user-owned, and the project checkout is untouched during the build.

# Acceptance

- The owner's Dockerfile gets past a root-only `RUN` (the `COPY --from` gap is M3.108).
- `make check`, `cargo test -p slipfork-image`, live `critical_dockerfile_live`.
- Commit `[M3.110] ...`, done, no push.
