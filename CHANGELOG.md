# Changelog

All notable, user-visible changes to Slipfork. The format follows Keep a Changelog, and versions
follow Semantic Versioning while the `# forkfile: 0.1` format is pre-`1.0`.

## [0.1.0] - 2026-11-21

### Added

- The `# forkfile: 0.1` language: `features` and `on` parity, enclosing `image` scopes, build-time
  `arg`, runtime `env` and `secret()`, `postgres` / `redis` / `s3` resources, `cdn` policies,
  `service` blocks with `route` rules and lifecycle hooks, and `@<tool>` auto-prequels.
- `slipfork up`, `down` and `restart` stream replaced-in-place boot phases and leave a failed world
  running for inspection, with one `next:` remedy per failure.
- `slipfork fork` snapshots the project and world data in under 50 ms and boots the fork on
  `http://<fork>--<world>.localhost`; `slipfork drop` reaps it and refuses a dirty checkout.
- `slipfork status` and `ps` render one dynamic-width tree of worlds, services, ports, routes and
  nested forks, with `--json` parity.
- `slipfork logs`, `shell`, `exec`, `db`, `redis`, `env`, `secret`, `open` and `auth` cover the
  daily loop and the remote `OSPP v1` pairing flow.
- `slipfork doctor` reports the three host requirements — Btrfs, namespaces, `crun` + `:80` ingress
  — with a `✓ Ready` verdict or one counted remedy per failing row.
- `slipfork fmt` and `slipfork migrate` format and convert `Forkfile`s; `slipfork completions`
  prints bash, zsh or fish scripts generated from the same verb table as `--help`.
- One per-user daemon serves every world behind a stateless `:80` `.localhost` ingress;
  `slipfork daemon install` writes the user unit and `sudo slipfork daemon install --ingress` adds
  the system socket and AppArmor profile.
- Native packages for Debian/Ubuntu (`.deb`), Fedora/RHEL/Rocky/Alma/openSUSE (`.rpm`), Arch
  (pacman + AUR) and Alpine (`.apk`), plus portable glibc/musl archives and a universal installer.
- A standalone `slipfork-lsp` language server with releases for Linux, macOS and Windows.

### Changed

- Terminal styling is TTY-aware: ANSI colours appear only on a real terminal with `NO_COLOR`
  unset and `TERM` not `dumb`; `--json` output carries zero escapes.
