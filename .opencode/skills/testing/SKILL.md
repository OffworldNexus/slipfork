---
name: Testing & Lab-First E2E Suites
description: Rules for Slipfork zero-mock lab E2E journeys, protected critical tests, algorithm-only unit tests, 0%-covered fallback pruning, and real CoW benchmarks.
---

# Testing Standards (Lab-First, Zero Mocks)

1. **Zero Mocks (Except OSPP Loopback Reference Server):**
   - Exercise real disk directories (`.slipfork/`, `~/.cache/slipfork`, `/var/tmp`, or disk `tempfile` — never heavy state on RAM `/tmp`), real `git` and shell subprocesses, real daemon/Unix-socket/Btrfs workflows, and (for OSPP) the minimal `127.0.0.1:0` loopback reference server (a real loopback reference server).
   - **Forbidden Mock Theater (`BLOCKER`):** Never introduce `Mock*`, `Fake*`, `Recording*`, `Stub*`, in-memory vault shims, or `TestTreeDriver` `chmod 0o555` directory-root theater (`0o555` on a directory root does not seal nested files and proves zero real Btrfs ioctl or `-pgid` process-group invariants).

2. **Consolidated Lab E2E Journeys & Protected Critical Tests:**
   - Protected critical suites (`crates/*/tests/critical_*.rs` and `**/tests/critical/**`) are mechanically guarded by `tools/critical_guard.py` and run directly with `cargo test --test 'critical_*'`.
   - **Consolidate Slow Boots:** Group related assertions (happy-path lifecycle + fail-closed refusal + secret redaction + clean world & `-pgid` process-group teardown) into **one cohesive lab E2E journey** per workflow rather than booting 15 separate daemons for 1-line checks.

3. **Algorithm-Only Unit Tests & Dead-Fallback Pruning:**
   - Write inline `#[cfg(test)]` unit tests strictly for pure, subtle algorithms: DAG cycle detection & wave ordering, Merkle content hashing, `Forkfile` mode inheritance, `??`/`!!` precedence, and secret-taint propagation.
   - **Delete 0%-Covered Fallbacks:** Never write unit tests just to cover dead fallback combinators (`LayeredVault`, `.ok().unwrap_or_default()`) — delete the dead fallback code instead.

4. **Real Measurements, Zero Simulated Benchmarks:**
   - Performance numbers in `specs/` must come from real lab runs against real OS processes (PostgreSQL 18, real CoW filesystem syscalls, real HTTP/browser requests) — never `sleep()` simulations.
