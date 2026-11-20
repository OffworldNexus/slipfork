---
name: Rust Standards (`crates/*`)
description: Idiomatic Rust 2024, True Constructor IoC/DI without single-impl mock traits, cohesive OOP structs <=500 LOC, canonical module ownership, process-group safety, and rich CLI UX rules for Slipfork crates.
---

# Rust Standards (`crates/*`)

1. **Edition, Sizing & Lint Discipline:**
   - Workspace uses Rust `edition = "2024"` (`rustfmt.toml` pins `edition = "2024"` and `style_edition = "2024"`).
   - Keep every `.rs` source file `<= 500` LOC by splitting cohesive submodules rather than accreting god-files.
   - Verify with `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets -- -D warnings`.

2. **True Constructor IoC / DI & Cohesive OOP:**
   - **Single Composition Root:** Only `crates/slipfork/src/main.rs` reads `std::env`, probes `/proc`, or binds ambient OS handles. Library crates contain **zero** `std::env::var`, `OnceLock`/`LazyLock`, or `static` mutable/atomic state.
   - **Cohesive Structs With Methods:** Encapsulate state + behavior in domain/adapter structs (`SupervisedProcess`, `WorldManager`, `FileVault`) — never 6-arg free-function galaxies or `*Inputs` parameter bags.
   - **Architectural Port Traits ONLY:** Define traits strictly at the canonical `specs/03` Crate & Port Map ports (`WorldEngine`, `BaseImageBuilder`, `SecretVault`, `DaemonControl`, `OsppClient`, `VcsWorkspace`). **NEVER** create single-implementation mock-only traits (`MonotonicClock`, `HandoverClock`, `HostLoadProbe`, `FakeClock`) — pass plain value structs (`Duration`, `StopBudget`) instead.
   - **Canonical Module Extension:** Keep syntax parsing, token/AST traversal, fallback evaluation, secret taint, and wire framing inside their canonical module (the `Forkfile` compiler, native image engine, world/Btrfs engine, ingress, OSPP wire module). Expose predicate/visitor helpers in the canonical module rather than re-scanning `${{` or HTTP headers downstream.

3. **Safety, Process-Group Hygiene & Error Redaction:**
   - `#![forbid(unsafe_code)]` on every crate except the world engine's thin Linux syscall/namespace boundary, where every `unsafe` block carries an explicit `// SAFETY:` invariant proof.
   - Reap and signal whole process groups (`-pgid`) so child trees never leak or stall test runners on open stdio FDs.
   - Zero `unwrap()` / `expect()` / `panic!()` in non-test library code; return typed `thiserror` errors formatted at the CLI boundary as `1 line + next:` actionable remedy, and redact secrets via `SecretValue` (`Debug` prints `"[REDACTED]"`).

4. **High-Craft CLI & Declared Dependencies:**
   - Reuse workspace dependencies (`tempfile`, `httparse`, `serde_json`, etc.) — never hand-roll duplicate parsers or temp cleaners.
   - Enforce dynamic ANSI-stripped column widths, semantic ANSI colors (`NO_COLOR` & `--json` pure of `\x1b[`), live `spawn()` `[step/total]` progress/spinners on `> 200ms` ops, pinned `GIT_AUTHOR_*`/`GIT_COMMITTER_*` on internal git calls, verdict-first `doctor`, and port-less `:80` URLs.
