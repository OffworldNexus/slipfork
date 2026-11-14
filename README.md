# Slipfork

> **Instant copy-on-write dev worlds, OpenCode worktree forks, and parallel snapshot-DAG test orchestration from `snow.yml`.**

Slipfork turns your [`snow.yml`](docs/snow-modes.md) infrastructure manifest into:

1. **Shared-UID local development worlds (`slipfork up`):** Boots PostgreSQL, Redis, S3 (SeaweedFS), and application components with your host source tree mounted under your exact `$UID:$GID` — zero Docker `root:root` file pollution, zero `.env` drift, and automatic write-once `!! random_string(50)` secret generation.
2. **Millisecond AI-agent worktree forks (`slipfork fork` & `@slipfork/opencode`):** Creating an OpenCode worktree implicitly CoW-clones your live database, caches, and services into an isolated network namespace and routes `shell` commands transparently by directory.
3. **Parallel `xdist`-style heavy test orchestration (`slipfork test`):** Compiles frontend/static assets **once** into content-addressed **Artifact Volumes**, builds a **Fixture Snapshot DAG** (`migrate -> seed_*`), and runs every heavy browser/BDD/transactional test inside its own ephemeral CoW clone on canonical ports (`:8000`, `:3000`, `:5432`) — zero `serialized_rollback`, zero port collisions, and zero teardown cost.

---

## Quick Links

- **Dream-Tool User Docs:** [`docs/index.md`](docs/index.md) (`make docs-check` / `make docs-serve`)
- **Vision & Roadmap:** [`docs/VISION.md`](docs/VISION.md) & [`plan/ROADMAP.md`](plan/ROADMAP.md)
- **Global Target Architecture & DI Tree:** [`ARCHITECTURE.md`](ARCHITECTURE.md)
- **Architecture Decisions:** [`docs/decisions/`](docs/decisions/) (`ADR-0001` .. `ADR-0004`)
- **Milestone 1 Measured PoC Benchmark:** [`docs/research/m1-poc-benchmark.md`](docs/research/m1-poc-benchmark.md) (`make bench`)
- **Human Validation Inbox:** [`plan/HUMAN.md`](plan/HUMAN.md)

---

## Repository Layout

| Path | Description |
|---|---|
| `crates/slipfork-snow/` | `snow.yml` v2 parser (`modes.all` + `modes.<mode>`, mutual exclusion with `envs`), `Faceless` v2 template engine (`??` fallback & `!!` write-once secret generation), and `SecretProvider` DI port + adapters |
| `crates/slipfork-dag/` | Orthogonal Merkle SHA-256 input hasher, `ArtifactStore`, and `FixtureDag` topological execution/invalidation planner |
| `crates/slipfork-engine/` | `WorldEngine` DI port + `IncusEngine` (Incus Unix-socket REST API for LXC/QEMU CoW & `shift=true` UID mounts) + `ReflinkNetnsEngine` (Linux `FICLONE` + unprivileged `CLONE_NEWUSER\|NEWNET\|NEWNS\|NEWPID`) |
| `crates/slipfork/` | Single Composition Root CLI (`slipfork doctor`, `snow resolve`, `dag plan`, `fork`, `bench`) |
| `packages/opencode-slipfork/` | OpenCode V2 plugin (`ctx.worktree.transform` + `ctx.shell.hook("create.before")` + `ctx.session.hook("context")`) |
| `poc/` | Real PostgreSQL 18 + XFS `FICLONE` + Linux `netns` + warm Python Zygote + Playwright M1 benchmark |
| `.opencode/` & `tools/` | Autonomous DX-first + Quad-Review-Gate (`reviewer`, `review-legality`, `review-ioc`, `review-cruft`) OpenCode harness |

## Validation

```console
$ make check   # plan validate + guard + ruff + pytest + cargo fmt/clippy/doc/test + zensical --strict
$ make bench   # run live PostgreSQL 18 + XFS reflink + netns parallel benchmark
```
