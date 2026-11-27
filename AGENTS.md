# Slipfork (`slipfork`): Engineering Constitution & Agent Rules

Slipfork (`0.1.0`) is a **DX-first copy-on-write Btrfs environment** for modern polyglot
applications and AI coding agents — *your cloud environment locally in a box; CI is the clutch*.
From a single `Forkfile`, Slipfork builds and runs your container images, databases, caches,
object buckets, and background workers behind one `.localhost` domain, **100% Docker-free** on
native Btrfs subvolumes and `crun`. `slipfork fork` clones code, dependencies, hash stamps, and
running databases together in milliseconds.

The clean-sheet contract is `specs/`: `01-forkfile.md` (the `# forkfile: 0.1` language),
`02-cli-ux.md` (visual status trees, live boot DAG, daily verbs), `03-under-the-hood.md` (Btrfs
layout, native image engine, per-service `crun` pods, single per-user daemon, `OSPP v1`, OpenCode
plugin). Root `TESTING_PLAN.md` is the only testing contract, and `.opencode/LESSONS.md` holds
hard-won host/kernel facts. Multi-distro Linux packaging (`.deb`, `.rpm`, pacman, APK + systemd)
is part of release scope, at parity with the sibling `tpmd` packaging.

When `plan/AUTOPILOT` is `on`, `deepseek-v4.1-flash` executes coarse vertical slices
(`implementer`, `writer`, `planner`, `lead`) and runs the parallel **Triad Review Gate**
(`review-arch` + `review-tests` + `review-ux`); `gemini-4-carbon` (`architect`, `strategist`,
`expert`) handles upward architecture crossroads, milestone gates, and hard kernel rescues.

---

## Five Non-Negotiable Engineering Laws

1. **True Constructor IoC/DI, Cohesive OOP & Canonical Module Ownership (`review-arch`):**
   - **Single Composition Root:** Only the `slipfork` CLI/daemon entry point and the
     `@slipfork/opencode` plugin entry point read `std::env` / `process.env`, probe `/proc`, or
     bind ambient OS sockets/filesystems. Library crates have **zero** `std::env::var`,
     `OnceLock`/`LazyLock`, or `static` mutable/atomic state.
   - **Cohesive Structs (`<= 500` LOC) Over Free-Function Galaxies:** Group state + behavior into
     cohesive structs (`SupervisedProcess`, `WorldManager`, `FileVault`) with methods — never
     6-arg free functions or `*Inputs` parameter bags.
   - **Architectural Port Traits ONLY — Zero Single-Impl Mock Traits:** Define traits **strictly**
     at the canonical `specs/03` Crate & Port Map ports (e.g. `WorldEngine`, `OsppClient`).
     **NEVER** invent single-impl mock-only traits
     (`MonotonicClock`, `HandoverClock`, `HostLoadProbe`, `FakeClock`) when a plain value struct
     (`Duration`, `StopBudget`) suffices.
   - **Extend Canonical Modules Instead of Leaking Internals:** `Forkfile` parsing and secret-taint
     rules stay in the `Forkfile` compiler; `Dockerfile`/OCI parsing stays in the native image
     engine; Btrfs ioctls stay in the world engine; `.localhost` routing and HTTP framing stay in
     the ingress; `OSPP v1` frames stay in the wire/OSPP module. Add predicate/visitor helpers
     inside the owner module — never re-scan `$` tokens, headers, or argv downstream.
   - **Think Upwards:** If a cross-backend DI conflict prevents a clean non-leaky tree, **STOP** and
     escalate `STATUS: blocked` (`BLOCK_KIND: direction-challenge`) rather than hacking around it.

