# Changelog

All notable, user-visible changes to Slipfork. The format follows Keep a Changelog, and versions
follow Semantic Versioning while the `# forkfile: 0.1` format is pre-`1.0`.

## [0.1.0] - 2026-11-23

### Added

- `slipfork shell <target>` requires its target and forwards terminal size over `SIGWINCH`; piped
  postgres input runs `psql -X -q -v ON_ERROR_STOP=1`.
- State guidance for `running`, `stopped`, `cached`, `up to date` and `orphaned`, each with one
  `next:` command.
- The `# forkfile: 0.1` language: `features` and `on` parity, enclosing `image` scopes, build-time
  `arg`, runtime `env` and `secret()`, `postgres` / `redis` / `s3` resources, `cdn` policies,
  `service` blocks with `route` rules and lifecycle hooks, and `@<tool>` auto-prequels.
- `slipfork up`, `down` and `restart` stream boot phases and leave a failed world running for
  inspection, with one `next:` remedy per failure.
- `slipfork fork` snapshots the project and world data in under 50 ms and boots the fork on
  `http://<fork>--<world>.localhost`; `slipfork drop` reaps it and refuses a dirty checkout.
- `slipfork status` and `ps` render one dynamic-width tree of worlds, domains and nested forks, with
  `--json` parity.
- `slipfork doctor` reports the three host requirements — Btrfs, namespaces, `crun` + `:80` ingress
  — with a `✓ Ready` verdict or one counted remedy per failing row.
- `slipfork fmt` and `slipfork migrate` format and convert `Forkfile`s; `slipfork completions`
  prints bash, zsh or fish scripts generated from the same verb table as `--help`.
- One per-user daemon serves every world behind a stateless `:80` `.localhost` ingress; package
  install hooks enable and start both the per-user daemon and the system `:80` socket for the
  installing user, and rootless installs stay silent.
- Native packages for Debian/Ubuntu (`.deb`), Fedora/RHEL/Rocky/Alma/openSUSE (`.rpm`), Arch
  (pacman + AUR) and Alpine (`.apk`), plus portable glibc/musl archives and a universal installer.
- A standalone `slipfork-lsp` language server with releases for Linux, macOS and Windows.

### Changed

- Packaging is zero-secret: CI builds and install-verifies every package, then stages one
  `offworld-dist` bundle that the organization publisher signs and syncs. Install docs and
  `install.sh` now use only `apt|rpm|apk|arch|dl.offworld-nexus.app`.
- Boot progress uses a fixed `DECSTBM` bottom bar instead of multi-progress: one aligned row per
  task, zero sha256/Dockerfile/hook lines in the terminal, truthful `cached`, and all raw output in
  `<store>/logs/<world>/<id>.log`. `slipfork logs <image>` reads an image build log.
- The status tree lists worlds flat with uniform two-level domains; human output drops ports and
  uptime ages, while `--json` keeps `port`, `started_at_ms`, `url`, `routes` and `orphaned`.
- Every `s3` bucket routes at `http://<bucket>.<world>.localhost`. `cdn <id>` is a cloud binding
  only: locally `<id>.domain` and `<id>.url` resolve to the bucket's own URL, with no separate CDN
  hostname.
- `slipfork up` is idempotent while the `Forkfile` content digest and features are unchanged, and
  otherwise reconciles the world, reaping undeclared pods that showed as `orphaned`.
- `up`, `down` and `restart` take positional targets: `slipfork up [TARGET]`, `down [TARGET]` and
  `restart [TARGET] [-s|--service <SERVICE>]`, where `-s` always names a service. `up` preflights
  the target's `Forkfile` before touching the daemon and ends at `✓ Ready` with no trailing
  reminder.
- `ps` and `status` are honest: stopped checkouts under `.forks/*` show a dim
  `○ <fork>  ~/demo/.forks/<fork>  stopped` row and work with `up <fork>`/`drop <fork>`; a vanished
  checkout shows `! checkout missing — …`; an uncompilable `Forkfile` shows
  `! Forkfile invalid: line:col: …` while live pods stay `running`, with no fabricated `orphaned`
  rows.
- Terminal styling is TTY-aware: ANSI colours appear only on a real terminal with `NO_COLOR`
  unset and `TERM` not `dumb`; `--json` output carries zero escapes.

### Removed

- `slipfork auth` (OSPP pairing) is not part of 0.1.0 and returns in a later release; local
  `secret()` values keep working.
- The `db` and `redis` subcommands are gone. `slipfork shell <target>` is the one interactive entry point:
  real-PTY `bash`/`sh` for services and images, `psql` for postgres databases, `redis-cli` for
  redis, and an exact refusal for S3 buckets.

### Fixed

- The `DECSTBM` boot bar advances through a real 2D VT100 grid, so the prompt returns to the row
  the shell expects and wide glyphs no longer drift the flow cursor.
- A failed `slipfork fork` rolls back atomically: it stops its supervisor and pods and drops the
  registry entry before unlinking snapshots, so no half-booted fork survives.
