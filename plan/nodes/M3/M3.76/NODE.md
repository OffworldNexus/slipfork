---
id: M3.76
title: "Prove every 0.1.0 package on every promised distro and finalise the offworld-dist release"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.75]
paths: [".github/workflows/**", "packaging/**", "scripts/**", docs/install.md, .offworld/project.toml]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-23T22:36"
updated: "2026-11-23T22:46"
---

# Goal

Align the 0.1.0 packaging pipeline with the updated `offworld-packaging` skill: build on the right platform, install-verify every artifact on every distro family the docs promise, and stage one truthful `offworld-dist` bundle for the publisher.

# Pre-baked findings

- `.github/workflows/packages.yml` already: builds deb/rpm/apk/pacman + four tarballs natively on `ubuntu-24.04`/`ubuntu-24.04-arm` (glibc 2.34 via `rockylinux:9`), builds the six `slipfork-lsp` archives natively (darwin/aarch64 on `macos-26`, darwin/x86_64 on `macos-15-intel`), runs the LSP out of its own archive, and verifies installs in `debian:12`, `debian:trixie`, `ubuntu:22.04`, `ubuntu:24.04`, `rockylinux:9`, `fedora:41`, `alpine:3.21`, `archlinux:latest` (+ `menci/archlinuxarm` for arm64).
- **Gap:** `docs/install.md` and `docs/install.sh` promise the zypper path (`opensuse`), but no verify leg exercises it. Add `opensuse/tumbleweed` (amd64) to the `verify` matrix and its own case:
  `zypper --non-interactive --gpg-auto-import-keys install --allow-unsigned-rpm /pkg/*.rpm`, then `/usr/bin/slipfork --version`, `slipfork-lsp --version`, `/usr/bin/slipfork --help | grep -q "^  up "`. Keep every existing leg and both architectures.
- `packaging/stage-dist.py` must stay byte-identical to
  `/home/remy/offworld-nexus/.opencode/skills/offworld-packaging/scripts/stage_dist.py` (verify with `diff -q`; re-copy verbatim only if it drifted).
- Re-read the `stage` job's `--file` list against the artifacts the build jobs actually produce and the formats declared in `.offworld/project.toml` (deb/rpm/apk/arch/tarball for `slipfork`; tarball/macos/windows/homebrew/scoop for `slipfork-lsp`). `--github-release` stays stable-only; `--homebrew-json`/`--scoop-json` stay.
- `docs/install.md`: lead with the org-standard one-liner from the skill
  (`curl -fsSL https://offworld-nexus.app/install.sh | sudo sh` then `sudo apt install slipfork`), keep the manual per-distro blocks (`apt.`/`rpm.`/`apk.`/`arch.`/`dl.` only — never internal hostnames), keep this project's own installer as a short alternative for `--user`/no-sudo/musl, and keep the very-experimental warning added by M3.75.

# Acceptance

- YAML parses; `actionlint` if available.
- Push and confirm on the PR run: every `verify` leg green (now 17: the eight images' arches plus openSUSE amd64), `packages`, `lsp`, and the `stage` dry-run manifest listing every artifact; `Docs` and `CI` green.
- `diff -q packaging/stage-dist.py <skill>/scripts/stage_dist.py` silent; `validate_project.py .offworld/project.toml` passes.
- `M3.76 -> done`, explicit-path commit `[M3.76] ...`.
