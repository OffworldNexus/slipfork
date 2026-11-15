# Slipfork (`0.1.0`)

**Copy-on-write worlds, one `snow.yml` manifest, and orthogonal build/fixture planning — on Linux, with zero `sudo` and zero `root:root` files.**

```console
$ slipfork doctor                                    # probe CoW reflink, Incus socket, and unprivileged netns
$ slipfork snow resolve --file snow.yml --mode local  # validate modes, persist !! secrets (0600)
$ slipfork dag plan --file slipfork.toml --repo .     # hash artifacts independently from fixture waves
$ slipfork snap --source ./pgdata canonical           # capture a CoW snapshot in milliseconds
```

!!! warning "World lifecycle returns with `slipforkd` (`0.2.0`)"
    `slipfork fork`, `exec`, and `drop` exist and parse their flags (invalid flags exit `1`), then exit `2`: a world is owned by the `slipforkd` daemon, which lands in `0.2.0`. The commands above run inside your own project once **[Getting started](getting-started.md)** creates `snow.yml`, `slipfork.toml`, and a CoW source directory; `doctor` alone works in an empty checkout.

---

## What Runs Today

| Surface | Shipped behaviour |
|---|---|
| **`snow.yml` resolver** (`slipfork snow resolve`) | `modes.all` + `modes.<mode>` inheritance, mode-scoped component add/override/`disabled: true`, `??` fallbacks, write-once `!!` CSPRNG secrets in a `0600` vault, and one exhaustive missing-value report |
| **Orthogonal DAG planner** (`slipfork dag plan`) | SHA-256 Merkle digests per `[artifacts.<id>]` and topological `[fixtures.<id>]` waves; frontend edits never invalidate database snapshots and migration edits never trigger frontend rebuilds |
| **CoW snapshot engine** (`slipfork snap`) | Kernel `ioctl(FICLONE)` clones on XFS/Btrfs/ZFS that fail closed on `tmpfs` or cross-device sources |
| **OpenCode V2 plugin** (`@slipfork/opencode`) | Worktree → world binding, `cwd`-based shell routing, and session hints — wired and tested, inert until `slipforkd` arrives in `0.2.0` |

## Measured M1 Benchmark

Recorded in `docs/research/m1-poc-benchmark.md` (PostgreSQL 18.7, XFS `FICLONE`, isolated `netns`, headless Chromium Playwright):

| Operation | Measured |
|---|---:|
| 64 KiB XFS `ioctl(FICLONE)` probe | **397 us** |
| CoW clone of a 41.5 MiB seeded PostgreSQL 18 cluster | **44.4 ms** |
| 4 worlds binding `127.0.0.1:5432` concurrently, zero cross-world leaks | **413.6 ms** |
| Sequential `TRUNCATE` + re-seed baseline | `6.07 s` (`380 ms`/test) |
| Pre-warmed CoW worlds (`-j 8`, `0 ms` SQL teardown) | **1.89 s** (`118 ms`/test, **3.21x** faster) |

---

## Next Steps

- **[Getting started](getting-started.md)** — build `slipfork`, probe the host, resolve `snow.yml`, plan the DAG, snapshot state.
- **[`snow.yml` modes & secrets](snow-modes.md)** — mode resolution rules, `!!` generators, and the single-`WorldOrigin` coordinate model.
- **[Orthogonal DAG planner](testing-dag.md)** — configure `slipfork.toml` and read Merkle rebuild waves.
- **[OpenCode V2 plugin](agents-and-opencode.md)** — worktree worlds, `cwd` routing, and when to enable it.
- **[CLI](reference/cli.md)** & **[Manifest reference](reference/manifest.md)** — the commands, flags, and manifest schemas shipped today.

---

## Roadmap Corner (Not Yet in `0.1.0`)

- **M2 (`0.2.0`):** unprivileged `slipforkd` daemon (port-80 `.localhost` edge, single-origin worlds with one `WorldOrigin`), mode-scoped component supervision, `OSPP v1` WebSocket secrets and `[resources]` strategies (`ADR-0008`), `git-remote-slipfork` (`refs/heads/<fork>` plus a `refs/slipfork/<fork>/wip` dirty view), Jujutsu workspaces, and the live `IncusEngine`.
- **M3 (`0.3.0`):** polyglot parallel test orchestrator (`RunnerProtocol v1`, pre-warmed world pools, typed coverage, surviving `SLIPFORK_OUTPUT_DIR`).
- **M4 (`0.4.0`):** remote content-addressed snapshot/artifact cache export and turnkey agent skills.
