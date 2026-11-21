---
id: M3.41.3
title: "Linux distro packages, portable standalone archives, installer & publisher"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: [M3.41.1]
paths: ["packaging/**", "scripts/**", docs/install.sh, Makefile, "editors/zed/checks/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-21T16:35"
updated: "2026-11-21T16:35"
---

# Goal

Build and verify real Linux distribution artifacts (deb/rpm/Arch/apk), portable standalone archives with checksums, the LSP asset set, the universal installer, and the Forgejo/Cloudsmith publisher.

# Context

- Repo has no `packaging/`, `scripts/`, `docs/install.sh`. Model 1:1 on `/home/remy/offworld-nexus/blastit/.worktrees/release-0.2.0/{packaging,scripts,.github}` and `/home/remy/offworld-nexus/tpmd/packaging/deb/{build-deb.sh,verify-install.sh,prune-nightlies.py,prune-forgejo-nightlies.py}`.
- `Makefile:24-53`: keep `check`, `test-tools`, `packages-check`, `guard`, `lsp-assets` green; add `packages` and `packages-test`. `lsp-assets` (`Makefile:55`) already pins the six Zed asset names.
- Static unit copies must match the Rust constants byte-for-byte in shape: `crates/slipfork/src/install.rs:33-95` — `USER_UNIT` -> `slipfork-daemon.service`; `INGRESS_SOCKET` (`127.0.0.1:80`, `Accept=yes`, `NoDelay=true`) -> `/lib/systemd/system/slipfork-ingress.socket`; `INGRESS_SERVICE` -> `slipfork-ingress@.service`; `APPARMOR_PROFILE` -> `/etc/apparmor.d/slipfork`. Do not fork the unit text; if a shared source is needed, generate at build time from the binary or document a single copy.
- `editors/zed/src/lib.rs:60-95` `asset_for` requires exactly `slipfork-lsp-{linux,darwin}-{x86_64,aarch64}.tar.gz` and `slipfork-lsp-windows-{x86_64,aarch64}.zip`, each archive containing a top-level `slipfork-lsp-<platform>-<arch>/` dir with the `slipfork-lsp[.exe]` payload.
- Host tools present: `dpkg-deb`, `zstd`, `docker`, `sh`, `dash`; `uvx --offline`. ABSENT: `rpmbuild`, `makepkg`, `abuild`, `lintian`, `actionlint`. Build rpm in `rockylinux:9` and apk in `alpine:3.21` containers; assemble Arch `.pkg.tar.zst` host-natively with `tar` + `zstd` plus a valid `.PKGINFO` (and `PKGBUILD`/`.SRCINFO` for AUR).
- `specs/06-distribution-and-docs.md:13-174` §1-§8 pins the artifact matrix, dependency sets, portable archives (`gnu`+`musl`), `docs/install.sh` flags (`--user --prefix --develop --nightly --version --mirror -y`), and publishing retention (7-nightly groups). Never hardcode a glibc floor: measure with `readelf` in `check-portable-elf.sh` (`--max-glibc`), allowlist `DT_NEEDED`, refuse `RUNPATH`.
- No `LICENSE` file exists; synthesize the same notice the `lsp-assets` target writes rather than inventing a license.

# Acceptance

- `packaging/{deb,rpm,arch,alpine}/` recipes + `scripts/build-bundle.sh build-deb.sh build-rpm.sh build-arch.sh build-apk.sh build-standalone.sh build-lsp.sh check-portable-elf.sh check-bundle-layout.sh publish_packages.py` all exist and run.
- Binaries/payload per package: `slipfork`, `slipfork-world-init`, manpage `slipfork.1`, bash/zsh/fish completions generated from the real binary at build time, static systemd units, AppArmor profile.
- `publish_packages.py --dry-run` exercises base-version, release stamp, stamp, push, forgejo, prune; Cloudsmith fails closed, Forgejo best-effort, 7-nightly group retention.
- `docs/install.sh` is POSIX `sh` with apt/dnf/yum/zypper/pacman/apk detection, Cloudsmith/Forgejo repos, SHA-256-verified standalone fallback, `--user/--prefix/--develop/--nightly/--version/--mirror/-y`, and a clean non-Linux exit that names `slipfork-lsp`.
- `make packages` exits 0 producing real host-arch `.deb`, `.rpm`, `.pkg.tar.zst`, `.apk`, `.tar.gz` + `SHA256SUMS` in `dist/packages`; `make packages-test` exits 0 (portable-ELF + bundle-layout + `bash -n`/`dash -n docs/install.sh` + local `--prefix` install from a `file://` archive); `make check` stays green.
