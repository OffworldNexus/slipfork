# Slipfork (`0.1.0`)

**Copy-on-write world primitives, `snow.yml` v2 secret/mode resolution, orthogonal DAG planning, and OpenCode V2 worktree isolation.**

Slipfork `0.1.0` provides the core Rust CLI, domain crates, and OpenCode V2 plugin for snapshotting and cloning stateful service directories in milliseconds on Linux (`XFS` `reflink=1` / `Btrfs` + unprivileged user/network namespaces, zero `sudo`, zero `root:root` files):

```console
$ slipfork doctor                                            # Probe CoW FICLONE latency & netns support
$ slipfork snow resolve --file snow.yml --mode local         # Validate snow.yml & persist !! secrets (0600)
$ slipfork dag plan --file slipfork.toml --repo .            # Compute orthogonal artifact & fixture waves
$ slipfork snap --source ./state canonical                   # Capture a CoW snapshot via ioctl(FICLONE)
$ slipfork fork exp --from canonical --workspace .           # Clone snapshot into an isolated CoW world
$ slipfork exec --world exp --snow-file snow.yml -- pytest   # Run inside private netns under host $UID:$GID
$ slipfork drop exp                                          # Destroy the cloned world in milliseconds
```

---

## What `0.1.0` Solves Today

| Capability | Shipped in `0.1.0` |
|---|---|
| **Single `snow.yml` v2 manifest** | `modes.all` + `modes.<mode>` inheritance, 2-level `components.<id>.run_command` override validation (M2 process supervision), `??` fallbacks, write-once `!! random_string(50)` secret persistence (`0600`), and exhaustive missing-key diagnostics |
| **Orthogonal DAG planning** | `slipfork dag plan` hashes `[artifacts.*]` independently from `[fixtures.*]` (`10.9 ms` warm check): frontend edits never invalidate DB fixture digests, and migration edits never trigger frontend rebuilds |
| **Rootless CoW cloning & private `netns`** | `slipfork snap`, `fork`, `exec`, and `drop` clone files via kernel `ioctl(FICLONE)` (failing closed on `tmpfs`), isolate `127.0.0.1` ports (`:5432`, `:8000`) per world, and preserve your exact `$UID:$GID` |
| **Implicit OpenCode worktree worlds** | [`@slipfork/opencode`](agents-and-opencode.md) forks a CoW world when OpenCode creates a git worktree and wraps `shell` commands into `slipfork exec --world ...` automatically |

---

## Measured `0.1.0` Benchmark (PostgreSQL 18.7 + XFS `FICLONE` + Playwright)

Reproduce locally with `make bench`:

| Operation / Suite (`16` full browser + committed SQL checkout tests) | Measured Result |
|---|---:|
| 64 KiB XFS `ioctl(FICLONE)` probe | **`397 us`** |
| `slipfork fork` on a 41.5 MiB seeded PostgreSQL 18 cluster | **`44.4 ms`** |
| 4 concurrent worlds binding `127.0.0.1:5432` in parallel | **`413.6 ms`** (`0` cross-world leaks) |
| Traditional sequential `TRUNCATE` + re-seed baseline | `6.07 s` (`380 ms`/test) |
| Pre-warmed Slipfork CoW worlds (`-j 8`, `0 ms` SQL teardown) | **`1.89 s` (`118 ms`/test, `3.21x` faster)** |

---

## Next Steps

- **[Getting started](getting-started.md)** — Build `slipfork`, verify host CoW capabilities, resolve `snow.yml`, and fork/exec an isolated world.
- **[`snow.yml` modes & secrets](snow-modes.md)** — `modes.all`, `!!` CSPRNG secret generators, `.localhost` coordinate rules, and `LayeredSecretProvider`.
- **[Orthogonal DAG planner](testing-dag.md)** — Configure `slipfork.toml` and inspect Merkle rebuild waves with `slipfork dag plan`.
- **[OpenCode V2 plugin](agents-and-opencode.md)** — Wire `@slipfork/opencode` for implicit per-worktree CoW worlds.
- **[CLI (`0.1.0`)](reference/cli.md)** & **[Manifest reference](reference/manifest.md)** — Exact flags and schemas shipped today.

---

## Roadmap Corner (Upcoming Milestones — Not Yet in `0.1.0`)

- **M2 (`0.2.0`):** Single-origin worlds (`WorldOrigin = Local | Shared` with `308` `.localhost` redirect when shared, running on `FileVaultProvider` without requiring OSPP), port-80 `slipforkd` edge proxy, Tunnel Weaver (`weaved`) leases (`H-006`), `git-remote-slipfork` (`<fork>` + `<fork>/wip`), Jujutsu (`jj workspace`) adapter, live `IncusEngine`, and evidence-backed integration support tiers (`ADR-0007`).
- **M3 (`0.3.0`):** Automated polyglot test orchestrator (`RunnerProtocol v1`, world-owned stacks + `pytest-slipfork` tracked via `ADR-0007` support tiers, pre-warmed CoW pools, built-in typed multi-world coverage, and surviving `SLIPFORK_OUTPUT_DIR` for Blastit, Allure, and Playwright traces).
- **M4 (`0.4.0`):** Remote CI/team content-addressed snapshot & artifact cache export/import and agent skills.
