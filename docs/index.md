# Slipfork (`0.2.0`)

**Copy-on-write worlds, one `snow.yml` manifest, and orthogonal build/fixture planning — on Linux, with zero `sudo` and zero `root:root` files.**

```console
$ slipfork doctor                                    # probe CoW reflink, Incus socket, and unprivileged netns
$ slipfork snow resolve --file snow.yml --mode local  # validate modes, persist !! secrets (0600)
$ slipfork dag plan --file slipfork.toml --repo .     # hash artifacts independently from fixture waves
$ slipfork up                                        # boot the canonical world through zero-setup slipforkd
$ slipfork list                                      # one origin URL per world
```

!!! info "World lifecycle is daemon-served"
    `slipfork up`, `down`, `fork`, `drop`, `list`, `prune`, `share`, `secret`, `exec`, and `daemon unit` drive the unprivileged `slipforkd` daemon over a `0600` `<runtime_dir>/daemon.sock`; the CLI auto-spawns it with `--listen 127.0.0.1:0` when the socket is absent, so no systemd setup is required. `doctor`, `snow resolve`, and `dag plan` remain local diagnostics. Interactive `logs`, `shell`, `db`, `redis`, `forward`, `hook run`, `auth login`, `support`, and `git-remote-slipfork` land in the remainder of `0.2.0`.

---

## What Runs Today

| Surface | Shipped behaviour |
|---|---|
| **`snow.yml` resolver** (`slipfork snow resolve`) | `modes.all` + `modes.<mode>` inheritance, mode-scoped component add/override/`disabled: true`, `??` fallbacks, write-once `!!` CSPRNG secrets in a `0600` vault, and one exhaustive missing-value report |
| **Orthogonal DAG planner** (`slipfork dag plan`) | SHA-256 Merkle digests per `[artifacts.<id>]` and topological `[fixtures.<id>]` waves; frontend edits never invalidate database snapshots and migration edits never trigger frontend rebuilds |
| **Daemon-served world lifecycle** (`slipfork up` / `fork` / `down` / `drop` / `list` / `prune` / `share` / `secret` / `exec`, `slipforkd`) | CoW worlds cloned with `ioctl(FICLONE)`, exactly one `WorldOrigin` URL per world, an in-netns `127.0.0.1` loopback bridge to the daemon ingress, `0600` Unix sockets, and zero-setup daemon auto-spawn |
| **OpenCode V2 plugin** (`@slipfork/opencode`) | Worktree → world binding, `cwd`-based shell routing, and session hints — the gateway is updated to the daemon-served CLI in `M2.18` |

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

- **[Getting started](getting-started.md)** — build `slipfork`, probe the host, resolve `snow.yml`, plan the DAG, and boot a world through the zero-setup daemon.
- **[`snow.yml` modes & secrets](snow-modes.md)** — mode resolution rules, `!!` generators, and the single-`WorldOrigin` coordinate model.
- **[Orthogonal DAG planner](testing-dag.md)** — configure `slipfork.toml` and read Merkle rebuild waves.
- **[OpenCode V2 plugin](agents-and-opencode.md)** — worktree worlds, `cwd` routing, and when to enable it.
- **[CLI](reference/cli.md)** & **[Manifest reference](reference/manifest.md)** — the commands, flags, and manifest schemas shipped today.

---

## Roadmap Corner (Not Yet Shipped)

- **M2 (`0.2.0`):** daemon-served world lifecycle (shipped) plus the remaining CLI part 2 (`logs`, `shell`, `db`, `redis`, `forward`, `hook run`, `auth login`, `support`, `git-remote-slipfork`), `OSPP v1` WebSocket secrets and `[resources]` strategies (`ADR-0008`), Jujutsu workspaces, and the live `IncusEngine`.
- **M3 (`0.3.0`):** polyglot parallel test orchestrator (`RunnerProtocol v1`, pre-warmed world pools, typed coverage, surviving `SLIPFORK_OUTPUT_DIR`).
- **M4 (`0.4.0`):** remote content-addressed snapshot/artifact cache export and turnkey agent skills.
