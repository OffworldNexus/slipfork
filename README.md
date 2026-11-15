# Slipfork (`0.1.0`)

> **Copy-on-write worlds, one `snow.yml` manifest, and orthogonal build/fixture planning — on Linux, with zero `sudo` and zero `root:root` files.**

```console
$ cargo install --path crates/slipfork
$ slipfork doctor                                    # probe CoW reflink, Incus socket, and unprivileged netns
$ slipfork snow resolve --file snow.yml --mode local  # validate modes, persist !! secrets (0600)
$ slipfork dag plan --file slipfork.toml --repo .     # hash artifacts independently from fixture waves
$ slipfork snap --source ./pgdata canonical           # capture a CoW snapshot in milliseconds
```

> **World lifecycle returns with `slipforkd` (`0.2.0`).** `slipfork fork`, `exec`, and `drop` exist and parse their flags (invalid flags exit `1`), then exit `2`: a world is owned by the `slipforkd` daemon, which lands in `0.2.0`. The commands above are what runs today — run them inside your own project after [Getting started](docs/getting-started.md) §2–§4 create `snow.yml`, `slipfork.toml`, and a CoW source directory; `doctor` is the only one that works in an empty checkout.

## What Ships Today

- **`snow.yml` resolver (`slipfork snow resolve` / `crates/slipfork-snow`):** `modes.all` + `modes.<mode>` inheritance, mode-scoped component add/override/`disabled: true`, `??` fallbacks, write-once `!!` CSPRNG secrets persisted atomically into a `0600` vault at `~/.local/share/slipfork/vaults/<project>.json`, and one exhaustive missing-value report.
- **Orthogonal artifact + fixture DAG planner (`slipfork dag plan` / `crates/slipfork-dag`):** SHA-256 Merkle digests for `[artifacts.<id>]` and topological `[fixtures.<id>]` waves, so frontend builds and database snapshots invalidate independently.
- **CoW snapshot engine (`slipfork snap` / `crates/slipfork-engine`):** kernel `ioctl(FICLONE)` clones on XFS/Btrfs/ZFS that fail closed on `tmpfs` or cross-device sources, with host `$UID:$GID` preserved.
- **OpenCode V2 plugin (`packages/opencode-slipfork`):** `ctx.worktree.transform`, `ctx.shell.hook("create.before")`, and `ctx.session.hook("context")` bind worktrees to worlds and route `shell` commands by `cwd` — wired and tested, inert until `slipforkd` lands in `0.2.0`.
- **Measured M1 benchmark:** PostgreSQL 18.7 + XFS `FICLONE` + isolated `netns` + headless Chromium Playwright: **`3.21x` faster** (`118 ms`/test, `0 ms` SQL teardown) than the sequential `TRUNCATE` baseline (`docs/research/m1-poc-benchmark.md`).

## Documentation

- **Getting started:** [`docs/getting-started.md`](docs/getting-started.md)
- **`snow.yml` modes & secrets:** [`docs/snow-modes.md`](docs/snow-modes.md)
- **CLI & manifest references:** [`docs/reference/cli.md`](docs/reference/cli.md), [`docs/reference/manifest.md`](docs/reference/manifest.md)
- **OpenCode plugin:** [`docs/agents-and-opencode.md`](docs/agents-and-opencode.md)
- **Architecture & decisions:** [`ARCHITECTURE.md`](ARCHITECTURE.md), [`docs/decisions/`](docs/decisions/), and [`docs/design/target-dx.md`](docs/design/target-dx.md) (internal future-DX spec — nothing there is shipped)

## Repository Layout

| Path | Contents |
|---|---|
| `crates/slipfork-world/` | `WorldTarget` / `LocalHostRoute` `.localhost` identity, fork and `domains[].id` validation, `WorldOrigin` |
| `crates/slipfork-snow/` | `SnowFile` v2 parser, `modes` resolution and `effective_components`, `Faceless` v2 (`??` & `!!`), `ResourceCoordinates`, `SecretProvider` + `FileVaultProvider` (`0600`) |
| `crates/slipfork-dag/` | `SlipforkConfig` (`slipfork.toml`), `FsMerkleHasher`, `FsDigestCache`, `DagPlanner` |
| `crates/slipfork-engine/` | `WorldEngine` trait, `ReflinkNetnsEngine` (`ioctl(FICLONE)` + unprivileged `netns`), `IncusEngine` REST `/1.0` adapter |
| `crates/slipfork/` | Single Composition Root CLI (`doctor`, `snow resolve`, `dag plan`, `snap`, and the daemon-owned `fork`/`exec`/`drop` surfaces) |
| `packages/opencode-slipfork/` | OpenCode V2 plugin |
| `poc/` | Committed M1 benchmark harness (PostgreSQL 18 + XFS `FICLONE` + Playwright) |

## Validation

```console
$ make check   # plan, hygiene, Python, TS, Rust fmt/clippy/doc/test, and zensical --strict
```

## Where This Is Going (Not Yet Shipped)

- **M2 (`0.2.0`):** unprivileged `slipforkd` daemon (port-80 `.localhost` edge, single-origin worlds with one `WorldOrigin`, 308/421 rules), mode-scoped component supervision, `OSPP v1` WebSocket secrets and `[resources]` strategies (`ADR-0008`), `git-remote-slipfork` (`refs/heads/<fork>` plus a `refs/slipfork/<fork>/wip` dirty view), Jujutsu workspaces, and the live `IncusEngine`.
- **M3 (`0.3.0`):** polyglot parallel test orchestrator (`RunnerProtocol v1`, pre-warmed CoW pools, typed coverage, surviving `SLIPFORK_OUTPUT_DIR`).
- **M4 (`0.4.0`):** remote content-addressed snapshot/artifact cache export and turnkey agent skills.