2. **Lab-First E2E Testing & Coverage Economy — Zero Mocks (`review-tests`):**
   - Test with real disk directories (`~/.cache/slipfork`, `.slipfork/`, `/var/tmp`, or disk
     `tempfile` — never heavy state on RAM `/tmp`), real `git`/shell subprocesses, real Btrfs
     subvolumes and `crun` pods, and (for OSPP) the minimal `127.0.0.1:0` loopback reference
     server. **NEVER** write `Mock*`, `Fake*`, `Recording*`, `Stub*`, in-memory vault
     shims, or `chmod 0o555` `TestTreeDriver` theater.
   - Consolidate slow daemon/world boots into 1 rich, cohesive E2E lifecycle journey (reaping
     worlds and `-pgid` process groups cleanly). Write unit tests strictly for subtle pure
     algorithms (DAG cycles/waves, Merkle hashing, `Forkfile` secret taint). Delete 0%-covered
     fallback dead code instead of testing it.
   - Protected critical tests (`crates/*/tests/critical_*.rs` and `**/tests/critical/**`) are
     guarded by `tools/critical_guard.py`; never weaken or delete them unless authorized in your
     node's `paths`.

3. **High-Craft CLI & Scannable Visual Docs (`review-ux`):**
   - Compute dynamic visible column widths (`.max()`, stripping ANSI when measuring) — never shear
     long IDs with hardcoded `{:<16}`/`{:<24}`. Style primary URLs bold/cyan, `running` green,
     `failed` red, and 12-char digests dimmed; emit **zero** `\x1b[` under `NO_COLOR` / non-TTY or
     `--json`.
   - Never swallow slow (`> 200ms`) child progress in blocking `.output()`, silent `sleep` loops,
     or `pull -q`: `spawn()` and stream live lines with `[step/total]` phase headers, spinners, and
     elapsed time. Pin `GIT_AUTHOR_*`/`GIT_COMMITTER_*` on internal git plumbing, format errors as
     `1 line + next:` remedy, lead `doctor` with `✓ Ready` / `✗ N issue(s)`, omit `:80` on port-80
     URLs, warn on stderr when ephemeral (the package enables the user service and the system
     `:80` socket for `SUDO_USER`; headless installs run `docs/install.sh`), and gate
     `[resources]` jargon behind `-v`.
   - Docs & specs: lead with user value + a working copy-pasteable snippet in the first 25 lines,
     use Mermaid diagrams + Zensical `!!!` admonitions + `<= 2`-sentence paragraphs, and ban
     internal `ADR-*` / `M3.*` lawyer prose.

4. **Anti-Vaporware, Upstream Grounding & Pre-`1.0` Scorched-Earth Deletion:**
   - Read `Cargo.toml`, `specs/*`, `TESTING_PLAN.md`, and `vendor/` BEFORE designing or coding.
   - If a required external daemon/webhook/wire spec is missing, **STOP immediately** (zero
     `[derived]`/`[guessed]` markers, zero `Unavailable*` CLI vaporware, zero degraded fallbacks)
     and return a `<= 12`-line `STATUS: blocked` (`BLOCK_KIND: missing-input`).
   - Published `README.md` and `docs/` describe **strictly what runs in the shipped binary**
     (`tools/check_docs_honesty.py`); unshipped UX lives in `specs/` only.
   - Pre-`1.0`: delete superseded commands/types/tests/docs atomically with zero legacy aliases,
     zero `#[deprecated]`, and zero `"Option A vs Option B"` pivot archaeology.

5. **Adaptive Scope, Triad Review Gate & Batched Execution Economy:**
   - **1-Call Discovery & Complete Writes:** Batch initial discovery into **one** command (never
     snoop `.opencode/**` internals, `candidate_meta.json`, `tools/*.py` source, `.gitignore`, or
     `git log`), write complete warning-free files in 1 `write` per file over micro-`edit` loops,
     and write **zero `RESULT.md` essays**.
   - **Coarse Vertical Slices & Triad Gate:** Plan 2–6 cohesive vertical slices per epic (1 writer
     per worktree; serialize shared `cli.rs`/`main.rs`/`Cargo.toml` edits via `--depends`; return
     `STATUS: needs_split` if a leaf explodes across `>= 4` subsystems). `lead` runs the parallel
     **Triad Review Gate**, filters cosmetic nits into at most 1 consolidated fix pass, runs
     targeted crate tests per slice, and runs the full workspace check once at the epic gate.

---

## Repository Safety & Workflow Rules

- **Plan & Path Locks:** Mutate `plan/` **exclusively** via `uv run --no-project tools/plan.py ...`
  (skill `plan-system`). Touch only globs in your node's `paths`.
