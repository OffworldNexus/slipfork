# Changelog

All notable changes to Slipfork are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## 0.1.0 (2026-11-17)

First public release: Btrfs copy-on-write worlds forked in milliseconds, a
daemon-served CLI, `snow.yml` mode/secret resolution, the orthogonal artifact &
fixture DAG planner, and OpenCode V2 worktree isolation.

### Added

- **`BtrfsEngine`** — the sole `WorldEngine`: one unified Btrfs subvolume per world (the `Dockerfile`-derived `L0` rootfs plus PostgreSQL, Redis, and S3 state), read-only snapshots, writable clones, and a fail-closed `KernelBtrfsDriver` probe that refuses any non-Btrfs state directory with `EngineError::BtrfsRequired { path, actual_fstype, mount_hint }` (`ADR-0010`).
- **`slipforkd`** — the sole stateful Composition Root and unprivileged daemon: TCP HTTP/WS ingress, a `0600` `daemon.sock`, a `0600` `http-ingress.sock` bridge for in-world `$BASE_URL`, inherited `LISTEN_FDS` activation, and explicit `--state-dir`/`--runtime-dir`/`--world-init`/`--crun` inputs.
- **Thin auto-spawning CLI** — `up`, `down`, `fork`, `drop`, `list`, `prune`, `secret set|unset|list`, `share`, `daemon unit`, `exec`, `shell`, `logs`, `hook run`, `db`, `redis`, `forward`, `auth login`, and `support` drive the daemon over `daemon.sock`; when nothing answers, the CLI starts `slipforkd --listen 127.0.0.1:0` in the background.
- **`Dockerfile`-derived `L0` world rootfs** — a `snow.yml` `BuildImage` stage is exported with `docker buildx build --output type=local`, cached content-addressed under `<state_dir>/cache/rootfs/<sha256>/app`, mounted read-only, validated against the World Image Contract, and executed as `$UID:$GID` (`ADR-0009`).
- **`[world]` manifest selector** — two optional keys in `slipfork.toml`: `world.image` (required when `snow.yml` declares several `BuildImage` entries, inferred for one) and `world.stage` (named `Dockerfile` stage, inferred when the final stage is named); unknown keys and unbuildable projects fail closed.
- **`--host-caches`** — per-invocation, ABI-gated read-only mounts of `~/.cargo`, `~/.rustup`, `~/.cache/uv`, `~/.local/share/uv`, and the pnpm store; never a `slipfork.toml` key.
- **Daemonless `crun` backing services** — `pg_clusters` → `postgres:<version>`, `redis_clusters` → `redis:<version>`, and non-empty `s3_buckets` → SeaweedFS run as OCI containers sharing the world's `127.0.0.1`; `slipfork db`/`redis` open `psql`/`redis-cli` inside them, and `hook run`/`logs`/`forward`/`shell` give interactive and retained-log access.
- **`git-remote-slipfork`** — a `slipfork://<fork>` Git remote that lists and fetches a running fork's branch and `refs/slipfork/<fork>/wip`.
- **`slipfork support`** and the generated **[integration support matrix](docs/support-matrix.md)** — ten integrations graded `declared` with verified recipe pages and protected critical tests, enforced by `tools/check_support.py`.
- **`slipfork auth login`** — OSPP device-key pairing from a `#handle=` URL, read from `argv` or stdin.
- **`LayeredSecretProvider`** — `slipforkd` resolves `secrets::`/`vars::` from the `0600` local vault first (the sole `!!` and `secret set|unset` write sink) and from a paired `OSPP v1` `env.bundle` second; an unreachable paired vault fails the render closed.
- **Container-restored seed dumps and sealed bases** — a paired OSPP session can offer one PostgreSQL `pg_dump -Fc` archive per database; `up`/`fork` stream it into a verified `<state_dir>/dumps/<sha256>.pgdump`, restore it with `pg_restore` inside the `postgres:<v>` container, and `freeze_world` the result as `<project>-base-<digest>` so every later world clones the base instead of restoring again (`--no-dump` / `--refresh-dump`).
- **Jujutsu workspaces** — a checkout carrying `.jj` forks through `JjWorkspaceAdapter` (`jj workspace add`, `@-`/`@` projected into the colocated Git store); `jj` is required on `PATH` and its absence fails closed with `ToolMissing`, never a Git fallback.
- **Optional `slipfork.toml`** — `up`/`fork` boot with defaults when the file is absent; a malformed file still fails closed.
- **`slipfork-snow`** — `snow.yml` v2 parser (`modes.all` + `modes.<mode>`, two-level `components.<id>.run_command` validation), `??` fallbacks, write-once `!!` CSPRNG secrets, exhaustive missing-key diagnostics, `SecretProvider` + `FileVaultProvider` (`0600`), and `ResourceCoordinates` built on `slipfork-world`.
- **`slipfork-dag`** — `slipfork.toml` configuration with the fail-closed typed `[test]` coverage schema and origin-classified `[fixtures.<id>].per_world` fixtures, SHA-256 Merkle input hashing, digest cache, and the orthogonal artifact + fixture snapshot wave planner.
- **`slipfork-world`** — validated `ProjectSlug`, `WorldTarget` (canonical / user fork / `test-<n>` slot), encapsulated `LocalHostRoute` for `*.localhost`, and the pure `TunnelRegistrar` / `TunnelLease` ports with a shared conformance suite.
- **Integration support registry** — typed `support/integrations.toml` (`SupportRegistry`, `IntegrationTier`, `IntegrationEntry`, `Evidence`) embedded at compile time with fail-closed tier→evidence validation (`ADR-0007`).
- **`@slipfork/opencode`** — OpenCode V2 plugin wiring `ctx.worktree.transform`, `ctx.shell.hook("create.before")`, and `ctx.session.hook("context")` for implicit per-worktree CoW worlds and `cwd`-routed shell commands.
- **Architecture decisions** — `ADR-0001` (snow modes, mode-scoped components & secret protocol), `ADR-0002` (world engine IoC on Btrfs), `ADR-0003` (orthogonal artifact & origin-classified fixture DAG), `ADR-0004` (OpenCode implicit worktree routing), `ADR-0005` (single-origin world invariant, `.localhost` edge, `weaved` leases & VCS remote), `ADR-0006` (manifest plumbing law, `STP v1` direction, world-owned stack & typed coverage), `ADR-0007` (integration support tiers & evidence registry), `ADR-0008` (OSPP v1, resource strategy & world UX), `ADR-0009` (world base image & image contract), and `ADR-0010` (Btrfs-only world engine).
- **Measured Btrfs benchmark** — reproducible `make bench` harness recording read-only snapshot, writable clone, warm-SQL, and per-world RSS numbers in `docs/research/btrfs-crun-benchmarks.md`.

### Changed

- **World execution:** every world boots from the validated read-only `L0` rootfs inside a single Btrfs subvolume, with writable state confined to the worktree mount, `/.slipfork/home`, `tmpfs`, and `<state_dir>/{pgdata,redis,s3}`.
- **Service runtime:** app components and `crun` containers share one `ServiceRuntime` supervision surface, so freeze, logs, entry commands, and `ServiceNotRunning` diagnostics behave identically for both.
- **Secrets and modes:** `modes.all` / `modes.<mode>` resolution is the only mode engine, and the `0600` file vault is layer 1 of a `LayeredSecretProvider` whose second layer is the paired `OSPP v1` `env.bundle` (read-only); `snow.yml` modes fully drive supervisory components.
- **OpenCode plugin:** `@slipfork/opencode` drives the daemon-served CLI (`fork`, `drop`, `list`, `exec`) with explicit project state and a live end-to-end test.

No compatibility shims are provided for any `0.1.0` surface: the workspace is pre-`1.0`, and the single Btrfs engine is the only supported world backend.
