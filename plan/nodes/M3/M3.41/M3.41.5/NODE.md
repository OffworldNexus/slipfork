---
id: M3.41.5
title: "Musl-clean slipfork-world fdpass CMSG widths for dynamic-musl .apk"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths: ["crates/slipfork-world/src/fdpass.rs", "crates/slipfork-world/src/**"]
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-21T16:56"
updated: "2026-11-21T17:02"
---

# Goal
Make `crates/slipfork-world` compile for `x86_64-unknown-linux-musl` with `-C target-feature=-crt-static` so `scripts/build-apk.sh` can emit a genuine dynamic-musl `.apk` and the musl standalone archive.

# Context
- M3.41.3 blocked: `cargo build -p slipfork-world --target x86_64-unknown-linux-musl` fails with 13 E0308/E0277 in `crates/slipfork-world/src/fdpass.rs` because musl types `cmsg_len` as `u32` while glibc uses `usize` (lines ~70-145: `CMSG_LEN`, `CMSG_SPACE`, `data_len`, `size_of::<RawFd>()`, pointer offsets).
- Keep glibc behaviour byte-identical; use explicit `usize::try_from`/`u32::try_from` with the existing error path, never `as` truncation for lengths.
- Do not change the `SCM_RIGHTS` wire contract or add traits/mocks. Extend the existing fdpass module and its tests only.

# Acceptance
- `cargo build -p slipfork-world --target x86_64-unknown-linux-musl` exits 0.
- `cargo test -p slipfork-world` and `cargo clippy -p slipfork-world --all-targets -- -D warnings` exit 0.
- Existing glibc fdpass tests (including critical ones) untouched and green.
- `cargo fmt --all -- --check` exits 0.
