# Slipfork: Vision

> **Fork the world in milliseconds. Never rebuild what didn't change. Never clean up a test database again.**

## Why Slipfork Exists

Modern full-stack development and autonomous AI coding hit the same wall from two directions:

1. **Integration & browser test suites are painfully slow and brittle across every language:**
   - Non-Django stacks (Flask+Alembic, FastAPI, Hono/SvelteKit, Symfony/Slim, Go, Rust) have to cobble together slow Docker `testcontainers`, random port-0 `live_server` scripts, and manual per-worker migrations — while Django (`pytest-django`) spends 80% of wall time creating `test_<db>` and running `TRUNCATE` / `serialized_rollback`.
   - Test runners end up compiling frontend bundles inside fixtures, monkey-patching `STORAGES`/`CACHES` in test settings, and colliding on background workers, Redis, and S3.
2. **Parallel AI coding agents trample each other's environments:**
   - Git worktrees (and Jujutsu workspaces) isolate source files, **not** PostgreSQL databases, Redis keys, S3 buckets, background workers, or TCP ports (`:5432`, `:8000`, `:3000`).
   - Docker Compose per worktree creates `root:root` files on bind mounts, duplicates gigabytes of layers, drifts from production `snow.yml` manifests via `.env` spaghetti, and confuses LLMs with custom wrapper invocations.

Meanwhile, modern Linux filesystems (`ZFS`, `Btrfs`, `LVM-thin`, `XFS reflink`) and hypervisors (`Incus` system containers/QEMU VMs, CRIU, and kernel process/namespace primitives) can **clone disk state and warm process memory via Copy-on-Write in milliseconds**.

Slipfork bridges that gap with a **DX-First, manifest-driven (`snow.yml`) orchestrator**.

---

## Core Design Principles

1. **Developer Experience First (Dream Tool Docs Before Code):**
   - One manifest (`snow.yml` with `modes.all` + `modes.local` + `modes.cloud`, optional per-mode `components.<id>.run_command`, and `!!` secret generation).
   - Port-80 `http://(<snow-domain>.)?(<fork>.)?<project>.localhost` routing (`slipforkd`), optional public sharing via Tunnel Weaver (`weaved`) through a pure `TunnelRegistrar` trait (wire adapter gated on `weaved`'s published IPC spec), `slipfork shell [<fork>]`, and native `git pull slipfork <fork>` (`+ <fork>/wip`) & Jujutsu (`jj workspace`) support.
   - Shared host filesystem running as the developer's exact `$UID:$GID` — zero weird root files.
   - Zero-confusion OpenCode integration: creating a worktree forks the world implicitly; standard `shell` commands route automatically by `cwd`.
2. **World-Owns-The-Stack Inversion + Orthogonal Artifact & Fixture Snapshot DAG:**
   - `snow.yml` (`hooks: pre-deploy`, `components`, `ingresses`) boots the migrated, seeded, routed application stack **before** tests run — giving Flask, FastAPI, Node/TS, PHP, Go, and Rust instant full-stack tests (`$BASE_URL` + `$DATABASE_URL`) while letting `pytest-slipfork` disable Django's redundant `test_<db>`, `live_server`, and `serialized_rollback`.
   - Stateless build outputs (`front` bundle, static assets) are cached independently from stateful service snapshots (`migrate` -> `seed_*`), and pre-warmed CoW world pools + shared host `BrowserContext`s over `http://test-<slot>.<project>.localhost` keep heavy browser+SQL tests fast.
3. **Pure Orthogonality with Kerfufoo, Tunnel Weaver, and Blastit:**
   - Shares `snow.yml` and the **Open Secret Provider Protocol (`OSPP v1`)** with Kerfufoo.
   - Connects to Tunnel Weaver (`weaved`) through a pure `TunnelRegistrar` DI trait without binary linking; the concrete wire adapter is gated on `weaved`'s published IPC specification.
   - Stays 100% decoupled from Blastit, coverage, and Allure via one generic per-world `$SLIPFORK_ARTIFACTS_DIR` + `[test].after_suite` hook — Blastit runs inside the world as an ordinary `snow.yml` component `run_command`.
4. **The Three Iron Laws (Enforced Mechanically & by Dedicated Reviewers):**
   - **Zero Illegal Solutions (`review-legality`):** Missing credentials, sockets, or spec inputs halt immediately in `HUMAN.md` — never faked, guessed, or monkey-patched.
   - **Strict IoC/DI & Upward Challenge (`review-ioc`):** Pure domain traits wired at a single Composition Root; any spec blind spot that breaks DI clarity triggers a human `direction-challenge` before coding.
   - **Zero Pre-`1.0` Compat Cruft (`review-cruft`):** One canonical way, zero fallback branches, immediate deletion of superseded drafts.

---

## Milestones

| Milestone | Target | Scope |
|---|---|---|
| **M1 (`0.1.0`)** | Global DX Design & Proven CoW PoC | Dream-tool Zensical docs, full multi-language/multi-engine IoC/DI architecture (`ARCHITECTURE.md` + `ADR-0001..0006`), core Rust crates (`slipfork-snow`, `slipfork-dag`, `slipfork-engine`, `slipfork`), `@slipfork/opencode` plugin, and measured real PostgreSQL 18 + XFS `FICLONE` + `netns` + Playwright benchmark |
| **M2 (`0.2.0`)** | `snow.yml` v2, `.localhost` Edge (`slipforkd`), `weaved` IPC & VCS Workspaces | Full `slipforkd` port-80 `*.localhost` router, `WeavedRegistrar` IPC lease client (gated on `weaved`'s published IPC spec), `VcsWorkspace` (`GitWorktreeAdapter` + `JjWorkspaceAdapter` + `git-remote-slipfork` + `slipfork shell [<fork>]`), live `IncusEngine` + `ReflinkNetnsEngine`, `OSPP v1` client, and `@slipfork/opencode` e2e |
| **M3 (`0.3.0`)** | Polyglot Parallel Test Orchestrator (`slipfork test`) | Universal NDJSON `RunnerProtocol v1`, pre-warmed CoW world pool + shared host `BrowserContext`s, generic `$SLIPFORK_ARTIFACTS_DIR` + `after_suite`, and shared-conformance `TestAdapter`s starting with `pytest-slipfork` (#1, warm Zygote) followed by `Playwright`/`Vitest`, `PHPUnit`/`Pest`, `cargo nextest`, and generic CLI |
| **M4 (`0.4.0`)** | Remote CI Snapshot/Artifact Cache & Agent Ecosystem | Content-addressed remote artifact & CoW snapshot cache export/import for CI & teams, `llms.txt`, and turnkey agent skills for OpenCode, Claude Code, Codex, and Junie |
