---
id: M3.42.3
title: "Close §2–§6 story gaps and green all local gates (check/docs/packages/packages-test/coverage)"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.42.2]
paths: ["crates/**", "scripts/**", "packaging/**", "docs/**", ".github/**", Makefile, TESTING_PLAN.md]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-21T17:30"
updated: "2026-11-21T17:30"
---

# Goal

Close the remaining `TESTING_PLAN.md` §2–§6 user-story gaps surfaced by the test-shape convergence,
and make all five local gates green on this dev host (no VM work here).

# Context

- Gate commands and their real definitions in `Makefile`: `check:` (line 28: fmt check, clippy
  `-D warnings`, `cargo test --workspace`, `bash editors/zed/check.sh`, `packages-check`),
  `docs-check:` (line 55: `uvx --offline zensical build --strict` + `tools/check_docs_honesty.py`),
  `packages:` (line 148: bundle/deb/rpm/arch/standalone/musl/apk + `packages-probe`),
  `packages-test:` (line 164: `packages-bundle`, layout, `bash -n`/`dash -n`, publisher unit tests,
  `check-install.sh`, `packages-probe`), `coverage:` added by M3.42.2.
- `packages-probe` (line 133) validates `.deb` via `dpkg-deb -I`, `.rpm` via `rpm -qpl` or a
  `rockylinux:9` docker fallback, `.pkg.tar.zst` via `tar --zstd`, `.apk`/`.tar.gz` via `tar -tzf`;
  `dist/packages/` currently holds `slipfork_0.1.0_amd64.deb`, `.rpm`, `.pkg.tar.zst`, `.apk`,
  `SHA256SUMS` — Docker is available on this host for the rpm/apk probes.
- Story gaps to audit and close (§2–§6): `s3 { cdn static_cdn }` public-read vs default-private `403`
  on anonymous `GET`; `route` `strip-prefix`/`rewrite`/`exact`/`domain mail.*` parity; `logs -f`
  streamed across `restart` (restart divider, no raw spinner bytes); `exec` forwarded exit code +
  `no-pod` remedy; `shell`/`db`/`redis`/`open` headless handling; OSPP secret rotation restart +
  encrypted dump seed; offline `secret(KEY) ?? random_hex(32)` fallback parity; PTY colour matrix
  (`NO_COLOR=1`, `TERM=dumb`, piped stdout) over `surface::VERBS`.
- Extend existing suites rather than duplicating: `crates/slipfork/tests/{ansi_matrix,status_surface,ingress_handoff}.rs`
  plus the protected `critical_*.rs` files under `crates/slipfork/tests/`, `crates/slipfork-image/tests/`,
  `crates/slipfork-ingress/tests/`, `crates/slipfork-world/tests/`.

# Acceptance

- `make check` exits 0 (zero env overrides).
- `make docs-check` exits 0.
- `make packages` exits 0 and regenerates `dist/packages/SHA256SUMS`.
- `make packages-test` exits 0.
- `make coverage` exits 0.
- Every §2–§6 story above has at least one assert in a real lab E2E test under `crates/*/tests/` (no mocks).
