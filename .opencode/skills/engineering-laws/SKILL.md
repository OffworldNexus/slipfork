---
name: Engineering Laws (True Constructor IoC/DI, Lab-First Zero Mocks, High-Craft UX, Scorched-Earth Pre-1.0)
description: MANDATORY for all Slipfork design, coding, planning, and review tasks. Defines the Five Non-Negotiable Engineering Laws enforced by the Triad Review Gate (review-arch, review-tests, review-ux).
---

# The Five Non-Negotiable Engineering Laws

Every slice in Slipfork passes the parallel **Triad Review Gate** (`review-arch`, `review-tests`, `review-ux`). Internalize these laws before designing or writing code.

---

## Law 1: True Constructor IoC/DI, Cohesive OOP & Canonical Module Ownership (`review-arch`)

1. **Single Composition Root:** Only the `slipfork` CLI/daemon entry point and the `@slipfork/opencode` plugin entry point read `std::env` / `process.env`, probe `/proc`, or bind ambient OS sockets/filesystems. Library crates contain **zero** `std::env::var`, `OnceLock`/`LazyLock`, or `static` mutable/atomic state.
2. **Cohesive OOP Structs (`<= 500` LOC) Over Free-Function Sprawl:** Group related state + behavior into cohesive structs (`SupervisedProcess`, `WorldManager`, `FileVault`) with methods — never 6-argument free-function galaxies or `*Inputs` parameter bags. Split files cleanly before exceeding `500` LOC.
3. **Architectural Port Traits ONLY — Zero Mock-Only Traits:** Introduce traits **strictly** at the canonical `specs/03` Crate & Port Map ports (`WorldEngine`, `BaseImageBuilder`, `SecretVault`, `DaemonControl`, `OsppClient`, `VcsWorkspace`). **NEVER** invent single-implementation mock-only traits (`MonotonicClock`, `HandoverClock`, `HostLoadProbe`, `FakeClock`) when a plain value struct (`Duration`, `StopBudget`) suffices.
4. **Extend Canonical Modules Instead of Leaking Internals:** Keep all syntax parsing, token/AST traversal, fallback evaluation, secret-taint rules, and protocol framing strictly inside their canonical owner module (the `Forkfile` compiler, native image engine, world/Btrfs engine, ingress, OSPP wire module; reuse declared `Cargo.toml` crates like `tempfile`, `httparse`, `serde_json`). When a caller needs filtered or partial evaluation, add a predicate/visitor helper inside the canonical module — **never** re-scan `${{` delimiters, HTTP headers, or tokens downstream.
5. **Mandatory Upward Direction Challenge:** If a cross-backend DI conflict makes a clean non-leaky tree impossible without `downcast_ref` or mode-flag hacks, **STOP** and escalate (`STATUS: blocked`, `BLOCK_KIND: direction-challenge`) with 2 concrete architectural options and a recommendation.

---

## Law 2: Lab-First E2E Testing & Coverage Economy — Zero Mocks (`review-tests`)

1. **Real Lab Infrastructure Only:** Test against real disk directories (`.slipfork/`, `~/.cache/slipfork`, `/var/tmp`, or disk `tempfile` — never heavy state on RAM `/tmp`), real `git`/shell subprocesses, real Btrfs/daemon paths, and (for OSPP) the minimal `127.0.0.1:0` loopback reference server.
2. **Zero Mock Theater (`BLOCKER`):** **NEVER** write `Mock*`, `Fake*`, `Recording*`, `Stub*`, in-memory vault shims, or `TestTreeDriver` `chmod 0o555` root-dir theater (`0o555` on a directory root fails to seal nested files and proves zero real Btrfs ioctl or `-pgid` process-group semantics).
3. **Consolidated Lifecycle Journeys & Algorithm-Only Unit Tests:** Consolidate multiple slow daemon/world boots into 1 rich, cohesive E2E lifecycle journey (asserting both happy path and fail-closed refusal + reaping worlds and `-pgid` process groups). Write unit tests strictly for subtle pure algorithms (DAG cycles/waves, Merkle hashing, `Forkfile` secret taint). Delete 0%-covered fallback dead code (`LayeredVault`, `.ok().unwrap_or_default()`) instead of writing tests for it.

---

## Law 3: High-Craft CLI & Scannable Visual Docs (`review-ux`)

1. **Dynamic Terminal Tables & ANSI Purity:** Compute dynamic visible column widths (`.max()`, stripping ANSI when measuring) — never shear long IDs with hardcoded `{:<16}`/`{:<24}`. Style primary URLs bold/cyan, `running` green, `failed` red, and 12-char digests dimmed; emit **zero** `\x1b[` when color is disabled (`NO_COLOR` / non-TTY) or in `--json`.
2. **Live Streamed Progress & Crisp Ergonomics:** Never swallow slow child operations (`> 200ms`) in blocking `.output()`, silent `sleep` poll loops, or `pull -q` — use `spawn()` and stream live lines with `[step/total]` phase headers, spinners, and elapsed time. Pin `GIT_AUTHOR_*`/`GIT_COMMITTER_*` on internal git plumbing, format errors as `1 line + next:` remedy, lead `doctor` with `✓ Ready` / `✗ N issue(s)`, omit `:80` on port-80 URLs, warn on stderr when ephemeral (`slipfork daemon install` user unit; `sudo slipfork daemon install --ingress` for the system `:80` socket + AppArmor), and gate `[resources]` plumbing behind `-v`.
3. **Visual, Problem-First Docs:** Lead user pages with value (*"Slipfork runs your cloud environment locally in a box; CI is the clutch"*) + a working snippet in the first 25 lines, include Mermaid diagrams + Zensical `!!!` admonitions + `<= 2`-sentence paragraphs, and ban `ADR-*` lawyer prose.

---

## Law 4: Anti-Vaporware, Upstream Grounding & Pre-`1.0` Scorched-Earth Deletion

1. **Read Upstream First & Block on Missing Inputs:** Inspect `Cargo.toml`, `package.json`, and `vendor/` before designing or coding. If an external daemon/webhook/wire spec is absent, **STOP immediately** (zero `[derived]`/`[guessed]` markers, zero `Unavailable*` CLI vaporware, zero silent degraded fallbacks) and return a `<= 12`-line `STATUS: blocked` (`BLOCK_KIND: missing-input`).
2. **Zero Vaporware in Published Docs:** Published `docs/` (`zensical.toml`) and `README.md` document strictly what runs in the shipped binary (`tools/check_docs_honesty.py`). Unshipped UX lives in `specs/` only.
3. **Pre-`1.0` Scorched-Earth Deletion:** While workspace `version < 1.0.0`, delete superseded commands, config keys, fallback branches, tests, and docs atomically — zero legacy aliases, zero `#[deprecated]`, and zero `"chose Option A over Option B"` pivot archaeology.

---

## Law 5: Adaptive Scope & Batched Execution Economy

1. **1-Call Discovery & Zero Meta-Snooping:** Batch initial file discovery into 1 call; never inspect `.opencode/**`, `candidate_meta.json`, `tools/*.py` source, `.gitignore`, or `git log`.
2. **Complete Authoring & Targeted Verification:** Write complete, clippy-clean files in 1 `write` per file over chained micro-`edit`s; run targeted crate checks per slice and the full workspace check (`cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`) once at the epic gate; write **zero `RESULT.md` essays**.
3. **Coarse Vertical Slices:** Decompose epics into 2–6 cohesive vertical slices with serialized `--depends` on shared files (`cli.rs`, `main.rs`, `Cargo.toml`), splitting dynamically (`STATUS: needs_split`) only when a leaf breaks across `>= 4` subsystems.
