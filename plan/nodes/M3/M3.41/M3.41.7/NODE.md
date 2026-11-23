---
id: M3.41.7
title: Install docs match shipped package filenames and OpenRC flow
kind: task
status: done
owner: "doc-writer"
priority: 3
depends_on: []
paths: [docs/install.md]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-21T17:26"
updated: "2026-11-21T17:27"
---

# Goal
Make the install page name the artifacts the release actually builds and the OpenRC flow that actually works.

# Context
- `scripts/build-apk.sh` emits `slipfork-<ver>-r0-<arch>.apk` (arch included); `docs/install.md:56` still says `slipfork-0.1.0-r0.apk`.
- Debian/rpm/Arch filenames match `dist/packages` today; keep the version but use `<arch>` for portability.
- OpenRC: `sudo slipfork daemon install --ingress` writes `/etc/conf.d/slipfork-ingress` with the invoking user; then `rc-update add slipfork-ingress default && rc-service slipfork-ingress start`.

# Acceptance
- `docs/install.md` package table uses `slipfork_0.1.0_<arch>.deb`, `slipfork-0.1.0-1.<arch>.rpm`, `slipfork-0.1.0-1-<arch>.pkg.tar.zst`, `slipfork-0.1.0-r0-<arch>.apk` and explains `amd64/x86_64` vs `arm64/aarch64` in one line.
- Alpine note names `rc-update` + `rc-service` after `daemon install --ingress`.
- `make docs-check` exits 0.
