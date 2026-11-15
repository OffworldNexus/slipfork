# Slipfork (`0.2.0`)

> **Copy-on-write worlds, one `snow.yml` manifest, and orthogonal build/fixture planning — on Linux, with zero `root:root` files and zero `sudo` on the default path.**

```console
$ cargo install --path crates/slipfork
$ slipfork doctor                                    # probe CoW reflink, Incus socket, and unprivileged netns
$ slipfork snow resolve --file snow.yml --mode local  # validate modes, persist !! secrets (0600)
$ slipfork dag plan --file slipfork.toml --repo .     # hash artifacts independently from fixture waves
$ slipfork up                                        # build the world rootfs from your Dockerfile, boot it
$ slipfork fork feat-billing                         # second CoW world + .worktrees/feat-billing
$ slipfork list                                      # one origin URL per world
```

In a repository with a `snow.yml` that declares a build image and `modes.local` ([getting started](docs/getting-started.md)).

Requires **Linux**, **Rust 1.94+**, Docker with `buildx`, `unshare`/`ip`, and a CoW filesystem (`XFS` `reflink=1`, `Btrfs`, or `ZFS`) under `~/.cache/slipfork`. Every world boots from a read-only rootfs built from a `snow.yml` `images:` `Dockerfile`.

`slipfork` auto-spawns the unprivileged `slipforkd` daemon over a `0600` `<runtime_dir>/daemon.sock` when nothing answers, so no systemd setup is required. `doctor` and `dag plan` stay local; `snow resolve` and every world command run through the daemon.

## What ships in `0.2.0`

- **`Dockerfile`-derived `L0` world rootfs** — content-addressed under `<state_dir>/cache/rootfs/<sha256>/app`, mounted read-only, executed as `$UID:$GID`, with `[world]` selecting the image and stage.
- **Daemonless `crun` backing services** — `postgres:<v>`, `redis:<v>`, and SeaweedFS in OCI containers sharing the world's `127.0.0.1`; `slipfork db`/`redis` run their clients inside the containers.
- **Daemon-served world lifecycle** — `up`, `down`, `fork`, `drop`, `list`, `prune`, `secret`, `daemon unit` (`share` fails closed until tunnel sharing lands), plus `exec`, `shell`, `logs`, `hook run`, `forward`, `auth login`, `support`, and `git-remote-slipfork`.
- **`snow.yml` resolver and DAG planner** — mode-scoped components, write-once `!!` secrets in a `0600` vault, Merkle artifact/fixture waves.
- **OpenCode V2 plugin** — worktrees become CoW worlds; shell commands route through `slipfork exec`.
- **Measured M1 benchmark** — `3.21x` faster than a sequential `TRUNCATE` baseline (`docs/research/m1-poc-benchmark.md`).

All supported integrations are graded against their evidence in the [support matrix](docs/support-matrix.md).

## Documentation

- [Getting started](docs/getting-started.md)
- [`snow.yml` modes & secrets](docs/snow-modes.md)
- [Orthogonal DAG planner](docs/testing-dag.md)
- [OpenCode V2 plugin](docs/agents-and-opencode.md)
- [CLI reference](docs/reference/cli.md) · [Manifest reference](docs/reference/manifest.md) · [Support matrix](docs/support-matrix.md)
- Internal design: [`ARCHITECTURE.md`](ARCHITECTURE.md), [`docs/decisions/`](docs/decisions/), [`docs/design/target-dx.md`](docs/design/target-dx.md) (nothing there is a shipped promise)

## Validation

```console
$ make check   # plan, hygiene, Python, TS, Rust fmt/clippy/doc/test, support registry, and Zensical --strict
```

## Where this is going (not yet shipped)

- **M2 (`0.2.0` remainder):** tunnel sharing (`weaved`), live `IncusEngine`, remote `OSPP v1` secret bundles, Jujutsu workspaces, rootfs/OCI cache GC in `prune`.
- **M3 (`0.3.0`):** polyglot parallel test orchestrator (`RunnerProtocol v1`, pre-warmed CoW pools, typed coverage, `SLIPFORK_OUTPUT_DIR`).
- **M4 (`0.4.0`):** remote content-addressed snapshot/artifact cache export and turnkey agent skills.
