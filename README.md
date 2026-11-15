# Slipfork (`0.2.0`)

> **Copy-on-write worlds, one `snow.yml` manifest, and orthogonal build/fixture planning — on Linux, with zero `sudo` and zero `root:root` files.**

```console
$ cargo install --path crates/slipfork
$ slipfork doctor                                    # probe CoW reflink, Incus socket, and unprivileged netns
$ slipfork snow resolve --file snow.yml --mode local  # validate modes, persist !! secrets (0600)
$ slipfork dag plan --file slipfork.toml --repo .     # hash artifacts independently from fixture waves
$ slipfork up                                        # boot the canonical world through zero-setup slipforkd
$ slipfork list                                      # one origin URL per world
```

> **World lifecycle is daemon-served.** `slipfork up`, `down`, `fork`, `drop`, `list`, `prune`, `share`, `secret set|unset|list`, `exec`, and `daemon unit` drive the unprivileged `slipforkd` daemon over a `0600` `<runtime_dir>/daemon.sock`. When that socket does not answer, `slipfork` auto-spawns `slipforkd --listen 127.0.0.1:0` in the background, so no systemd setup is required. `doctor`, `snow resolve`, and `dag plan` remain local diagnostics. Interactive `logs`, `shell`, `db`, `redis`, `forward`, `hook run`, `auth login`, `support`, and `git-remote-slipfork` land in the remainder of `0.2.0`.

## What Ships Today

- **`snow.yml` resolver (`slipfork snow resolve` / `crates/slipfork-snow`):** `modes.all` + `modes.<mode>` inheritance, mode-scoped component add/override/`disabled: true`, `??` fallbacks, write-once `!!` CSPRNG secrets persisted atomically into a `0600` vault at `~/.local/share/slipfork/vaults/<project>.json`, and one exhaustive missing-value report.
- **Orthogonal artifact + fixture DAG planner (`slipfork dag plan` / `crates/slipfork-dag`):** SHA-256 Merkle digests for `[artifacts.<id>]` and topological `[fixtures.<id>]` waves, so frontend builds and database snapshots invalidate independently.
- **Daemon-served world lifecycle (`slipfork up` / `down` / `fork` / `drop` / `list` / `prune` / `share` / `secret` / `exec`, `slipforkd`):** unprivileged CoW worlds cloned with `ioctl(FICLONE)` on XFS/Btrfs/ZFS, one `WorldOrigin` URL per world, an in-netns `127.0.0.1` loopback bridge to the daemon's HTTP/WS ingress, and zero `sudo` / zero `root:root` files over `0600` Unix sockets with zero-setup daemon auto-spawn.
- **OpenCode V2 plugin (`packages/opencode-slipfork`):** `ctx.worktree.transform`, `ctx.shell.hook("create.before")`, and `ctx.session.hook("context")` bind worktrees to worlds and route `shell` commands by `cwd` — the gateway is updated to the daemon-served CLI in `M2.18`.
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
| `crates/slipfork/` | `slipforkd` daemon Composition Root plus the thin `slipfork` client (`doctor`, `snow resolve`, `dag plan`, and daemon-served `up`/`down`/`fork`/`drop`/`list`/`prune`/`share`/`secret`/`exec`/`daemon unit`) |
| `packages/opencode-slipfork/` | OpenCode V2 plugin |
| `poc/` | Committed M1 benchmark harness (PostgreSQL 18 + XFS `FICLONE` + Playwright) |

## Validation

```console
$ make check   # plan, hygiene, Python, TS, Rust fmt/clippy/doc/test, and zensical --strict
```

## Where This Is Going (Not Yet Shipped)

- **M2 (`0.2.0`):** remaining CLI part 2 (`logs`, `shell`, `db`, `redis`, `forward`, `hook run`, `auth login`, `support`, `git-remote-slipfork`), `OSPP v1` WebSocket secrets and `[resources]` strategies (`ADR-0008`), Jujutsu workspaces, and the live `IncusEngine` — on top of the shipped `slipforkd` daemon, single-origin worlds, and zero-setup world lifecycle.
- **M3 (`0.3.0`):** polyglot parallel test orchestrator (`RunnerProtocol v1`, pre-warmed CoW pools, typed coverage, surviving `SLIPFORK_OUTPUT_DIR`).
- **M4 (`0.4.0`):** remote content-addressed snapshot/artifact cache export and turnkey agent skills.
