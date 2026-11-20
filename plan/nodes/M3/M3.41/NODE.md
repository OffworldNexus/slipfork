---
id: M3.41
title: "Proper visual Zensical documentation & native packaged distribution for all Linux distros (tpmd + blastit parity)"
kind: epic
status: ready
priority: 3
depends_on: [M3.40]
paths:
  - Makefile
  - README.md
  - CHANGELOG.md
  - zensical.toml
  - "docs/**"
  - "packaging/**"
  - "scripts/**"
  - ".github/**"
  - tools/check_docs_honesty.py
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-21T00:44"
updated: "2026-11-21T00:44"
---

# Goal

Deliver complete, problem-first visual Zensical documentation and native packaged distribution for **all Linux distros** modeled directly on `OffworldNexus/tpmd` and `/home/remy/offworld-nexus/blastit/.worktrees/release-0.2.0` (`specs/06-distribution-and-docs.md`):
1. **Visual Zensical Documentation (`zensical.toml`, `README.md`, `CHANGELOG.md`, `docs/`):**
   - Problem-first pages (`docs/index.md`, `getting-started.md`, `install.md`, `forkfile.md`, `cli.md`, `under-the-hood.md`, `ospp.md`, `agents-and-opencode.md`, `troubleshooting.md`) opening with user value + working `# forkfile: 0.1` snippets in the first 25 lines, Mermaid diagrams, Zensical `!!!` admonitions, custom `Forkfile` syntax highlighting (`docs/javascripts/forkfile-highlight.js` + WCAG AA CSS), `README.md`, and `CHANGELOG.md` (`0.1.0`).
   - Verified mechanically by `uvx --offline zensical build --strict` and `uv run --no-project tools/check_docs_honesty.py`.
2. **Native Packaged Distribution Across All Linux Distros (`packaging/` & `scripts/`):**
   - **Debian / Ubuntu (`.deb`, `amd64` + `arm64`)**: `packaging/deb/` + `scripts/build-deb.sh` (`dpkg-deb --root-owner-group`, measured ELF `GLIBC_X.Y` floor -> `Depends: libc6 (>= <floor>), libgcc-s1, crun, btrfs-progs`, socket-activated `:80` ingress units `/lib/systemd/system/slipfork-ingress.{socket,service}`, `/etc/apparmor.d/slipfork`, `slipfork(1)` manpage, `bash`/`zsh`/`fish` completions, `postinst`/`prerm`, `lintian`-clean).
   - **Fedora / RHEL / Rocky / Alma / openSUSE (`.rpm`, `x86_64` + `aarch64`)**: `packaging/rpm/slipfork.spec` + `scripts/build-rpm.sh` (`rpmbuild`, measured `glibc >= <floor>`, `Requires: crun, btrfs-progs`, systemd `:80` units, manpage, completions).
   - **Arch Linux (`.pkg.tar.zst` + AUR `PKGBUILD`/`.SRCINFO`, `x86_64` + `aarch64`)**: `packaging/arch/PKGBUILD` + `scripts/build-arch.sh` (systemd units, AppArmor profile, manpage, completions).
   - **Alpine Linux (`.apk` + OpenRC, `x86_64` + `aarch64`)**: `packaging/alpine/APKBUILD` + `scripts/build-apk.sh` (static `musl` binaries, OpenRC `:80` ingress init script, manpage, completions).
   - **Universal Portable Standalone Archives (`.tar.gz` + `SHA256SUMS` for `x86_64`/`aarch64` × `gnu`/`musl`)** + `scripts/check-portable-elf.sh` + `scripts/check-bundle-layout.sh`.
   - **Universal Installer (`docs/install.sh`) & Repo Publishing (`scripts/publish_packages.py`):** auto-detects `apt`, `dnf`, `yum`, `zypper`, `pacman`, `apk` to configure GPG-signed Cloudsmith (`offworld-nexus`) / Forgejo (`forge.offworld.nexus`) repos or installs SHA-256-verified standalone `.tar.gz` (`--user`, `--prefix`, `--develop`), plus Cloudsmith + Forgejo upload & 7-nightly retention pruning.
3. **GitHub Actions Workflows (`.github/workflows/{ci.yml, docs.yml, packages.yml, nightly.yml}`):**
   - Loop-backed Btrfs E2E CI on Linux runners, strict Zensical docs build/deploy, native `amd64` + `arm64` package matrix + multi-distro container install verification (`debian`, `ubuntu`, `fedora`, `rockylinux`, `archlinux`, `alpine`), and signed release/nightly publishing.

# Acceptance

- `make docs-check` (`uvx --offline zensical build --strict` + `tools/check_docs_honesty.py`) exits `0`.
- `make packages` builds real local artifacts (`.deb`, `.rpm`/bundle, Arch/Alpine/standalone `.tar.gz`, `SHA256SUMS`) and `make packages-test` (`check-portable-elf.sh`, `check-bundle-layout.sh`, `install.sh` syntax & `--prefix` install verification) exits `0`.
- `make check` exits `0`.
