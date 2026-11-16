---
id: M3.1
title: "Deterministic make check: disk TMPDIR, CARGO_BUILD_JOBS=4, serialized world-booting suites"
kind: task
status: review
owner: implementer
priority: 1
depends_on: []
paths:
  - Makefile
  - crates/slipfork/tests/critical_slug.rs
  - crates/slipfork/tests/critical_cli.rs
  - "crates/slipfork-engine/tests/critical_oci_runtime.rs"
  - crates/slipfork/tests/support/mod.rs
  - "crates/slipfork-engine/tests/support/mod.rs"
  - tools/rs_test_list.py
  - tools/tests/test_rs_test_list.py
  - "crates/slipfork-testkit/**"
  - crates/slipfork/Cargo.toml
  - "crates/slipfork-engine/Cargo.toml"
  - Cargo.toml
  - Cargo.lock
attempts: 6
max_attempts: 6
escalation: none
created: "2026-11-16T06:34"
updated: "2026-11-16T21:21"
---

# Goal

Make plain `make check` deterministic and memory-bounded on a loaded shared host: zero Cargo/build scratch on the 32 GiB RAM `/tmp` `tmpfs`, bounded parallel `rustc` jobs, and serialized world-booting test binaries — with no `RUST_TEST_THREADS=1` requirement on the caller.

# Context

- At the `0.2.0` gate, `RUST_TEST_THREADS=1 make check` passed 100% while default-thread runs intermittently red-lined world-boot teardown in `critical_slug` / `critical_oci_runtime`, because concurrent test cases each booted a real `postgres:18` `crun` container.
- `/tmp` on this host is a RAM-backed `tmpfs` (`docs/knowledge/LESSONS.md`); Cargo build scripts, `TMPDIR` scratch, and `uv` caches there spike host RAM and are lost on reboot.
- After the Btrfs-only wipe (`M3.7`+), every world-booting suite boots real Btrfs subvolumes and `crun` containers, so host pressure is strictly worse than at `0.2.0`.

# Design / Invariants

1. Near the top of `Makefile`: `TMPDIR := $(HOME)/.cache/slipfork-tmp`, `export TMPDIR`, and a `$(TMPDIR):` rule that `mkdir -p`s it on disk as an order-only prerequisite of every target that compiles or executes Rust/Python (`rs-test`, `rs-lint`, `rs-fmt` if it needs it, `py-test`, `bench`, `bench-update`, `docs-check`). Never `/tmp`.
2. `CARGO_BUILD_JOBS ?= 4` exported so `cargo` never spawns one `rustc` per host core.
3. `rs-test` runs fast pure/unit/doc suites in parallel, then runs each world-booting integration binary serialized (`--test-threads=1`, one binary at a time) for `slipfork-engine` and `slipfork`; keep wall time bounded by listing binaries explicitly instead of `--workspace --all-targets` in one invocation.
4. Touch **only** `Makefile` plus the two test helpers whose intrinsic races make a plain `make check` non-deterministic (`LEAD SCOPE RULING`, attempt 2): `crates/slipfork/tests/critical_slug.rs` waits boundedly until `daemon.sock` refuses connections after the process-group TERM (mirroring `critical_cli.rs`'s existing auto-spawn cleanup proof), and `crates/slipfork-engine/tests/critical_oci_runtime.rs` polls real `psql -c 'select 1'` readiness boundedly instead of asserting one-shot. Never weaken timeouts, delete assertions, or add `sleep()` padding to `critical_*.rs`.

# Acceptance

```bash
grep -n 'TMPDIR := $(HOME)/.cache/slipfork-tmp' Makefile
grep -n 'CARGO_BUILD_JOBS ?= 4' Makefile
! grep -nE 'TMPDIR[^\n]*/tmp(/|$)' Makefile
make rs-test
make check
make check
test -d "$HOME/.cache/slipfork-tmp"
```

Expected: both `grep`s print their line and exit 0; the `! grep` prints nothing and exits 0; `make rs-test` and two back-to-back `make check` runs each exit 0 with zero environment overrides; `~/.cache/slipfork-tmp` exists on disk afterwards.
