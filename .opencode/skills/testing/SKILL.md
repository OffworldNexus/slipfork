---
name: Testing & Conformance Suites
description: Rules for Slipfork protected critical tests, DI trait conformance matrices, tool tests, and real un-mocked CoW benchmarks.
---

# Testing Standards

1. **Three Test Tiers:**
   - **Unit tests** (inline `#[cfg(test)]` & `tools/tests/`): fast edge-case verification of parsers, Merkle hashing, DAG topological ordering, and error diagnostics.
   - **Shared Trait Conformance Suites**: Every DI port (`SecretProvider`, `WorldEngine`, `ArtifactStore`, `TestAdapter`) defines one shared conformance suite executed identically against **every** implementation (`MemorySecretProvider` & `FileVaultProvider`; `IncusEngine` protocol harness & `ReflinkNetnsEngine`).
   - **Protected Critical Tests** (`crates/*/tests/critical_*.rs` and `**/tests/critical/**`): Guarded mechanically by `tools/critical_guard.py`. Pin core invariants:
     - `snow.yml` `modes` vs `envs` mutual exclusion, `modes.all` inheritance, `!!` write-once secret persistence, missing-secret hard fail, and secret redaction.
     - Orthogonal DAG invalidation (front change never invalidates DB snapshot; migration change never invalidates front artifact).
     - Real CoW isolation (`FICLONE` + `netns`: mutations in clone A never leak to base snapshot or clone B; `$UID` ownership preserved).
2. **Both Positive & Adversarial Assertions:**
   - Every critical test asserts both the happy path AND the fail-closed refusal path (exact error variant, zero partial side effects).
3. **Real Measurements, Zero Fake Benchmarks:**
   - Performance claims in `docs/research/` must come from `poc/run_benchmark.py` running real OS processes (PostgreSQL 18, real CoW filesystem syscalls, real HTTP/browser requests) — never `sleep()` simulations.
