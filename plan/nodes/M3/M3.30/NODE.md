---
id: M3.30
title: "0.1.0 Ship-Ready Sweep: dead-weight purge, god-file splits, high-craft UX, lab-first tests & visual docs"
kind: epic
status: done
priority: 3
depends_on: []
paths: []
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-18T22:19"
updated: "2026-11-19T06:45"
---

# Goal
Turn the working `0.1.0` tree into a shippable one: real code only (`43,271` src LOC / `43.77%` line coverage baseline), cohesive `<=500`-LOC structs, zero mock/fake doubles, live `>200ms` progress, dynamic tables, two rich lab E2E journeys, and visual docs — all proven on the live lab VM.

# Context
Six serialized vertical slices, each pre-baked with exact `file:line` anchors from the completed code / coverage / UX audits. Workers must NOT re-research; every target is already located.

Ordering is a hard dependency chain because every slice edits shared wiring (`Cargo.toml`, `Cargo.lock`, `crates/**`):
`M3.30.1` purge → `M3.30.2` god-file splits → `M3.30.3` terminal UX → `M3.30.4` tests/Makefile → `M3.30.5` docs → `M3.30.6` live VM proof.

Absorbed / superseded work: `M3.27` and `M3.28.24` are already in `HEAD`; `M3.28.13`, `M3.28.14`, `M3.29.5` fold in here.

# Deliverables
- Dead weight, forked `snow` resolver, single-impl mock traits, 14 recording/fake doubles, support-matrix bureaucracy and duplicated helpers deleted (`M3.30.1`).
- `daemon.rs` / `btrfs_engine.rs` / `main.rs` split into cohesive OOP structs; `slipfork-edge` layering fixed; hand-rolled HTTP/NDJSON wheels replaced; `PairingStore` `<state_dir>/ospp.json` persistence (`M3.30.2`).
- `TerminalUi` semantic palette, `indicatif` live progress on every `>200ms` op, ANSI-stripped dynamic column widths, verdict-first `doctor` (`M3.30.3`).
- One rich Btrfs world E2E journey + one rich OSPP loopback journey + pure algorithm tests; single-build `Makefile`; no nested `cargo build` from tests (`M3.30.4`).
- Visual `README`/`docs` with quickstart, Mermaid diagram, `!!!` admonitions, single clean `0.1.0` `CHANGELOG` entry (`M3.30.5`).
- Live droplet `165.232.86.174` proof: full E2E green under a real PTY with color + spinners, `:80`-less `.localhost` URL, hot-restart, fork/ps/logs/open/drop/down, plus before/after `llvm-cov` LOC + coverage numbers (`M3.30.6`).

# Acceptance
- `uv run --no-project tools/plan.py validate` exits `0`.
- `make check` green once at the epic gate (single workspace build).
- `make docs-check` + `uvx --offline zensical build --strict` green.
- `tools/do_vm.py test` green on `165.232.86.174`, agent leaves stock `0.1.0` binaries and the `demo` world up on `:80`.
- Zero `RESULT.md` requirements anywhere; proof lives in `LOG.md` / `JOURNAL.md`.
