# Slipfork (`slipfork`): Engineering Constitution & Agent Rules

Slipfork (`0.1.0`) is a **DX-first copy-on-write Btrfs environment** for modern polyglot applications and AI coding agents ("your cloud environment locally in a box; CI is the clutch"). From a single `snow.yml` manifest (`modes.all`, `modes.local`, `modes.cloud` + `!!` auto-generated secrets via the Open Secret Provider Protocol), Slipfork runs shared-UID local worlds behind `.localhost` and forks running worlds in milliseconds as Btrfs subvolume snapshots when agents open worktrees. (`0.2.0` adds the parallel STP v1 **Artifact + Fixture Snapshot DAG** test orchestrator.)

When `plan/AUTOPILOT` is `on`, `deepseek-v4.1-flash` executes coarse vertical slices (`implementer`, `writer`, `planner`, `lead`) and runs the parallel **Triad Review Gate** (`review-arch` + `review-tests` + `review-ux`); `gemini-4-carbon` (`architect`, `strategist`, `expert`) handles upward architecture crossroads, milestone gates, and hard kernel/systems rescues.

---

## Five Non-Negotiable Engineering Laws

1. **True Constructor IoC/DI, Cohesive OOP & Canonical Module Ownership (`review-arch`):**
   - **Single Composition Root:** Only `crates/slipfork/src/main.rs` and `packages/opencode-slipfork/src/index.ts` read `std::env` / `process.env`, probe `/proc`, or bind ambient OS sockets/filesystems. Library crates have **zero** `std::env::var`, `OnceLock`/`LazyLock`, or `static` mutable/atomic state.
   - **Cohesive Structs (`<= 500` LOC) Over Free-Function Galaxies:** Group related state + behavior into cohesive structs (`SupervisedProcess`, `WorldManager`, `FileVault`) with methods — never 6-arg free functions or `*Inputs` parameter bags.
   - **Architectural Port Traits ONLY — Zero Single-Impl Mock Traits:** Define traits **strictly** at genuine architectural ports (`WorldEngine`, `SecretProvider`, `SubvolumeDriver`, `UnitPort`, `TunnelRegistrar`, `VcsWorkspace`). **NEVER** invent single-impl mock-only traits (`MonotonicClock`, `HandoverClock`, `HostLoadProbe`, `FakeClock`) when a plain value struct (`Duration`, `StopBudget`) suffices.
   - **Extend Canonical Modules Instead of Leaking Internals:** Keep all syntax parsing, token/AST traversal, fallback evaluation, secret-taint rules, and framing strictly inside their canonical owner module (`slipfork-snow`, `wire`, `relay`, `slipfork-edge`; keep `slipfork-edge` pure of CLI tables; reuse `Cargo.toml` crates like `tempfile`, `httparse`, `serde_json`). Add predicate/visitor helpers inside the canonical module rather than re-scanning `${{` delimiters, HTTP headers, or tokens downstream.
   - **Think Upwards:** If a cross-backend DI conflict prevents a clean non-leaky tree, **STOP** and escalate `STATUS: blocked` (`BLOCK_KIND: direction-challenge`) rather than hacking around a leaky port.

2. **Lab-First E2E Testing & Coverage Economy — Zero Mocks (`review-tests`):**
   - Test with real disk directories, real `git`/shell subprocesses, real Btrfs/daemon workflows, and (for OSPP) the minimal `127.0.0.1:0` loopback reference server. **NEVER** write `Mock*`, `Fake*`, `Recording*`, `Stub*`, `MemorySecretProvider` shims, or `chmod 0o555` `TestTreeDriver` theater.
   - Consolidate slow daemon/world boots into 1 rich, cohesive E2E lifecycle journey (reaping worlds and `-pgid` process groups cleanly). Write unit tests strictly for subtle pure algorithms (DAG cycles/waves, Merkle hashing, `snow.yml` secret taint). Delete 0%-covered fallback dead code (`LayeredSecretProvider`, `.ok().unwrap_or_default()`) instead of testing it.
   - Protected critical tests (`crates/*/tests/critical_*.rs` and `**/tests/critical/**`) are guarded by `tools/critical_guard.py`; never weaken or delete them unless authorized in your node's `paths`.

3. **High-Craft CLI & Scannable Visual Docs (`review-ux`):**
   - Compute dynamic visible column widths (`.max()`, stripping ANSI when measuring) — never shear long IDs with hardcoded `{:<16}`/`{:<24}`. Style primary URLs bold/cyan, `running` green, `failed` red, and 12-char digests dimmed; emit **zero** `\x1b[` under `NO_COLOR` / non-TTY or `--json`.
   - Never swallow slow (`> 200ms`) child progress in blocking `.output()`, silent `sleep` loops, or `pull -q`: `spawn()` and stream live lines with `[step/total]` phase headers, spinners, and elapsed time. Pin `GIT_AUTHOR_*`/`GIT_COMMITTER_*` on internal git plumbing, format errors as `1 line + next:` remedy, lead `doctor` with `✓ Ready` / `✗ N issue(s)`, omit `:80` on port-80 URLs, warn on stderr when ephemeral (`sudo slipfork daemon install`), and gate `[resources]` jargon behind `-v`.
   - Docs & specs: lead with user value + a working copy-pasteable snippet in the first 25 lines, use Mermaid diagrams + Zensical `!!!` admonitions + `<= 2`-sentence paragraphs, and ban internal `ADR-*` / `M3.*` lawyer prose.

