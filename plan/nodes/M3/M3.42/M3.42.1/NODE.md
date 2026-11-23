---
id: M3.42.1
title: "Test-shape convergence: prune non-§7 inline tests into lab E2E journeys"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - "crates/slipfork-world/**"
  - "crates/slipfork-image/**"
  - "crates/slipfork-ingress/**"
  - "crates/slipfork-vcs/**"
  - "crates/slipfork/**"
  - "crates/slipfork-forkfile/**"
  - "crates/slipfork-ospp/**"
  - "crates/slipfork-lsp/**"
  - "editors/**"
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-21T17:30"
updated: "2026-11-21T17:53"
---

# Goal

Converge the test shape to `TESTING_PLAN.md` §7: remove every non-§7 inline `#[cfg(test)] mod tests`
block from the runtime crates, relocating any behavior it uniquely proved into the real lab E2E
suites under `crates/*/tests/` (one rich journey per §2–§6 user story, zero mocks) instead of
dropping coverage. Protected `crates/*/tests/critical_*.rs` must stay green and unweakened.

# Context

- Verified inline `mod tests` distribution (files containing `mod tests` under `src/`):
  `crates/slipfork-world` 13 of 35 `.rs`, `crates/slipfork-image` 10 of 21,
  `crates/slipfork` 7 of 48, `crates/slipfork-ingress` 1 of 11, `crates/slipfork-vcs` 0 of 1. All of
  these are non-§7 and must be deleted (after relocating proven behavior).
- Keep (§7 "Language, Wire Protocol & Editor Tooling (Focused Coverage)"):
  `crates/slipfork-forkfile` 8 files, `crates/slipfork-ospp` 6 files, `crates/slipfork-lsp` 1 file,
  plus `editors/tree-sitter-forkfile` and `editors/zed`.
- Existing lab suites to extend: `crates/slipfork/tests/` has `ansi_matrix.rs`, `status_surface.rs`,
  `ingress_handoff.rs`, `fork_nonbtrfs.rs` and protected `critical_cli_surface.rs`,
  `critical_daemon.rs`, `critical_fmt_migrate.rs`, `critical_fork_live.rs`, `critical_host_verbs.rs`,
  `critical_status_live.rs`, `critical_world_journey.rs`; `crates/slipfork-world/tests/` has
  `critical_btrfs_live.rs`, `critical_pod_runtime.rs`; `crates/slipfork-image/tests/` has
  `critical_oci_live.rs`, `critical_dockerfile_live.rs`; plus `critical_ingress.rs` (ingress),
  `git_workspace.rs` (vcs), `critical_lsp.rs` (lsp), `critical_forkfile.rs` (forkfile),
  `critical_ospp.rs` (ospp).
- `tools/critical_guard.py` (run via `make guard`) protects every `crates/*/tests/critical_*.rs` and
  `crates/*/tests/critical/**`; never weaken, rename, or delete them.
- §2–§6 of `TESTING_PLAN.md` name the exact user stories the relocated assertions belong to; delete
  0%-covered fallback branches instead of porting them into tests.

# Acceptance

- `grep -rl 'mod tests' crates/slipfork-world/src crates/slipfork-image/src crates/slipfork-ingress/src crates/slipfork/src crates/slipfork-vcs/src | wc -l` prints `0`.
- `grep -rl 'mod tests' crates/slipfork-forkfile/src crates/slipfork-ospp/src crates/slipfork-lsp/src | wc -l` prints `15`.
- `cargo fmt --all -- --check` exits 0.
- `cargo clippy --workspace --all-targets -- -D warnings` exits 0.
- `cargo test --workspace` exits 0.
- `uv run --no-project tools/guard.py` exits 0.
