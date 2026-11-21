---
id: M3.39.2
title: "Native OCI Distribution v2 puller: bearer auth, multi-arch manifests, streaming sha256 layers into RO Btrfs subvolumes"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.39.1]
paths:
  - "crates/slipfork-image/**"
  - "crates/slipfork-world/src/port.rs"
  - "crates/slipfork-world/src/engine.rs"
  - Cargo.toml
  - Cargo.lock
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-21T01:53"
updated: "2026-11-21T10:10"
---

# Goal
Pull upstream images directly over HTTPS with no Docker/BuildKit on the host: Docker Hub/GHCR/Quay bearer auth, multi-arch manifest list resolution for amd64/arm64, streaming sha256 verification, and tar + gzip/zstd whiteout extraction into cached read-only Btrfs subvolumes owned by `slipfork-image`.

# Context
- `specs/03-under-the-hood.md:57-60` (direct OCI pulls, cache layers once as read-only Btrfs subvolumes, `postgres:18` / `redis:7` / `python:3.13-slim` / `axllent/mailpit:latest` as the real pull targets).
- Crate/port ownership: `BaseImageBuilder` -> `NativeImageBuilder` (`specs/03-under-the-hood.md:125`); image cache lives at `.slipfork-store/images` per `specs/03-under-the-hood.md:25-26`.
- Replace placeholder in `crates/slipfork-image/src/lib.rs:1-13`; crate deps are empty today (`crates/slipfork-image/Cargo.toml:1-7`), so add an HTTPS client (rustls, no openssl) and a tar/gzip/zstd reader — add versions to `[workspace.dependencies]` in root `Cargo.toml` and inherit, never inline version drift.
- This crate must not read the environment nor bind sockets on its own: the composition root injects the cache root, registry base URLs, and credentials provider. Env-free library, cohesive struct (`OciPuller`/`ImageStore`), methods not free-function galaxies.
- Reference implementation anchors: whiteout semantics are `.wh.` files (character device / `.wh..wh..opq` opaque dirs); verify digest streamed while writing, reject mismatch before sealing; seal with `BTRFS_SUBVOL_RDONLY`.
- Test harness rule: zero mocks — either hit a real registry (network allowed in the lab) or stand up a real `127.0.0.1:0` loopback OCI reference server that serves the Distribution v2 endpoints from disk (`specs/04-testing-plan.md:9`).

# Acceptance
- `cargo test -p slipfork-image`
- Protected `crates/slipfork-image/tests/critical_oci_live.rs` pulls at least one real multi-arch manifest list and one small image end to end, asserts the resolved architecture matches the host, asserts layer sha256 mismatch is rejected with a typed error, asserts whiteout deletion and an opaque-directory whiteout apply correctly, and asserts the unpacked rootfs subvolume is read-only (write fails) on real Btrfs. Uses a real registry or the loopback reference server — never a `Mock*`/`Fake*` client. Unpack target is real Btrfs or fails closed.
- `cargo clippy --workspace --all-targets -- -D warnings`
- `uv run --no-project tools/guard.py`
