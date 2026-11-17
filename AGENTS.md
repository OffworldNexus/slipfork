# Slipfork (`slipfork`): rules for every agent

Slipfork is a **DX-first copy-on-write environment** for modern polyglot applications and AI coding agents. From a single `snow.yml` manifest (`modes.all`, `modes.local`, `modes.cloud` + `!!` auto-generated secrets via an open Secret Provider Protocol), Slipfork builds shared-UID local development worlds and forks running worlds in milliseconds as Btrfs subvolume snapshots when agents open worktrees. `0.1.0` ships Btrfs worlds plus the `snow.yml` resolver and the `dag plan` planner. The parallel test orchestrator is `0.2.0`: it targets heavy multi-component suites (`pytest`, `Playwright`/`Vitest`, `PHPUnit`, `cargo test`, `blastit check`) across a content-addressed **Artifact + Fixture Snapshot DAG**.

The project runs autonomously when `plan/AUTOPILOT` is `on`: a cheap model (`deepseek-v4.1-flash`) executes leaf tasks and narrow-lens goldfish reviews; an expensive model (`gemini-4-carbon`) runs architecture crossroads, upward direction checks, milestone gates, and hard rescues.

**Read these before doing anything:**

1. `docs/VISION.md`: why Slipfork exists and why **everything stems from Developer Experience**.
2. `ARCHITECTURE.md`: the **Composition Root & DI Tree** governing every crate, trait, and adapter.
3. `plan/STATE.md`: current plan & open `HUMAN.md` items (`uv run --no-project tools/plan.py state`).
4. Your assigned `NODE.md`, plus `docs/knowledge/LESSONS.md`.

## The Three Engineering Iron Laws (learned the hard way — enforced by dedicated reviewers)

1. **Zero "Illegal" Solutions — Block on Missing Inputs (`review-legality`):**
   - Initiative is good **only** when the resulting code is genuinely useful and clean afterwards.
   - **NEVER** invent specs, designs, `[derived]`/`[guessed]` placeholders, fake/in-memory production fallbacks, `os.environ` side-doors, or monkey-patches around missing credentials, missing host capabilities (a non-Btrfs state directory, a missing secret key), or upstream library contracts.
   - Check `Cargo.toml`, `pyproject.toml`, and `package.json` before coding: never hand-roll what a workspace crate or declared dependency already provides.
   - When a required external input, credential, host daemon, or specification constraint is missing or unreachable: **STOP immediately** and return `STATUS: blocked` so `lead`/`conductor` files `plan.py human add --kind missing-input`.
2. **Strict IoC / DI & Mandatory Upward Direction Challenge (`review-ioc`):**
   - **Zero global/mutable state**, zero singletons, and zero direct `std::env::var`, filesystem, clock, network, or subprocess calls outside thin adapter implementations wired at the **single Composition Root**.
   - Collaborators depend strictly on small, honest capability traits (`WorldEngine`, `SecretProvider`, `ArtifactStore`, `TestAdapter`, `TunnelRegistrar`, `VcsWorkspace`) — never concrete cross-layer structs or `match backend_kind` ladders.
   - **Think UPWARDS before coding downwards:** Integrating a new backend, language, or feature must challenge whether the global DI tree and specification cover all directions cleanly. **If you cannot draw a clean, non-leaky DI tree, there is a blind spot in the specification.** Do NOT build a parallel pipeline or hack around a leaky trait — stop and escalate a `direction-challenge` (`plan.py human add --kind direction-challenge`) for human architectural feedback.
3. **SemVer Honesty: Zero Pre-`1.0` Backward Compatibility & Zero Fallback Cruft (`review-cruft`):**
   - Workspace version `< 1.0.0` carries **zero backward-compatibility promises** unless explicitly requested by the human.
   - **One Canonical Way, No Fallbacks:** When an idea or syntax is superseded, **delete** the old code, config keys, tests, docs, and plan nodes in the same sweep. Never preserve legacy aliases, dual syntaxes, deprecated shims, or degraded fallback modes ("try Btrfs, else silently fall back to a plain host directory"). In doubt about breaking an already-used `0.x` contract, **ask the human** — never add a compatibility shim preemptively.

## Core workflow rules

4. **Developer Experience First — Zero Vaporware in Published Docs.** Draft and critique target DX specs in `docs/design/dx-<slug>.md` (excluded from the public Zensical site) *before* writing internal designs or Rust/TS code; promote pages and commands into published `docs/` and `README.md` **only** when the real feature exists and runs. Load skill `dx-first`.
5. **Mandatory Quad Review Gate on Code & Design Tasks.** Every implementation or design task must pass:
   - `reviewer` (fresh-context functional review: re-runs all `# Acceptance` commands + `make check` + `guard.py` + `critical_guard.py`), **plus**
   - the three narrow-context goldfish smell reviewers in parallel: **`review-legality`**, **`review-ioc`**, and **`review-cruft`**.
