# Slipfork (`0.1.0`)

**Copy-on-write worlds, one `snow.yml` manifest, and orthogonal build/fixture planning — on Linux, with zero `root:root` files and no `sudo` for day-to-day worlds. Root is needed only for the one-time Btrfs mount and the Ubuntu 24.04 AppArmor `userns` profile.**

In a repository with a `snow.yml` from [getting started](getting-started.md) (a `slipfork.toml` is optional), after [installing and mounting the Btrfs state directory](getting-started.md#prerequisites):

```console
$ slipfork doctor                                    # probe the Btrfs substrate, the live subvolume round-trip, and unprivileged namespaces
$ slipfork snow resolve --file snow.yml --mode local  # validate modes, persist !! secrets (0600)
$ slipfork up                                        # build the world rootfs from your Dockerfile, boot it
$ slipfork fork feat-billing                         # second CoW world + .worktrees/feat-billing
$ slipfork list                                      # one origin URL per world
```

Once you have written a `slipfork.toml`, `slipfork dag plan` previews the artifact/fixture waves ([DAG planner](testing-dag.md)).

!!! info "One daemon, many thin clients"
    `slipfork` auto-spawns the unprivileged `slipforkd` daemon when `<runtime_dir>/daemon.sock` does not answer, so no systemd setup is required. `doctor` and `dag plan` are local diagnostics; `snow resolve` and every world command run through the daemon.

!!! warning "Btrfs only"
    The state directory (`~/.cache/slipfork` by default) must be a `btrfs` filesystem mounted `user_subvol_rm_allowed,noatime` and owned by your user; every world and sealed snapshot is one unprivileged subvolume. There is no second filesystem mode: `slipfork doctor` prints the exact `mount` line and exits `1` on anything else.

---

## What runs today

| Surface | Shipped behaviour |
|---|---|
| **Host preflight** (`slipfork doctor`) | One `statfs` of the state dir, a live scratch subvolume round-trip proving unprivileged create/snapshot/destroy, and `unshare --user`/`--net --ipc --pid` probes; text or `--json`, exit `1` with one `fix:` line per failed check, in `btrfs -> unshare_userns -> netns` order |
| **World rootfs** (`slipfork up`) | Read-only `L0` rootfs built from your `snow.yml` `images:` `Dockerfile`, content-addressed under `<state_dir>/cache/rootfs/<sha256>/app`, running as `$UID:$GID` |
| **Backing services** | `postgres:<v>`, `redis:<v>`, and SeaweedFS run as daemonless `crun` OCI containers sharing the world's `127.0.0.1`, and `slipfork db`/`redis` open their clients |
| **World lifecycle** (`up`/`down`/`fork`/`drop`/`list`/`prune`) | One Btrfs subvolume per world, cloned from a sealed read-only snapshot, one `WorldOrigin` URL per world, `--json` output, zero-setup daemon; `fork` creates a Git worktree or, when the checkout carries `.jj`, a Jujutsu workspace (`jj` on `PATH`, no Git fallback); `share` fails closed until tunnel sharing lands |
| **Inside a world** (`exec`/`shell`/`logs`/`hook run`/`forward`) | Daemon-resolved environment, native terminal stdio, retained component/hook logs, daemon-owned forward listeners |
| **`snow.yml` resolver** (`snow resolve`) | `modes.all` + `modes.<mode>`, mode-scoped components, `??` fallbacks, write-once `!!` CSPRNG secrets in a local `0600` vault ([`OSPP v1`](reference/ospp.md)) |
| **DAG planner** (`dag plan`) | SHA-256 Merkle digests per `[artifacts.<id>]` and topological `[fixtures.<id>]` waves |
| **OpenCode V2 plugin** (`@slipfork/opencode`) | Worktree → world binding, `cwd`-based shell routing through `slipfork exec`, session context hints |
| **`git-remote-slipfork`** | `git fetch` a running fork's branch and `refs/slipfork/<fork>/wip` over `slipfork://<fork>` |

Integration rows above are graded in the [integration support matrix](support-matrix.md); the OpenCode plugin is covered by its own package tests.

## Measured cost model

Owner-recorded figures for the single-subvolume world (provenance and open questions in `docs/research/btrfs-crun-benchmarks.md`; re-measurement is tracked separately):

| Operation | Measured |
|---|---:|
| Read-only snapshot of a world subvolume | **`19.5 ms`** |
| Writable clone of a world | **`11–24 ms`** |
| Clone to a warm-SQL world | **`39 ms`** |
| RAM per running world | **`6.3 MiB`** |

## Next steps

- **[Getting started](getting-started.md)** — mount Btrfs, probe, boot, fork, and work inside a world.
- **[`snow.yml` modes & secrets](snow-modes.md)** — mode resolution, `!!` generators, coordinates.
- **[Orthogonal DAG planner](testing-dag.md)** — configure `slipfork.toml` and read rebuild waves.
- **[OpenCode V2 plugin](agents-and-opencode.md)** — automatic worktree worlds for agents.
- **[CLI](reference/cli.md)** & **[manifest reference](reference/manifest.md)** — commands, flags, schemas.
- **[Support matrix](support-matrix.md)** — tiers, evidence, and what each integration guarantees.

## Roadmap corner (not yet shipped)

- **Planned:** tunnel sharing (`weaved` registrar), and unreferenced snapshot plus rootfs/OCI cache garbage collection in `prune`.
- **`0.2.0`:** polyglot parallel test orchestrator (`STP v1`, artifact/fixture snapshot materialization, pre-warmed Btrfs world slots, typed coverage, `SLIPFORK_OUTPUT_DIR`).
- **Later:** remote content-addressed snapshot/artifact cache export and turnkey agent skills.
