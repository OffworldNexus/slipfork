# Slipfork (`0.2.0`)

**Copy-on-write worlds, one `snow.yml` manifest, and orthogonal build/fixture planning — on Linux, with zero `root:root` files and zero `sudo` on the default path.**

In a repository with a `snow.yml` from [getting started](getting-started.md):

```console
$ slipfork doctor                                    # probe CoW reflink, Incus socket, and unprivileged netns
$ slipfork snow resolve --file snow.yml --mode local  # validate modes, persist !! secrets (0600)
$ slipfork dag plan --file slipfork.toml --repo .     # hash artifacts independently from fixture waves
$ slipfork up                                        # build the world rootfs from your Dockerfile, boot it
$ slipfork fork feat-billing                         # second CoW world + .worktrees/feat-billing
$ slipfork list                                      # one origin URL per world
```

!!! info "One daemon, many thin clients"
    `slipfork` auto-spawns the unprivileged `slipforkd` daemon when `<runtime_dir>/daemon.sock` does not answer, so no systemd setup is required. `doctor` and `dag plan` are local diagnostics; `snow resolve` and every world command run through the daemon.

---

## What runs today

| Surface | Shipped behaviour |
|---|---|
| **World rootfs** (`slipfork up`) | Read-only `L0` rootfs built from your `snow.yml` `BuildImage` `Dockerfile`, content-addressed under `<state_dir>/cache/rootfs/<sha256>/app`, running as `$UID:$GID` |
| **Backing services** | `postgres:<v>`, `redis:<v>`, and SeaweedFS run as daemonless `crun` OCI containers sharing the world's `127.0.0.1`; `slipfork db`/`redis` open their clients |
| **World lifecycle** (`up`/`down`/`fork`/`drop`/`list`/`prune`) | CoW worlds cloned with `ioctl(FICLONE)`, one `WorldOrigin` URL per world, `--json` output, zero-setup daemon; `share` fails closed until tunnel sharing lands |
| **Inside a world** (`exec`/`shell`/`logs`/`hook run`/`forward`) | Daemon-resolved environment, native terminal stdio, retained component/hook logs, daemon-owned forward listeners |
| **`snow.yml` resolver** (`snow resolve`) | `modes.all` + `modes.<mode>`, mode-scoped components, `??` fallbacks, write-once `!!` CSPRNG secrets in a `0600` vault |
| **DAG planner** (`dag plan`) | SHA-256 Merkle digests per `[artifacts.<id>]` and topological `[fixtures.<id>]` waves |
| **OpenCode V2 plugin** (`@slipfork/opencode`) | Worktree → world binding, `cwd`-based shell routing through `slipfork exec`, session context hints |
| **`git-remote-slipfork`** | `git fetch` a running fork's branch and `refs/slipfork/<fork>/wip` over `slipfork://<fork>` |

Every claim above is graded in the [integration support matrix](support-matrix.md).

## Measured M1 benchmark

Recorded in `docs/research/m1-poc-benchmark.md` (PostgreSQL 18.7, XFS `FICLONE`, isolated `netns`, headless Chromium Playwright):

| Operation | Measured |
|---|---:|
| 64 KiB XFS `ioctl(FICLONE)` probe | **397 us** |
| CoW clone of a 41.5 MiB seeded PostgreSQL 18 cluster | **44.4 ms** |
| 4 worlds binding `127.0.0.1:5432` concurrently, zero cross-world leaks | **413.6 ms** |
| Sequential `TRUNCATE` + re-seed baseline | `6.07 s` (`380 ms`/test) |
| Pre-warmed CoW worlds (`-j 8`, `0 ms` SQL teardown) | **1.89 s** (`118 ms`/test, **3.21x** faster) |

## Next steps

- **[Getting started](getting-started.md)** — install, probe, boot, fork, and work inside a world.
- **[`snow.yml` modes & secrets](snow-modes.md)** — mode resolution, `!!` generators, coordinates.
- **[Orthogonal DAG planner](testing-dag.md)** — configure `slipfork.toml` and read rebuild waves.
- **[OpenCode V2 plugin](agents-and-opencode.md)** — automatic worktree worlds for agents.
- **[CLI](reference/cli.md)** & **[manifest reference](reference/manifest.md)** — commands, flags, schemas.
- **[Support matrix](support-matrix.md)** — tiers, evidence, and what each integration guarantees.

## Roadmap corner (not yet shipped)

- **M2 (`0.2.0`, remainder):** tunnel sharing (`weaved` registrar, `H-006`), live `IncusEngine` (`H-004`), `OSPP v1` remote secret bundles, Jujutsu workspaces, and rootfs/OCI cache garbage collection in `prune`.
- **M3 (`0.3.0`):** polyglot parallel test orchestrator (`RunnerProtocol v1`, pre-warmed world pools, typed coverage, `SLIPFORK_OUTPUT_DIR`).
- **M4 (`0.4.0`):** remote content-addressed snapshot/artifact cache export and turnkey agent skills.
