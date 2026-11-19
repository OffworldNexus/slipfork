# Changelog

All notable changes to Slipfork are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## 0.1.0 (2026-11-17)

First public release. Slipfork boots the `Forkfile` as a disposable Btrfs world,
forks the whole world in milliseconds, and injects configured secrets without
copying `.env` files.

### Added

- **`Forkfile`** — one procedural manifest per repository: `project`, `features`,
  `postgres` / `redis` / `s3` / `domain` resources, reusable `env` groups,
  `service` blocks, `on` conditionals, and `task`. Statements run top to bottom,
  and the whole-file checker reports every error in every branch with
  rustc-style spans.
- **`slipfork lsp`** — the same checker and hover documentation over stdio for
  Zed, Neovim, and VS Code.
- **`slipfork migrate`** — one-way, statically verified conversion of an
  upstream Snow manifest into a `Forkfile`.
- **Btrfs world engine** — one unprivileged subvolume per world holding the
  read-only service rootfs plus PostgreSQL, Redis, and S3 state. Sealing a base
  and cloning a world are metadata-only subvolume operations.
- **Per-service `crun` containers** — every `service` runs in its own container,
  with `postgres:<v>`, `redis:<v>`, and SeaweedFS alongside it in the world's
  `127.0.0.1`. `db` and `redis` open clients inside those containers.
- **CoW volumes** — `volume .venv` mounts a writable directory that
  `slipfork fork` clones in milliseconds while source files stay live-mounted.
- **Fail-closed host probe** — `slipfork doctor` checks the Btrfs super-magic,
  runs a live create/snapshot/destroy round-trip, and probes unprivileged
  `userns`/`netns`; it refuses any non-Btrfs state directory with the exact
  `mount -o user_subvol_rm_allowed,noatime` remedy.
- **`slipforkd`** — the sole stateful daemon: a `0600` control socket, HTTP
  ingress, seed-dump restore, and OSPP subscriptions. Runs unprivileged.
- **Thin auto-spawning CLI** — `status`, `doctor`, `up`, `down`, `restart`,
  `ps`, `open`, `fork`, `drop`, `prune`, `exec`, `run`, `env`, `shell`, `logs`,
  `hook run`, `db`, `redis`, `forward`, `secret`, `daemon unit`,
  `daemon install`, `auth`, `migrate`, and `lsp`.
- **`slipfork daemon install`** — one root command writes
  `/etc/systemd/system/slipforkd.{socket,service}` plus
  `/etc/apparmor.d/slipfork`, enables `:80` socket activation, and releases a
  zero-setup daemon holding the control socket. Port-less
  `http://<project>.localhost` origins follow.
- **`Dockerfile`-derived rootfs** — each service's named build stage is exported
  by `buildx`, cached content-addressed under `<state_dir>/cache/rootfs`,
  mounted read-only, and executed as `$UID:$GID`.
- **`--host-caches`** — read-only mounts of Cargo, rustup, uv, and pnpm caches
  after an ELF/libc ABI probe; never a manifest key.
- **Secret expressions** — `secret(KEY) ?? <generator>` writes once to a local
  `0600` vault, `var(KEY) ?? <fallback>` handles local overrides, and any
  tainted value prints as `[REDACTED]` until `--reveal`.
- **OSPP v1 secrets** — P-256 challenge-response, browser-fragment pairing,
  HPKE-encrypted values decrypted in `slipforkd` memory, live `env_updated`
  pushes, ID-only claims, and encrypted PostgreSQL seed dumps. Pair once;
  `<state_dir>/ospp.json` stores only `{endpoint, account, machine}`.
- **`git-remote-slipfork`** — fetch a running fork's branch and
  `refs/slipfork/<fork>/wip` over `slipfork://<fork>`.
- **Jujutsu workspaces** — a checkout carrying `.jj` forks through
  `jj workspace add`; missing `jj` fails closed instead of using Git.
- **`@slipfork/opencode`** — OpenCode V2 plugin: worktree create forks a world,
  `cwd`-routed shell commands run through `slipfork exec`, and session context
  names the world and its origin.
- **Measured cost model** — owner-recorded `19.5 ms` read-only snapshot,
  `11–24 ms` writable clone, `39 ms` warm-SQL clone, and `6.3 MiB` RSS per
  world, reproducible with `make bench`.

### Changed

- Every world boots from the validated read-only rootfs; writable state is
  confined to the worktree mount, `/.slipfork/home`, `tmpfs`, and
  `<state_dir>/{pgdata,redis,s3}`.
- Services and backing containers share one supervision surface, so logs, entry
  commands, and `ServiceNotRunning` diagnostics are identical.

### Notes

- Linux `x86_64` only; Btrfs is mandatory and there is no second backend.
- Pre-`1.0`: no compatibility shims, aliases, or deprecations are provided.
