---
name: Dispatch & Triad Review Gate
description: How Slipfork agents spawn subagents, run the parallel Triad Review Gate (review-arch + review-tests + review-ux), filter cosmetic nits into at most 1 consolidated fix pass, and escalate cleanly without RESULT.md bloat.
---

# Dispatch Protocol & Triad Review Gate

## Routing Table

| Agent | Tier / Model | Responsibility | Can spawn |
|---|---|---|---|
| `conductor` | cheap (`deepseek-v4.1-flash#high`) | Top-level loop across `plan/ROADMAP.md` on `release/<version>` branches; halts on open `direction-challenge`/`missing-input` blockers; opens Release PRs into `develop`. Never edits code/docs. | `lead`, `planner`, `strategist`, `architect`, `expert`, `researcher`, `review-arch`, `review-tests`, `review-ux` |
| `lead` | cheap (`deepseek-v4.1-flash#high`) | Orchestrates one subtree (`ROOT=<id>`). Runs `execute -> Triad Review Gate -> explicit-path commit`, caps nit churn to <=1 consolidated fix pass, runs targeted crate tests per slice and `make check` at epic gate. | `lead`, `implementer`, `writer`, `review-arch`, `review-tests`, `review-ux`, `planner`, `researcher`, `architect`, `expert`, `strategist` |
| `planner` | cheap (`deepseek-v4.1-flash#high`) | Decomposes epics into 2–6 coarse vertical slices with pre-baked `file:line` findings and serialized `--depends` on shared files in <=4 calls. | `researcher`, `explore` |
| `implementer` | cheap (`deepseek-v4.1-flash#high`) | Implements one vertical code/UX slice + consolidated real lab E2E tests in 4–7 batched calls. Blocks on missing specs; splits if >=4 subsystems. | `explore`, `researcher` |
| `writer` | cheap (`deepseek-v4.1-flash#high`) | Authors upstream-grounded `docs/design/*.md` specs and visual Zensical `docs/**` pages (`plan.py` `owner: doc-writer` / `designer` routes here) in 3–5 calls. | `explore`, `researcher` |
| `researcher` | cheap (`deepseek-v4.1-flash`) | Investigates Btrfs ioctls, Linux namespaces/process groups, OSPP/runner specs -> `docs/research/` & `LESSONS.md`. | `explore` |
| `review-arch` | cheap (`deepseek-v4.1-flash`) | **Triad Reviewer #1:** Full-repo Constructor IoC/DI, single Composition Root, cohesive structs `<=500` LOC, canonical module ownership, zero single-impl mock traits. | — |
| `review-tests` | cheap (`deepseek-v4.1-flash`) | **Triad Reviewer #2:** Full-repo + coverage lens; zero mocks (except OSPP ref server), consolidated lab E2E boots, algorithm-only unit tests, 0%-covered fallback deletion. | — |
| `review-ux` | cheap (`deepseek-v4.1-flash`) | **Triad Reviewer #3:** ANSI hierarchy + `NO_COLOR`/`--json` purity, live spinners/progress on `>200ms` ops, dynamic column widths, visual Mermaid/admonition docs, zero vaporware. | — |
| `architect` | **expensive** (`gemini-4-carbon#medium`) | Core DX surfaces, Upward Constructor DI architecture, ADRs, and `DIRECTION_CHALLENGE` escalations. | `researcher`, `review-arch`, `review-ux`, `explore` |
| `strategist` | **expensive** (`gemini-4-carbon#medium`) | Owns `plan/ROADMAP.md`, milestone gates (`make check` + Triad sign-off), and scorched-earth subtree replans. | `planner`, `researcher`, `review-arch`, `review-tests`, `review-ux`, `explore` |
| `expert` | **expensive** (`gemini-4-carbon#high`) | Last-resort solver for hard Linux namespace, `-pgid` reaping, Btrfs CoW, or Rust concurrency bugs. | `explore`, `researcher` |

## Parallel Triad Review Gate (`review <task>`)

For every slice in `review` status, `lead` runs targeted slice checks (`cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings` + targeted `cargo test -p <crates>` or `zensical build --strict`, `guard.py`, `critical_guard.py`) and spawns **all 3 Triad reviewers in parallel**:
1. `review-arch`
2. `review-tests`
3. `review-ux`

- **`SPEC_BLIND_SPOT` or `MISSING_INPUT`:** Halt immediately (`set <id> blocked` + `plan.py human add --kind direction-challenge | missing-input` or escalate to `architect`) — never retry cheap workers to invent workarounds!
- **Anti-Churn Triage (`<= 1` Fix Pass):** `lead` filters out cosmetic/contradictory nits (casing bike-shedding, unrequested type renames, `RESULT.md` demands) and dispatches **at most 1** consolidated `<= 40`-line fix brief for real `BLOCKER`/`MAJOR` bugs. If a hard systems defect persists, escalate to `expert`.
- **Full `make check` runs ONCE at `gate <epic>`** (and milestone/release gates), keeping per-slice feedback fast.

## Brief Template (Parent -> Worker)

```text
ROLE: you are <implementer|writer|...> for Slipfork node <ID>.
NODE: plan/nodes/.../<ID>/NODE.md  (pre-baked context + # Acceptance)
ATTEMPT: <n>/<max>. <If n>1: consolidated <=40-line surgical fix list>
SCOPE: touch ONLY <paths from NODE.md>. Write NO RESULT.md!
LAWS: True Constructor IoC/DI (zero globals, zero single-impl mock traits), extend canonical module, zero mocks except OSPP loopback ref server, rich ANSI/spinner/dynamic-width UX, block immediately on missing external specs.
REPORT: reply ONLY with a <= 20-line report block.
```

## Report Template (Worker -> Parent)

```text
STATUS: done | needs_split | blocked | failed
BLOCK_KIND: none | missing-input | direction-challenge | compat-question
NODE: <ID>
SUMMARY: <2-4 lines>
EVIDENCE: <commands run + exit status / test counts>
FILES: <changed paths, comma-separated>
BLOCKER: <if blocked/failed: exact missing input or DI conflict + options>
SPLIT: <if needs_split: 2-4 crate-DAG-ordered vertical slices>
```
