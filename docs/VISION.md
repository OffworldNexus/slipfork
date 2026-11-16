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

1. **Developer Experience First (Dream Tool Docs Before Code, Zero Black Boxes):**
   - One manifest (`snow.yml` with `modes.all` + `modes.local` + `modes.cloud` and `!!` secret generation — shipped for env/secret resolution and `components.<id>.run_command` override validation in `0.1.0`, with full mode-scoped `components.<id>` add/remove/override supervision in M2) plus optional `slipfork.toml` (`[artifacts]`, `[fixtures]` with `per_world`, typed `[test]` coverage shipped in `0.1.0`, and mode-aware `[resources]` `ResourceStrategy` in M2), governed by `ADR-0006` §1's typed-manifest law.
   - **Single-Origin Worlds (`WorldOrigin = Local { url } | Shared { url }`, `ADR-0005`, shipped in `0.2.0`):** each world has at most one public origin known before materialization (`slipforkd` running unprivileged via `systemd` `LISTEN_FDS` socket activation or `--listen` as sole local + shared ingress, `308`/`421` rules on `*.localhost` when shared, `H-008`), deterministic `<= 31`-byte Tunnel Weaver service-label truncation (`<prefix>-<hash>`), a `Dockerfile`-derived read-only `$UID:$GID` `L0` world rootfs + daemonless `crun` backing-service containers (`ADR-0009`), native `git` + Jujutsu (`jj workspace`) support, and built-in container-scoped `slipfork db`, `slipfork redis`, and `slipfork forward` bridges — starting offline on `FileVaultProvider` without requiring OSPP.
   - **First-Class Hook & Migration Failure Debugging (shipped in `0.2.0`):** hooks and migrations stream stdout/stderr live; on failure, Slipfork prints the exact command and full un-truncated output, **preserves the failed world** for inspection, and prints 1-line rescue commands (`slipfork hook run <id>` with interactive TTY `stdin`/`stdout`/`stderr` for `pdb`, `slipfork logs`, and `slipfork shell`).
   - Shared host filesystem running as the developer's exact `$UID:$GID` — zero weird root files (shipped in `0.1.0` `ReflinkNetnsEngine`).
   - Zero-confusion OpenCode integration: creating a worktree forks the world implicitly; standard `shell` commands route automatically by `cwd` (shipped in `0.1.0` `@slipfork/opencode`).
2. **Target M3 World-Owns-The-Stack Inversion + Orthogonal Artifact & Fixture Snapshot DAG:**
   - In M3 (`slipfork test`), `snow.yml` (`hooks: pre-deploy`, `components`, `ingresses`) boots the migrated, seeded, routed application stack **before** tests run — supplying boot-resolved `$BASE_URL` + `$DATABASE_URL` across polyglot stacks with **hermetic-by-default `[resources]`** (always ephemeral local S3 + clean `initdb -> migrate -> [fixtures.*]`, never mutating shared dev S3 or pulling drifting dev dumps unless opted in), while `pytest-slipfork` subtracts Django's redundant `test_<db>`, `live_server`, and `serialized_rollback`. Integration maturity is tracked via evidence-checked support tiers (`none`..`hardened` in `support/integrations.toml`, schema shipped in `0.1.0`, CLI & ceiling in M2; `ADR-0007`, `H-009` Option B).
   - Stateless build outputs (`front` bundle, static assets) hash independently from origin-independent fixture snapshots (`0.1.0` `slipfork dag plan`; M2/M3 adds `per_world = true` boot-time seeds plus pre-warmed CoW world pools and shared host `BrowserContext`s over `http://test-<slot>.<project>.localhost`).
3. **Target M2/M3 `OSPP v1` WebSocket Protocol, Built-In Coverage & Pure Orthogonality:**
   - Shares `snow.yml` (`0.1.0`) and pairs with Kerfufoo or compatible vaults over the **Open Secret Provider Protocol (`OSPP v1`, `ADR-0008`)**: a WebSocket protocol using `tpmd`-backed ECDSA P-256 (`p256:<hex>`) SSH-style challenge-response auth (`"ospp-v1" || nonce_s(32) || nonce_c(32) || server_name`) and browser-fragment pairing (`#handle=<hex>`) to deliver (a) `vars::`, `secrets::`, and `secrets.ensure` (`!!`), (b) remote resource coordinates (such as shared cloud dev S3 buckets), and (c) streaming PostgreSQL seed dumps (`zstd`/`pg_restore`) into local `base` CoW snapshots (`--no-dump` / `--refresh-dump`). OSPP remains optional for local worlds and never supplies world origins.
   - Connects to Tunnel Weaver (`weaved`) in M2 through the pure `TunnelRegistrar` DI trait (`0.1.0` `slipfork-world`) without binary linking; the concrete wire adapter stays isolated behind `H-006` until `weaved` publishes its IPC specification.
   - In M3, multi-world coverage is built in via typed `[test]` fields (`coverage`, `coverage_report`, `coverage_fail_under`) and `TestAdapter` `CoverageDriver` capabilities — while Blastit, Playwright traces, and Allure stay 100% decoupled via a surviving tool-owned `SLIPFORK_OUTPUT_DIR` directory (`ADR-0006` §5).
