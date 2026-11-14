# Slipfork: Vision

> **Fork the world in milliseconds. Never rebuild what didn't change. Never clean up a test database again.**

## Why Slipfork Exists

Modern full-stack development and autonomous AI coding hit the same wall from two directions:

1. **Integration & browser test suites are painfully slow and brittle:**
   - Test runners (`pytest`, `Playwright`, `PHPUnit`) end up compiling frontend bundles inside fixtures, binding random `live_server` ports so bundles must be re-baked or pinned to a single worker, monkey-patching `STORAGES`/`CACHES` in test settings, and spending 80% of wall time in `TRUNCATE` / `serialized_rollback` / `DROP DATABASE` contention.
2. **Parallel AI coding agents trample each other's environments:**
   - Git worktrees isolate source files, **not** PostgreSQL databases, Redis keys, S3 buckets, background workers, or TCP ports (`:5432`, `:8000`, `:3000`).
   - Docker Compose per worktree creates `root:root` files on bind mounts, duplicates gigabytes of layers, drifts from production `snow.yml` manifests via `.env` spaghetti, and confuses LLMs with custom wrapper invocations.

Meanwhile, modern Linux filesystems (`ZFS`, `Btrfs`, `LVM-thin`, `XFS reflink`) and hypervisors (`Incus` system containers/QEMU VMs, CRIU, and kernel process/namespace primitives) can **clone disk state and warm process memory via Copy-on-Write in milliseconds**.

Slipfork bridges that gap with a **DX-First, manifest-driven (`snow.yml`) orchestrator**.

---

## Core Design Principles

1. **Developer Experience First (Dream Tool Docs Before Code):**
   - One manifest (`snow.yml` with `modes.all` + `modes.local` + `modes.cloud` and `!!` secret generation).
   - Shared host filesystem running as the developer's exact `$UID:$GID` — zero weird root files.
   - Zero-confusion OpenCode integration: creating a worktree forks the world implicitly; standard `shell` commands route automatically by `cwd`.
2. **Orthogonal Artifact Volumes + Stateful Fixture Snapshot DAG:**
   - Stateless build outputs (`front` bundle, static assets) are cached independently from stateful service snapshots (`migrate` -> `seed_*`), so CSS edits never invalidate seeded DB snapshots and DB migrations never rebuild the frontend.
   - Heavy tests run in parallel (`xdist`-style across **any** language), each inside a dedicated CoW world on canonical ports, discarded on exit with zero teardown cost.
3. **First-Class Synergy with Kerfufoo & Blastit:**
   - Shares `snow.yml` and the **Open Secret Provider Protocol (`OSPP v1`)** with Kerfufoo/Jonator.
   - Merges parallel wire-level observations across CoW test clones for **Blastit** (`--blastit-learn` / `--blastit-check`).
4. **The Three Iron Laws (Enforced Mechanically & by Dedicated Reviewers):**
   - **Zero Illegal Solutions (`review-legality`):** Missing credentials, sockets, or spec inputs halt immediately in `HUMAN.md` — never faked, guessed, or monkey-patched.
   - **Strict IoC/DI & Upward Challenge (`review-ioc`):** Pure domain traits wired at a single Composition Root; any spec blind spot that breaks DI clarity triggers a human `direction-challenge` before coding.
   - **Zero Pre-`1.0` Compat Cruft (`review-cruft`):** One canonical way, zero fallback branches, immediate deletion of superseded drafts.

---

## Milestones

| Milestone | Target | Scope |
|---|---|---|
| **M1 (`0.1.0`)** | Global DX Design & Proven CoW PoC | Dream-tool Zensical docs, full multi-language/multi-engine IoC/DI architecture (`ARCHITECTURE.md` + `ADR-0001..0004`), core Rust crates (`slipfork-snow`, `slipfork-dag`, `slipfork-engine`, `slipfork`), `@slipfork/opencode` plugin design, and measured real PostgreSQL 18 + XFS `FICLONE` + `netns` + Playwright benchmark |
| **M2 (`0.2.0`)** | `snow.yml` v2, OSPP v1 & `slipfork up/fork` | Full `IncusEngine` (LXC + QEMU VM stateful snapshots, `shift=true` UID passthrough) + `ReflinkNetnsEngine` daemon, local ingress router, `OSPP v1` client + Kerfufoo endpoint spec, and live `@slipfork/opencode` worktree/shell plugin |
| **M3 (`0.3.0`)** | Polyglot Parallel Test Orchestrator (`slipfork test`) | Orthogonal `ArtifactStore` + `FixtureDag` runtime, `TestAdapter` implementations for `pytest` (with warm Zygote fork!), `Playwright`/`Vitest`, `PHPUnit`/`Pest`, and `cargo test`/`nextest` |
| **M4 (`0.4.0`)** | Blastit Parallel Wire Learning & CI/Team Cache | `BlastitObsCollector` parallel `--learn`/`check` union, remote snapshot/artifact cache export for CI, and agent skills (`llms.txt`, OpenCode, Claude Code, Codex, Junie) |
