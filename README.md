# Slipfork (`0.1.0`)

> **Copy-on-write world primitives, `snow.yml` v2 mode/secret resolution, orthogonal DAG planning, and OpenCode V2 worktree isolation.**

## What Ships Today in `0.1.0`

Every command and package below is implemented, tested, and runnable today:

- **`snow.yml` v2 Resolver (`slipfork snow resolve` / `crates/slipfork-snow`):** Resolves `modes.all` + `modes.<mode>` (and validates per-mode `components.<id>.run_command` overrides for M2 process supervision), `??` read-only fallbacks, and `!!` write-once CSPRNG secrets persisted atomically into `~/.local/share/slipfork/vaults/<slug>.json` (`0600`), with exhaustive missing-key refusal and `.localhost` hostname/fork validation via `slipfork_world::LocalHostRoute`.
- **Orthogonal Artifact + Fixture DAG Planner (`slipfork dag plan` / `crates/slipfork-dag`):** Hashes `[artifacts.<id>]` and `[fixtures.<id>]` in `slipfork.toml` into SHA-256 Merkle waves (`~10.9 ms`), keeping frontend build invalidation 100% orthogonal to database snapshot lineages.
- **Rootless CoW World Engine (`slipfork doctor`, `snap`, `fork`, `exec`, `drop` / `crates/slipfork-engine`):** CLI commands run `ReflinkNetnsEngine`, cloning stateful service directories via Linux `ioctl(FICLONE)` (XFS/Btrfs reflink, failing closed on `tmpfs`) and executing commands inside unprivileged `CLONE_NEWUSER | CLONE_NEWNET | CLONE_NEWNS | CLONE_NEWPID` namespaces with host `$UID:$GID` preserved (`0` `root:root` files); `crates/slipfork-engine` also ships an `IncusEngine` Unix-socket REST `/1.0` library adapter awaiting M2 daemon/CLI wiring.
- **OpenCode V2 Worktree Plugin (`packages/opencode-slipfork`):** Hooks `ctx.worktree.transform`, `ctx.shell.hook("create.before")`, and `ctx.session.hook("context")` so creating a git worktree forks a CoW world and routes standard `shell` commands implicitly by `cwd`.
- **Measured Live Benchmark (`make bench`):** Real PostgreSQL 18.7 + XFS `FICLONE` + isolated `netns` + warm Python runner + headless Chromium Playwright achieving **`3.21x` speedup (`118 ms`/test)** with `0 ms` SQL teardown (`docs/research/m1-poc-benchmark.md`).

```console
$ cargo run -q -p slipfork -- doctor
$ cargo run -q -p slipfork -- snow resolve --file snow.yml --mode local
$ cargo run -q -p slipfork -- dag plan --file slipfork.toml --repo .
$ cargo run -q -p slipfork -- snap --source ./pgdata canonical
$ cargo run -q -p slipfork -- fork exp --from canonical --workspace .
$ cargo run -q -p slipfork -- exec --world exp --snow-file snow.yml -- env
$ cargo run -q -p slipfork -- drop exp
```

---

## Documentation & Architecture

- **Published `0.1.0` User Docs:** [`docs/index.md`](docs/index.md) & [`docs/reference/cli.md`](docs/reference/cli.md) (`make docs-check`)
- **Measured PoC Benchmark:** [`docs/research/m1-poc-benchmark.md`](docs/research/m1-poc-benchmark.md) (`make bench`)
- **Architecture, ADRs & Target M2–M4 DX Specs:** [`ARCHITECTURE.md`](ARCHITECTURE.md), [`docs/decisions/`](docs/decisions/) (`ADR-0001`..`ADR-0007`), [`docs/design/target-dx.md`](docs/design/target-dx.md), and [`docs/VISION.md`](docs/VISION.md)

---

## Repository Layout

| Path | Shipped in `0.1.0` |
|---|---|
| `crates/slipfork-world/` | Pure `WorldTarget` / `LocalHostRoute` `.localhost` identity, fork/project validation, and opaque `TunnelRegistrar`/`TunnelLease` DI traits |
| `crates/slipfork-snow/` | `SnowFile` v2 parser (`modes`, `components.<id>.run_command` validation for M2), `Faceless` v2 (`??` & `!!`), `ResourceCoordinates` on `slipfork-world` types, `SecretProvider` + `FileVaultProvider` (`0600`) |
| `crates/slipfork-dag/` | `SlipforkConfig` (`slipfork.toml`), SHA-256 `FsMerkleHasher`, `FsDigestCache`, and `DagPlanner` |
| `crates/slipfork-engine/` | `WorldEngine` trait + CLI-wired `ReflinkNetnsEngine` (`ioctl(FICLONE)` + unprivileged `netns`) + `IncusEngine` REST `/1.0` library adapter (M2 CLI/live wiring) |
| `crates/slipfork/` | Single Composition Root CLI (`doctor`, `snow resolve`, `dag plan`, `snap`, `fork`, `exec`, `drop`) |
| `packages/opencode-slipfork/` | OpenCode V2 plugin (`worktree.transform` + `shell.hook` + `session.hook`) |
| `poc/` | Live PostgreSQL 18 + XFS `FICLONE` + `netns` + Playwright benchmark (`make bench`) |

## Validation

```console
$ make check   # plan validate + guard + ruff + pytest + bun test + cargo fmt/clippy/doc/test + zensical --strict
$ make bench   # run live PostgreSQL 18 + XFS reflink + netns parallel benchmark
```

---

## Where This Is Going (Upcoming Roadmap — Not Yet in `0.1.0`)

- **M2 (`0.2.0`):** Single-origin worlds (`WorldOrigin = Local | Shared` with `308` `.localhost` redirect when shared, starting on `FileVaultProvider` without requiring OSPP), port-80 `slipforkd` edge daemon, Tunnel Weaver (`weaved`) leases (`H-006`), `git-remote-slipfork` (`<fork>` + `<fork>/wip`), Jujutsu (`jj workspace`) adapter, and evidence-backed integration support registry (`ADR-0007`).
- **M3 (`0.3.0`):** Polyglot parallel test coordinator (`RunnerProtocol v1`, world-owned stacks + `pytest-slipfork` tracked via `ADR-0007` support tiers, pre-warmed CoW pools, built-in typed multi-world coverage, and surviving `SLIPFORK_OUTPUT_DIR` for Blastit, Allure, and Playwright traces).
- **M4 (`0.4.0`):** Remote content-addressed CI/team snapshot & artifact cache export/import and turnkey agent skills.
