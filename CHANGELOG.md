# Changelog

All notable changes to Slipfork are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## 0.1.0 (2026-11-15)

First public release: copy-on-write worlds, `snow.yml` v2 mode/secret resolution,
orthogonal artifact & fixture DAG planning, and OpenCode V2 worktree isolation.

### Added

- **`slipfork-world`** — validated `ProjectSlug`, `WorldTarget` (canonical / user fork / `test-<n>` slot), encapsulated `LocalHostRoute` for `*.localhost`, and the pure `TunnelRegistrar` / `TunnelLease` ports with a shared conformance suite.
- **`slipfork-snow`** — `snow.yml` v2 parser (`modes.all` + `modes.<mode>`, two-level `components.<id>.run_command` validation), `??` fallbacks, write-once `!!` CSPRNG secrets, exhaustive missing-key diagnostics, `SecretProvider` + `FileVaultProvider` (`0600`), and `ResourceCoordinates` built on `slipfork-world`.
- **`slipfork-dag`** — `slipfork.toml` configuration, SHA-256 Merkle input hashing, digest cache, and the orthogonal artifact + fixture snapshot wave planner.
- **`slipfork-engine`** — `WorldEngine` port with `ReflinkNetnsEngine` (`ioctl(FICLONE)` + unprivileged user/net/PID namespaces preserving host `$UID:$GID`) and an `IncusEngine` Unix-socket REST `/1.0` library adapter awaiting M2 CLI wiring.
- **`slipfork` CLI** — `doctor`, `snow resolve`, `dag plan`, `snap`, `fork`, `exec`, and `drop`, plus fail-closed canonical VCS `ProjectSlug` discovery from Git or Jujutsu roots (ADR-0005 §3).
- **`@slipfork/opencode`** — OpenCode V2 plugin wiring `ctx.worktree.transform`, `ctx.shell.hook("create.before")`, and `ctx.session.hook("context")` for implicit per-worktree CoW worlds and `cwd`-routed shell commands.
- **Architecture decisions** — `ADR-0001` (snow modes, mode-scoped components & secret protocol), `ADR-0002` (world engine IoC, Incus & reflink), `ADR-0003` (orthogonal artifact & origin-classified fixture DAG), `ADR-0004` (OpenCode implicit worktree routing), `ADR-0005` (single-origin world invariant, `.localhost` edge, `weaved` leases & VCS remote), `ADR-0006` (manifest plumbing law, polyglot runner protocol, world-owned stack & built-in typed coverage), and `ADR-0007` (integration support tiers & evidence registry).
- **Measured Milestone 1 benchmark** — reproducible `make bench` harness running native PostgreSQL 18.7, XFS `FICLONE` snapshots, isolated `netns` worlds, and headless Chromium (Playwright) across baseline, cold-CoW, and pre-warmed pool regimes (`docs/research/m1-poc-benchmark.md`).
