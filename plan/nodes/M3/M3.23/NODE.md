---
id: M3.23
title: "Docs hygiene gate: purge Option A/B and Spike A/B framing plus stale post-pivot engine residue from ARCHITECTURE.md, README.md, docs/, crates/, tools/"
kind: task
status: active
owner: implementer
priority: 5
depends_on: [M3.11, M3.18, M3.21, M3.22]
paths:
  - ARCHITECTURE.md
  - README.md
  - "docs/**"
  - "crates/**"
  - "tools/**"
  - "packages/**"
  - Cargo.toml
  - Cargo.lock
  - CHANGELOG.md
  - zensical.toml
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-16T18:43"
updated: "2026-11-17T13:42"
---

# Goal

Make the owner's terminology rule mechanically true repo-wide: zero `Option A` / `Option B` / `Spike A` / `Spike B` framing in `ARCHITECTURE.md`, `README.md`, `docs/`, `crates/`, and `tools/`, so nothing in the repository implies a second architecture exists or was ever weighed.

# Context

- OWNER RULE (2026-11-16): "Don't call it Option A in the docs. Forget there was another option; just say it's what we're doing." Slipfork has exactly one architecture: `BtrfsEngine` over one unified Btrfs subvolume per world + Linux `userns`/`netns`/`ipcns`/`pidns`/`mntns` + `pivot_root` + daemonless static `crun`.
- Known offenders at creation time (`grep -riE 'option [ab]|spike [ab]'`): `ARCHITECTURE.md` §1 heading/§1 bullet/§2 table/§4 bullet plus `H-009`'s ratified label; `docs/VISION.md` M3 row; `ADR-0010` title/context/decision/ruling-banner; `ADR-0002` status line; `ADR-0007` `H-009` label occurrences; historical critic artifacts under `docs/design/*.review-*.md`; and `tools/tests/test_tools.py` fixture prose.
- `docs/research/**` and `docs/knowledge/LESSONS.md` are swept by `M3.11`; do not touch them here.

# Design / Invariants

1. Describe the architecture directly ("the unified Btrfs subvolume world engine", "`BtrfsEngine`", "the Btrfs + `crun` substrate"). Delete alternative-architecture history rather than paraphrasing it.
2. The ratified `H-009` support-hygiene ruling must be described by meaning ("the ratified `H-009` user-reachable scope rule"), never by an option letter. `plan/HUMAN.md` keeps its own historical label — it is out of scope.
3. Historical critic artifacts under `docs/design/*.review-*.md` whose recommendations are already adjudicated may be deleted; anything kept must be re-stated without option framing.
4. `tools/tests/test_tools.py` fixture strings that merely exercise the `HUMAN.md` parser may use neutral wording (e.g. `Use local mode.`) as long as the test still covers the same branch.
5. No behaviour change: `make check` and `make docs-check` stay green; `tools/guard.py` and `tools/check_docs_honesty.py` must not regress.
6. **Stale post-pivot design residue (added after the `M3.7` IoC pass):** `docs/design/**` must describe the one Btrfs-only architecture. Delete or rewrite every stale row naming deleted engines, the old artifact-volume cloner seam, or the superseded per-language adapter contract (e.g. `docs/design/m1-core-and-poc.md`), leaving the `M3.8`/`M3.9`/`M3.10` rewrites intact. Crate rows in `ARCHITECTURE.md` that name a not-yet-existing `crates/slipfork-test` must stay labelled `M3.2` target state.

# Acceptance

```bash
! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/
uv run --no-project tools/plan.py validate
uv run --no-project tools/guard.py
uv run --no-project tools/check_docs_honesty.py
make docs-check
make check
```

Expected: the inverted `grep` prints nothing and exits 0; `plan.py validate`, `guard.py`, `check_docs_honesty.py`, `make docs-check`, and `make check` all exit 0.
