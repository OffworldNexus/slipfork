---
id: M3.108
title: "COPY --from=<external image ref> (Docker parity) in the native builder"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.107]
paths:
  - "crates/slipfork-image/**"
  - "crates/slipfork/src/world/runtime/images/**"
  - "specs/03-under-the-hood.md"
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-24T12:45"
updated: "2026-11-24T15:07"
---

# Problem

The owner's Dockerfile (which Docker builds successfully):

```dockerfile
FROM python:3.14-trixie AS base
COPY --from=ghcr.io/astral-sh/uv:latest /uv /uvx /bin/
ADD https://offworld-nexus.app/install.sh /tmp/oni.sh
RUN sh /tmp/oni.sh && apt-get install -y blastit
```

fails with `no build stage named \`ghcr.io/astral-sh/uv:latest\``. Docker's `COPY --from` accepts a **previous build stage or any image reference**; Slipfork only resolves stage names.

# Change

- `CopySpec.from` resolution: numeric index → the walked stage; a declared stage name (and `AS` aliases) → that stage; anything else that parses as an OCI reference → an **external image**.
- Pull external refs through the existing puller/cache (`ImagePuller` in `crates/slipfork/src/world/runtime/images/`), honoring the build platform/arch and the existing store; hand the builder resolved rootfs paths (e.g. a `BuildRequest.externals: BTreeMap<String, PathBuf>`), so the builder stays free of network/ambient state (composition root does the pulling).
- Copy from the external rootfs with the same path traversal, `--chown`, permissions and symlink rules as a stage rootfs; never follow symlinks out of the root.
- `FROM <image>` continues to pull as today; a missing/unpullable external ref is one line + `next:`.
- Stage indices (`--from=0`) must work too.

# Acceptance

- Real build of the owner's first three instructions (base + external `COPY --from=ghcr.io/astral-sh/uv:latest`) succeeds; `/bin/uv --version` runs in the produced rootfs.
- Unit test for resolution (index/name/external ref) and a lab build with a local/minimal registry fixture if the repo has one; otherwise the live CI build test against the public ref.
- `make check`, `cargo test -p slipfork-image`.
- Commit `[M3.108] ...`, done, no push.