4. **The Three Iron Laws (Enforced Mechanically & by Dedicated Reviewers):**
   - **Zero Illegal Solutions (`review-legality`):** Missing credentials, sockets, or spec inputs halt immediately in `HUMAN.md` — never faked, guessed, or monkey-patched.
   - **Strict IoC/DI & Upward Challenge (`review-ioc`):** Pure domain traits wired at a single Composition Root; any spec blind spot that breaks DI clarity triggers a human `direction-challenge` before coding.
   - **Zero Pre-`1.0` Compat Cruft (`review-cruft`):** One canonical way, zero fallback branches, immediate deletion of superseded drafts (including replacing M1's REST OSPP sketch with `OSPP v1` WebSocket cleanly).

---

## Milestones

| Milestone | Target | Scope |
|---|---|---|
| **M1 (`0.1.0`)** | Global DX Design & Proven CoW PoC | Dream-tool Zensical docs, full multi-language/multi-engine IoC/DI architecture (`ARCHITECTURE.md` + `ADR-0001..0007`), core Rust crates (`slipfork-world`, `slipfork-snow`, `slipfork-dag`, `slipfork-engine`, `slipfork`), embedded `support/integrations.toml` (`SupportRegistry`), `@slipfork/opencode` plugin, and measured real PostgreSQL 18 + XFS `FICLONE` + `netns` + Playwright benchmark |
| **M2 (`0.2.0`)** | Single-Origin `.localhost` Edge (`slipforkd`), `OSPP v1` WebSocket + `ResourceStrategy`, VCS Workspaces & Service CLI | Unprivileged socket-activated `slipforkd` (`LISTEN_FDS` + `--listen`), single-origin `WorldOrigin` (`Local \| Shared`, `308`/`421` edge rules) & `<= 31`-byte Tunnel Weaver service-label truncation; a `Dockerfile`-derived read-only `$UID:$GID` `L0` world rootfs with daemonless `crun` backing-service OCI containers (`postgres:<v>`, `redis:<v>`, SeaweedFS) and container-scoped `db`/`redis`, per `ADR-0009`; mode-scoped `components.<id>` supervision + live-streamed inspectable hook debugging (`hook run`, `logs`, `shell`); built-in `db`, `redis`, and `forward` CLI; `OSPP v1` WebSocket + `tpmd` P-256 auth + streaming dev DB dumps + `[resources]` `ResourceStrategy` (`ADR-0008`); `VcsWorkspace` (`Git` + `Jj` + `git-remote-slipfork`); `slipfork support` + generated docs matrix & `make check` ceiling (`H-009` Option B); and isolated leaves for live `IncusEngine` (`H-004`) and `WeavedRegistrar` (`H-006`) |
| **M3 (`0.3.0`)** | Polyglot Parallel Test Orchestrator (`slipfork test`) | Universal NDJSON `RunnerProtocol v1`, hermetic-by-default `[resources]` test strategy, `per_world` vs origin-independent fixture execution, pre-warmed CoW world pool + shared host `BrowserContext`s, built-in typed `CoverageDriver` (`[test]` `coverage` / `coverage_report` / `coverage_fail_under`), surviving `SLIPFORK_OUTPUT_DIR` for Blastit/Allure/Playwright traces, and `ADR-0007` tier-verified `TestAdapter`s starting with `pytest-slipfork` (#1, warm Zygote) |
| **M4 (`0.4.0`)** | Remote CI Snapshot/Artifact Cache & Agent Ecosystem | Content-addressed remote artifact & CoW snapshot cache export/import for CI & teams, `llms.txt`, and turnkey agent skills for OpenCode, Claude Code, Codex, and Junie |