- **Disk, Never `/tmp` `tmpfs`, for Heavy State:** `/tmp` on Linux runners is RAM-backed `tmpfs`.
  Keep worktrees under `.worktrees/`, CoW snapshots/volumes under `~/.cache/slipfork`,
  `.slipfork/`, or `/var/tmp`, and use `/tmp/opencode/` only for tiny short-lived files.
  Build caches are scratch: point one `CARGO_TARGET_DIR` outside the repo/`.worktrees` (e.g.
  `/var/tmp/<node>-target`), reuse it across verification runs, and `rm -rf` it plus any temporary
  worktree the moment the slice is committed. Never leave a `target/debug` or `target/release`
  inside a worktree or the repo root — `tools/coverage_report.py`'s fixed `target/cov` is the one
  exception, deleted after the run — and keep per-agent scratch under ~5 GB.
- **Python & Rust Execution:** Bare `python`, `python3`, and `pip` are denied. Use
  `uv run --no-project` for `tools/*.py`, `uvx --offline` for linters, and `cargo` for the
  workspace. Coverage runs through `uv run --no-project tools/coverage_report.py` into
  `target/cov`.
- **Confidentiality:** Never mention proprietary client or sibling private repository names in
  code, docs, tests, comments, or commit messages (`Forkfile` and Kerfufoo's public Snow/OSPP
  reference are public specs).
- **Git & Release PR Workflow:** Work on `release/<version>` branches; only `lead` and `conductor`
  commit using explicit paths (`git add -- <explicit files>`). Never push directly to `develop`,
  force-push, tag, or `gh pr merge`.

## Where Things Are

| Path | What |
|---|---|
| `specs/` | Clean-sheet specification suite: `01-forkfile.md`, `02-cli-ux.md`, `03-under-the-hood.md`, and the testing/packaging specs. |
| `TESTING_PLAN.md` | Root user-story E2E testing contract (fail-closed boundaries, lab journeys, coverage economy). |
| `.opencode/LESSONS.md` | Concise host, Rust, Linux namespace/process-group, Btrfs subvolume, AppArmor, and OpenCode facts. |
| `plan/` | Autonomous plan tree: `ROADMAP.md`, `STATE.md`, `HUMAN.md`, `ESCALATIONS.md`, `JOURNAL.md`, `BUDGET.json`, `AUTOPILOT`, `nodes/`. |
| `crates/` | Rust workspace: `slipfork` CLI/daemon, `Forkfile` compiler, world/Btrfs engine, native image engine, ingress, OSPP, and VCS crates. |
| `@slipfork/opencode` | OpenCode V2 plugin (`ctx.worktree.transform` + `ctx.shell.hook` + `ctx.session.hook`). |
| `scripts/` | Release scripts: `build-bundle`, `build-deb`, `build-rpm`, `build-arch`, `build-apk`, `build-lsp`, `build-standalone`, `check-bundle-layout.sh`, `check-install.sh`, `check-portable-elf.sh`, `release_stamp.py`. |
| `docs/install.sh` | Universal installer entry point (`curl … | sh`) for every distro. |
| `packaging/` | Distro recipes, systemd units and `stage-dist.py` (validates the `offworld-dist` bundle) consumed by `scripts/`; never a second installer implementation. |
| `tools/` | Stdlib-only harness: `plan.py`, `guard.py`, `critical_guard.py`, `check_docs_honesty.py`, `coverage_report.py`, `do_vm.py`. |

## Skills Index (load with `skill`)

| Skill | Load when |
|---|---|
| `dx-first` | Designing CLI/`Forkfile` UX, writing `specs/**`, `README.md`, or running `review-ux` |
| `engineering-laws` | Designing, coding, planning, or reviewing anything (Five Non-Negotiable Laws & Triad Gate) |
| `plan-system` | Reading or mutating `plan/` via `tools/plan.py` |
| `dispatch` | Spawning subagents, running the Triad Review Gate, triaging findings, or escalating |
| `rust-standards` | Writing or reviewing Rust crates (`crates/*`) |
| `testing` | Writing consolidated lab E2E / protected critical tests or running `review-tests` |
| `git-workflow` | Staging explicit files, running slice vs epic-gate checks, committing, or opening Release PRs |
