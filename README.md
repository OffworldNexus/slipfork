# Slipfork (`0.1.0`)

> **Copy-on-write worlds, one `snow.yml` manifest, and orthogonal build/fixture planning — on Linux, with zero `root:root` files and no `sudo` for day-to-day worlds. Root is needed only for the one-time Btrfs mount and the Ubuntu 24.04 AppArmor `userns` profile.**

```console
$ git clone https://github.com/OffworldNexus/slipfork && cd slipfork
$ cargo install --path crates/slipfork-engine    # installs `slipfork-world-init`
$ cargo install --path crates/slipfork           # `slipfork` + `slipforkd` + `git-remote-slipfork`
$ cd /path/to/your/project
$ slipfork doctor                                # probe the Btrfs substrate, the live subvolume round-trip, and unprivileged namespaces
$ slipfork snow resolve --file snow.yml --mode local  # validate modes, persist !! secrets in the local 0600 vault
$ slipfork up                                    # build the world rootfs from your Dockerfile, boot it
$ slipfork fork feat-billing                     # second CoW world + .worktrees/feat-billing
$ slipfork list                                  # one app URL per world
```

In a repository with a `snow.yml` that declares a build image and `modes.local` ([getting started](docs/getting-started.md)); a `slipfork.toml` is optional. Run the `slipfork` commands after the one-time Btrfs mount below.

Slipfork needs Linux `x86_64` and Rust 1.94+; everything else — the never-use-`tmpfs` warning, your `--state-dir` path, and the Ubuntu 24.04 AppArmor `userns` step — is in [getting started › Prerequisites](docs/getting-started.md#prerequisites). The state directory must be **Btrfs** mounted `user_subvol_rm_allowed,noatime` and owned by you; `mkfs.btrfs` erases that device:

```console
$ mkdir -p ~/.cache/slipfork && sudo mkfs.btrfs -L slipfork /dev/<device> && sudo mount -o user_subvol_rm_allowed,noatime /dev/<device> ~/.cache/slipfork && sudo chown "$(id -u):$(id -g)" ~/.cache/slipfork
```

Every world and sealed snapshot is one unprivileged Btrfs subvolume, so any other filesystem fails closed: `slipfork doctor` prints an equivalent `sudo mount -o user_subvol_rm_allowed,noatime` line and exits `1`. Static `crun` is **not** a host prerequisite: `slipforkd` downloads the pinned binary once (sha256-verified) into `<state_dir>/cache/runtime/` and stages it inside the world rootfs.

`slipfork` auto-spawns the unprivileged `slipforkd` daemon over a `0600` `<runtime_dir>/daemon.sock` when nothing answers, so no systemd setup is required. `doctor` stays local; `snow resolve` and every world command run through the daemon.

## What ships in `0.1.0`

- **`Dockerfile`-derived `L0` world rootfs** — content-addressed under `<state_dir>/cache/rootfs/<sha256>/app`, mounted read-only, executed as `$UID:$GID`, with `[world]` selecting the image and stage.
- **One Btrfs subvolume per world** — the read-only rootfs tree and `postgres`/`redis`/S3 state live inside it, so sealing a snapshot and cloning a world are single metadata operations with zero file copies.
- **Daemonless `crun` backing services** — `postgres:<v>`, `redis:<v>`, and SeaweedFS in OCI containers sharing the world's `127.0.0.1`, and `slipfork db`/`redis` run their clients inside the containers.
- **Daemon-served world lifecycle** — `up`, `down`, `fork`, `drop`, `list`, `prune`, `secret`, plus local `daemon unit` rendering and `exec`, `shell`, `logs`, `hook run`, `forward`, `auth`, `git-remote-slipfork`; `fork` creates a Git worktree or, when the checkout carries `.jj`, a Jujutsu workspace with zero Git fallback.
- **`snow.yml` resolver** — mode-scoped components and write-once `!!` secrets in the project's local `0600` vault.
- **OSPP v1 secrets** — pair once with `slipfork auth` (server-supplied `<url>#handle=…` page), then let the provider resolve CI values: they arrive HPKE-encrypted, are decrypted in `slipforkd` memory, and are injected into children only. The local vault stays local-only; remote secrets are never written to disk by Slipfork.
- **OpenCode V2 plugin** — worktrees become CoW worlds; shell commands route through `slipfork exec`.
- **Measured cost model** — `19.5 ms` read-only snapshot, `11–24 ms` writable clone, `39 ms` to a warm-SQL world, `6.3 MiB` RSS per world (owner-recorded; [`docs/research/btrfs-crun-benchmarks.md`](docs/research/btrfs-crun-benchmarks.md)).

## Documentation

- [Getting started](docs/getting-started.md)
- [`snow.yml` modes & secrets](docs/snow-modes.md)
- [OpenCode V2 plugin](docs/agents-and-opencode.md)
- [CLI reference](docs/reference/cli.md) · [Secrets & OSPP v1](docs/reference/ospp.md) · [Manifest reference](docs/reference/manifest.md)
- Internal design: [`ARCHITECTURE.md`](ARCHITECTURE.md), [`docs/decisions/`](docs/decisions/), [`docs/design/target-dx.md`](docs/design/target-dx.md) (nothing there is a shipped promise)

## Validation

```console
$ make check   # plan, hygiene, Python, TS, Rust fmt/clippy/doc, and Zensical --strict
```

## Where this is going (not yet shipped)

- **Planned:** unreferenced snapshot plus rootfs/OCI cache garbage collection in `prune`.
- **`0.2.0`:** polyglot parallel test orchestrator (`STP v1`, artifact/fixture snapshot materialization, pre-warmed Btrfs world slots, typed coverage, `SLIPFORK_OUTPUT_DIR`).
- **Later:** remote content-addressed snapshot/artifact cache export and turnkey agent skills.