4. **Anti-Vaporware, Upstream Grounding & Pre-`1.0` Scorched-Earth Deletion:**
   - Read `Cargo.toml`, `package.json`, `ARCHITECTURE.md`, and `vendor/` BEFORE designing or coding.
   - If a required external daemon/webhook/wire spec is missing, **STOP immediately** (zero `[derived]`/`[guessed]` markers, zero `Unavailable*` CLI vaporware, zero degraded fallbacks) and return a `<= 12`-line `STATUS: blocked` (`BLOCK_KIND: missing-input`).
   - Published `docs/` (`zensical.toml`) and `README.md` describe **strictly what runs in the shipped binary** (`tools/check_docs_honesty.py`); pre-code DX blueprints live in `docs/design/dx-*.md`.
   - Pre-`1.0`: delete superseded commands/types/tests/docs atomically with zero legacy aliases, zero `#[deprecated]`, and zero `"Option A vs Option B"` pivot archaeology.

5. **Adaptive Scope, Triad Review Gate & Batched Execution Economy:**
   - **1-Call Discovery & Complete Writes:** Batch initial discovery into **one** command (never snoop `.opencode/**`, `candidate_meta.json`, `tools/*.py` source, `.gitignore`, or `git log`), write complete warning-free files in 1 `write` per file over micro-`edit` loops, and write **zero `RESULT.md` essays**.
   - **Coarse Vertical Slices & Triad Gate:** Plan 2–6 cohesive vertical slices per epic (1 writer per worktree; serialize shared `cli.rs`/`main.rs`/`Cargo.toml` edits via `--depends`; return `STATUS: needs_split` if a leaf explodes across `>= 4` subsystems). `lead` runs the parallel **Triad Review Gate** (`review-arch` + `review-tests` + `review-ux`), filters cosmetic nits into at most 1 consolidated fix pass, runs targeted crate tests per slice, and runs full `make check` once at the epic gate.

---

## Repository Safety & Workflow Rules

- **Plan & Path Locks:** Mutate `plan/` **exclusively** via `uv run --no-project tools/plan.py ...` (skill `plan-system`). Touch only globs in your node's `paths`.
- **Disk, Never `/tmp` `tmpfs`, for Heavy State:** `/tmp` on Linux runners is RAM-backed `tmpfs`. Keep worktrees under `.worktrees/`, CoW snapshots/volumes under `~/.cache/slipfork`, `.slipfork/`, or `/var/tmp`, and use `/tmp/opencode/` only for tiny short-lived files.
- **Python & Rust Execution:** Bare `python`, `python3`, and `pip` are denied. Use `uv run --no-project` for `tools/*.py` and `poc/*.py`, `uvx --offline` for `ruff` and `zensical`, and `cargo` / `make` targets for Rust.
- **Confidentiality:** Never mention proprietary client or sibling private repository names in code, docs, tests, comments, or commit messages (`snow.yml` and Kerfufoo's public Snow reference are public specs).
- **Git & Release PR Workflow:** Work on `release/<version>` branches; only `lead` and `conductor` commit using explicit paths (`git add -- <explicit files>`). Never push directly to `develop`, force-push, tag, or `gh pr merge`.

## Where Things Are

| Path | What |
|---|---|
| `plan/` | Autonomous plan tree: `ROADMAP.md`, `STATE.md`, `HUMAN.md`, `ESCALATIONS.md`, `JOURNAL.md`, `BUDGET.json`, `AUTOPILOT`, `nodes/` |
| `docs/VISION.md` | North Star vision, DX-first mandate, and milestone scope (excluded from public Zensical site) |
| `ARCHITECTURE.md` | Living technical specification: Composition Root, Constructor DI Tree, port traits, wire protocols, and invariants |
| `docs/` & `README.md` | Lean, visual public user documentation (`zensical.toml` -> `make docs-check` + `tools/check_docs_honesty.py`) |
| `docs/design/` | Pre-code DX blueprints (`dx-*.md`) & `<= 150`-line technical/protocol specs |
| `docs/decisions/` | 1-page Architecture Decision Records (`ADR-NNNN-<slug>.md`) |
| `docs/research/` | Primary-source research & real CoW benchmark reports (`m1-poc-benchmark.md`) |
| `docs/knowledge/` | `LESSONS.md` — concise host, Rust, Linux namespace/process-group, Btrfs subvolume, AppArmor, and OpenCode facts |
| `crates/` | Rust workspace (`slipfork-world`, `slipfork-snow`, `slipfork-dag`, `slipfork-engine`, `slipfork-edge`, `slipfork-vcs`, `slipfork-ospp`, `slipfork`); `slipfork-testkit` & `slipfork-ospp-testkit` are test-only lab harnesses |
| `packages/opencode-slipfork/` | OpenCode V2 plugin (`ctx.worktree.transform` + `ctx.shell.hook` + `ctx.session.hook`) |
| `poc/` | Real PostgreSQL 18 + Linux `netns` + Playwright M1 PoC benchmark harness |
| `tools/` | Stdlib-only harness & honesty guards (`plan.py`, `guard.py`, `critical_guard.py`, `check_docs_honesty.py`, `check_support.py`, `rs_test_list.py`) |

## Skills Index (load with `skill`)

| Skill | Load when |
|---|---|
| `dx-first` | Designing CLI/`snow.yml` UX, writing `docs/**` or `docs/design/dx-*.md`, or running `review-ux` |
| `engineering-laws` | Designing, coding, planning, or reviewing anything (Five Non-Negotiable Laws & Triad Gate) |
| `plan-system` | Reading or mutating `plan/` via `tools/plan.py` |
| `dispatch` | Spawning subagents, running the Triad Review Gate, triaging findings, or escalating |
| `rust-standards` | Writing or reviewing Rust crates (`crates/*`) |
| `testing` | Writing consolidated lab E2E / protected critical tests or running `review-tests` |
| `git-workflow` | Staging explicit files, running slice vs epic-gate checks, committing, or opening Release PRs |
