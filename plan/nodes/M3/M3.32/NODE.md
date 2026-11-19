---
id: M3.32
title: "Forkfile DSL, Per-Service crun Pod Engine & Clean-Sheet 0.1.0 CLI UX"
kind: epic
status: ready
priority: 3
depends_on: []
paths: []
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-19T13:02"
updated: "2026-11-19T13:02"
---

# Goal

Replace `snow.yml` + `slipfork.toml` with one purpose-made procedural `<repo_root>/Forkfile` DSL, run **every** service in its own daemonless `crun` container sharing one `userns`/`netns`/`ipcns` + one Btrfs world subvolume, and ship the clean-sheet `0.1.0` CLI surface — all proven by zero-mock lab E2E on the live Btrfs droplet.

# Context

Normative specs already on disk (written by `architect`): `docs/design/forkfile.md` (§1 features/`on`, §2 resource property parity, §3 services/volumes/direct `execve`, §4 multi-error caret checker, §5 Rust/JSON interface), `docs/design/dx-cli-0.1.0.md` §1 pod lifecycle & fleet tree, §2 multiplexed logs + container entry, §3 complete command/`--json` contract, `docs/design/ospp.md`, `ARCHITECTURE.md`, `ADR-0001`, `ADR-0009`.

Six coarse vertical slices, each owning domain module + CLI/presenter + a slice-level test. They are **strictly serialized** because every slice edits shared wiring (`Cargo.toml`, `Cargo.lock`, `Makefile`, `crates/**`) — `plan.py set active` path locks would otherwise collide. Order: `M3.32.1` forkfile crate → `M3.32.2` crun pod engine → `M3.32.3` slipforkd cut-over + logs/ingress → `M3.32.4` CLI → `M3.32.5` docs/demo → `M3.32.6` live VM proof.

Pre-`1.0` scorched earth: `slipfork-snow`, `docs/snow-modes.md`, `docs/reference/manifest.md`, `slipfork.toml`, `WorldImageConfig`, and `slipfork list` are **deleted**, never aliased or `#[deprecated]`.

# Deliverables

- `crates/slipfork-forkfile`: parser, whole-file multi-error checker with `line:col` carets, evaluator, JSON IR, `TaintedString`, `FileVaultProvider` (`M3.32.1`).
- Per-service `crun` pods with Btrfs `volume` CoW submounts and direct `execve` argv, `slipfork.toml` gone (`M3.32.2`).
- `slipforkd`/OSPP on `EvaluatedFork`; prefix + subdomain + port-agnostic `.localhost` ingress; multiplexed `logs -f` surviving `restart` (`M3.32.3`).
- Clean-sheet `0.1.0` CLI per `dx-cli-0.1.0.md` §1–§3; `@slipfork/opencode` on `status --json` (`M3.32.4`).
- Visual `Forkfile` docs, `CHANGELOG`, multi-service `support/demo` (`M3.32.5`).
- Live `165.232.86.174` PTY + Btrfs proof, `demo` left running on `:80` (`M3.32.6`).

# Acceptance

- `make check` (single workspace build) and `make docs-check` exit `0` locally.
- `uv run --no-project tools/plan.py validate` exits `0`.
- `uv run --no-project tools/do_vm.py test` exits `0` on `165.232.86.174`.
- Zero `RESULT.md` anywhere; evidence lives in `LOG.md` / `JOURNAL.md`.
