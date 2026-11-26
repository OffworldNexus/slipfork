---
id: M3.144
title: "One Report layer: typed outcomes own state and palette, no disjoint prints"
kind: task
status: done
owner: architect
priority: 3
depends_on: []
paths:
  - "specs/02-cli-ux.md"
  - "crates/slipfork/src/ui/**"
  - "crates/slipfork/src/boot/**"
  - "crates/slipfork/src/commands/**"
  - "crates/slipfork/src/daemon/reconcile/**"
  - crates/slipfork/src/main.rs
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-25T03:10"
updated: "2026-11-25T09:43"
---

# Owner direction

"Your display code must be full of spaghetti... There should be a class that exposes a nice API which matches up what we're trying to express, not disjoint insane prints."

Evidence: a failed build printed `✓ [1/3] image api built 7 steps <1ms` and then `error: build api failed` in **plain** text (no palette), with five blank lines and the wrong `logs` remedy. The same row vocabulary is produced by at least four code paths (`boot/` renderer, `ui/palette`, `daemon/reconcile/{mod,progress}`, `daemon/client` mistakes, `docker` printer, bare `eprintln!` in `main.rs`), so state/palette drift is structural.

# Contract to design (specs/02 §"One Report Layer")

- One typed vocabulary for what the user sees: `Report` (an ordered set of `Section`/`Row`), `Row` with `icon | counter | keyword | target | state | command | timing | id`, and `Outcome::{Ok, Cached, Failed{at}, Skipped, Refused}`. **State text and icon are derived from `Outcome`**, never passed as a free string.
- One renderer owns the palette, terminal capability, width computation and blank-line policy; every command (`up`, `fork`, `drop`, `docker build/pull`, `logs -r`, `status`, `ps`, `doctor`) builds a `Report` and hands it to it. `--json` serializes the same typed events (no second formatter).
- Failures are a value: exactly one `error:` + one `next:` from the `Failure`/`Outcome`, emitted by the renderer with the approved roles; no path may print `error:` itself.
- Grep gate: user-facing `println!`/`eprintln!` outside the report renderer is forbidden (help text and `--json` serialization exempt).
- Streaming stays live: `stream()` rows for child output keep the gutter/indent rules, and progress/bar state comes from the same report.

# Slices

Design first (this node): the spec section + a migration map (which call sites become `Report`s, which prints are deleted, what the `Outcome` enum is), then 3–4 implementer slices in crate order with disjoint paths.

# Acceptance

- Spec section is DX-first, ≤2-sentence paragraphs, with a Mermaid flow of `event → Report → renderer → terminal|json`.
- No ambiguity left for a planner: `Outcome` variants listed with their icon/colour/state text, the blank-line rule, and the width algorithm.
- `guard.py` + `check_docs_honesty.py` green; commit `[M3.145] ...` (no push).
