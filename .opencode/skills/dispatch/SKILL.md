---
name: Dispatch & Quad Review Gate
description: How Slipfork agents spawn subagents, run the mandatory Quad Review Gate (reviewer + review-legality + review-ioc + review-cruft), respect path locks, and escalate cleanly.
---

# Dispatch Protocol & Quad Review Gate

## Routing Table

| Agent | Tier / Model | Responsibility | Can spawn |
|---|---|---|---|
| `conductor` | cheap (`deepseek-v4.1-flash#high`) | Top-level loop across `plan/ROADMAP.md` on `release/<version>` branches; halts on open `direction-challenge`/`missing-input` blockers; opens Release PRs into `develop`. Never edits code/docs. | `lead`, `planner`, `strategist`, `architect`, `expert`, `researcher`, `test-curator`, `reviewer` |
| `lead` | cheap (`deepseek-v4.1-flash#high`) | Orchestrates one subtree (`ROOT=<id>`). Runs `execute -> Quad Review Gate -> commit` and epic gates. | `lead`, `planner`, `doc-writer`, `designer`, `implementer`, `researcher`, `reviewer`, `review-legality`, `review-ioc`, `review-cruft`, `critic`, `test-curator`, `architect`, `expert`, `strategist` |
| `planner` | cheap (`deepseek-v4.1-flash#high`) | Decomposes containers into DX-first -> DI-design -> leaf code tasks (≤ 500 LOC) with disjoint `paths`. | `researcher`, `explore` |
| `doc-writer` | cheap (`deepseek-v4.1-flash#high`) | Authors lean Zensical user docs (`docs/**`, `README.md`) before & after code; runs `dx` + `ai-ergonomics` critics. | `critic`, `researcher`, `explore` |
| `designer` | cheap (`deepseek-v4.1-flash#high`) | Drafts scoped crate designs (`docs/design/*.md`) with mandatory `## Composition Root & DI Tree`; runs critics. | `critic`, `researcher`, `explore` |
| `implementer` | cheap (`deepseek-v4.1-flash#high`) | Implements one leaf task + protected critical tests. Blocks immediately on missing inputs or DI blind spots. | `explore`, `researcher` |
| `researcher` | cheap (`deepseek-v4.1-flash`) | Investigates Incus APIs, Linux namespaces/CoW, test runners -> `docs/research/`. | `explore` |
| `reviewer` | cheap (`deepseek-v4.1-flash#high`) | Functional goldfish reviewer: re-runs all `# Acceptance` commands + `make check` + `guard.py` + `critical_guard.py`. | `explore` |
| `review-legality` | cheap (`deepseek-v4.1-flash`) | **Smell Reviewer #1:** Hunts "Illegal Solutions" (invented fallbacks/guesses, monkey-patches, ignored deps, silent degraded modes). | — |
| `review-ioc` | cheap (`deepseek-v4.1-flash`) | **Smell Reviewer #2:** Hunts IoC/DI violations (global state, ambient env/IO, `match kind` sprawl) & `SPEC_BLIND_SPOT`s needing upward human feedback. | — |
| `review-cruft` | cheap (`deepseek-v4.1-flash`) | **Smell Reviewer #3:** Hunts pre-`1.0` backward-compat shims, dual syntaxes, leftover dead code/docs from scrapped ideas, and duplication. | — |
| `critic` | cheap (`deepseek-v4.1-flash#high`) | Single-lens design/docs critic (`dx`, `ai-ergonomics`, `ioc-upward`, `legality`, `cruft`, `implementability`, `testability`). | `explore` |
| `test-curator` | cheap (`deepseek-v4.1-flash#high`) | Audits critical test coverage, conformance suites, and mutation kill ratios -> `docs/testing/AUDIT-*.md`. | `explore` |
| `architect` | **expensive** (`gemini-4-carbon#medium`) | Core DX surfaces, Upward DI architecture, ADRs, and `DIRECTION_CHALLENGE` escalations. | `critic`, `researcher`, `explore` |
| `strategist` | **expensive** (`gemini-4-carbon#medium`) | Owns `plan/ROADMAP.md`, milestone gates, core architecture sign-off, and subtree replans. | `planner`, `researcher`, `critic`, `test-curator`, `explore` |
| `expert` | **expensive** (`gemini-4-carbon#high`) | Last-resort solver for hard Linux namespace, CoW, Incus, or async concurrency bugs. | `explore`, `researcher` |

## Mandatory Quad Review Gate (`review <task>`)

For every code or technical-design task in `review` status, `lead` spawns **all 4 goldfish reviewers in parallel**:
1. `reviewer` (runs `# Acceptance` + `make check` + `critical_guard.py` + `guard.py`)
2. `review-legality` (given `NODE.md` + `git diff <base>` + manifests)
3. `review-ioc` (given `ARCHITECTURE.md` + `git diff <base>`)
4. `review-cruft` (given root `Cargo.toml` version + `git diff <base>`)

- **Any `SPEC_BLIND_SPOT` or `MISSING_INPUT` verdict** from `review-ioc` / `review-legality`: **DO NOT retry blindly with the cheap implementer!** Halt the node (`set <id> blocked`) and escalate upwards (`architect` or `plan.py human add --kind direction-challenge | missing-input`).
- **Any `BLOCKER` or `MAJOR` code finding**: log all merged findings to `LOG.md` and return `<id>` to `ready` (or escalate to `expert` if the same finding repeated).
- **All 4 `PASS`**: run pre-commit gates and commit explicit paths.

## Brief Template (Parent -> Worker)

```text
ROLE: you are <agent> for Slipfork node <ID>.
NODE: plan/nodes/.../<ID>/NODE.md  (read first; source of truth)
READ ALSO: docs/VISION.md, ARCHITECTURE.md, docs/knowledge/LESSONS.md, skills <dx-first, engineering-laws, ...>
ATTEMPT: <n>/<max>. <If n>1: exact findings from previous Quad Review Gate>
SCOPE: touch ONLY <paths from NODE.md>.
IRON LAWS:
  1. Missing input/daemon/secret or contract mismatch? Return STATUS: blocked (NEVER invent a fallback, mock, or monkey-patch).
  2. Cannot fit cleanly into ARCHITECTURE.md's DI traits? Return STATUS: blocked with DIRECTION_CHALLENGE.
  3. Version < 1.0.0: zero backward-compat shims, zero dual syntaxes, delete superseded code cleanly.
DONE MEANS: every # Acceptance command run and verified; RESULT.md written.
REPORT: reply ONLY with the <= 25-line report block.
```

## Report Template (Worker -> Parent)

```text
STATUS: done | needs_split | blocked | failed
BLOCK_KIND: none | missing-input | direction-challenge | compat-question
NODE: <ID>
SUMMARY: <2-4 lines>
EVIDENCE: <one line per # Acceptance item: PASS/FAIL + command>
FILES: <changed paths, comma-separated>
FOLLOWUPS: <bullets or "none">
BLOCKER: <if blocked/failed: exact missing input or DI blind spot + concrete options>
SPLIT: <if needs_split: proposed child tasks>
```