6. **The plan lives on disk (`plan/nodes/`).** Mutate it **only** via `uv run --no-project tools/plan.py ...` (skill `plan-system`). `plan.py set <id> active` mechanically blocks overlapping `paths` across concurrent active tasks — never bypass it.
7. **Stay in your lane & stage explicit paths.** Touch only globs in your node's `paths`. When committing (`lead`/`conductor` only), run `git add -- <explicit files>`, never `git add .` or broad directory globs.
8. **Disk, never `/tmp` tmpfs, for heavy state.** `/tmp` on Linux runners is RAM-backed `tmpfs`. Keep worktrees under `.worktrees/`, CoW snapshots/volumes under `~/.cache/slipfork` or `.slipfork/`, and scratch builds on disk. Use `/tmp/opencode/` only for tiny short-lived files.
9. **Python & Rust execution:** Bare `python`, `python3`, and `pip` are denied. Use `uv run --no-project` for `tools/*.py` and `poc/*.py`, `uvx --offline` for `ruff` and `zensical`, and `cargo` / `make` targets for Rust.
10. **Confidentiality:** Never mention proprietary client or sibling private repository names in code, docs, tests, comments, or commit messages. (`snow.yml` and Kerfufoo's public Snow reference are public specs.)
11. **Protected critical tests:** `crates/*/tests/critical_*.rs` and `**/tests/critical/**` are guarded by `tools/critical_guard.py`. Never weaken or delete them unless authorized in your node's `paths`.
12. **Git & Release PR workflow:** Work on `release/<version>` branches (`lead` and `conductor` commit; `conductor` opens PRs into `develop` and pauses for human merge). Never push directly to `develop`, never force-push, never `git tag`, never `gh pr merge`.

## Where things are

| Path | What |
|---|---|
| `plan/` | Autonomous plan tree: `ROADMAP.md`, `STATE.md`, `HUMAN.md`, `ESCALATIONS.md`, `JOURNAL.md`, `BUDGET.json`, `AUTOPILOT`, `nodes/` |
| `docs/VISION.md` | North Star vision, DX-first mandate, and Milestone 1–4 scope (excluded from public Zensical site) |
| `ARCHITECTURE.md` | Living technical specification: Composition Root, DI Tree, traits, wire/API protocols, and invariants |
| `docs/` & `README.md` | Lean dream-tool public user documentation (`zensical.toml` -> `make docs-check`) |
| `docs/design/` | Technical design docs (`*.md`) + goldfish critic artifacts (`*.review-<lens>.md`) |
| `docs/decisions/` | Architecture Decision Records (`ADR-NNNN-<slug>.md`) |
| `docs/research/` | Cited hypervisor/CoW/framework research & measured benchmark reports (`m1-poc-benchmark.md`) |
| `docs/knowledge/` | `LESSONS.md` — concise host, Rust, Linux namespace, Btrfs subvolume, AppArmor, and OpenCode facts |
| `crates/` | Rust workspace (`slipfork-world`, `slipfork-snow`, `slipfork-dag`, the `BtrfsEngine` in `slipfork-engine`, `slipfork-edge`, `slipfork-vcs`, `slipfork-ospp`, `slipfork`): one unified Btrfs subvolume per world, snapshotted and cloned through `SubvolumeDriver`; `slipfork-testkit`/`slipfork-ospp-testkit` are test-only scaffolding |
| `packages/opencode-slipfork/` | OpenCode V2 plugin (`ctx.worktree.transform` + `ctx.shell.hook` + `ctx.session.hook`) |
| `poc/` | Real PostgreSQL 18 + Linux `netns` + Playwright M1 PoC benchmark harness |
| `tools/` | Stdlib-only harness tools (`plan.py`, `guard.py`, `critical_guard.py`) + `tools/tests/` |

## Skills index (load with `skill`)

| Skill | Load when |
|---|---|
| `dx-first` | Planning, writing docs, designing CLI/`snow.yml` UX, or reviewing any user-facing surface |
| `engineering-laws` | Designing, coding, or reviewing anything (Zero Illegal Solutions, Strict IoC/DI + Upward Challenge, Zero Pre-1.0 Compat) |
| `plan-system` | Reading or mutating `plan/` via `tools/plan.py` |
| `dispatch` | Spawning subagents, writing briefs/reports, running the Quad Review Gate, or escalating |
| `rust-standards` | Writing or reviewing Rust crates (`crates/*`) |
| `testing` | Writing critical tests, tool tests, or running `test-curator` audits |
| `git-workflow` | Staging explicit files, running pre-commit gates, committing, or opening Release PRs |
