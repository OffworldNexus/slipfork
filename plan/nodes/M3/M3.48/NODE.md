---
id: M3.48
title: "Build portable glibc<=2.34 gnu bundle in rockylinux:9 & fix Arch pacman -Syu + DisableSandbox in packages.yml"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths: [Makefile, "scripts/**", ".github/workflows/**", "packaging/**"]
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-22T10:57"
updated: "2026-11-22T11:13"
---

# Goal

Close the final 3 distro-install legs in `.github/workflows/packages.yml` (`rockylinux:9` arm64, `archlinux:latest` amd64, `menci/archlinuxarm:latest` arm64) so all 33/33 jobs across `Docs`, `CI`, and `Packages` pass 100% green:

1. **Portable `glibc <= 2.34` Linux-gnu Bundle (`rockylinux:9` `aarch64` & `x86_64`):**
   - On `ubuntu-24.04-arm` (`glibc 2.39`), compiling `slipfork` on the host linked against `GLIBC_2.39`, so `scripts/build-bundle.sh` measured `max-glibc: 2.39` and `rockylinux:9` (`glibc 2.34`) rejected `slipfork-0.1.0-1.aarch64.rpm` (`nothing provides glibc >= 2.39`), and on `x86_64` Rocky 9 emitted a `weak version GLIBC_2.39 not found` warning!
   - Fix `scripts/build-bundle.sh` / `Makefile` / `.github/workflows/{packages,nightly}.yml` so `make packages` builds the `gnu` bundle inside a `glibc 2.34` container (e.g. `rockylinux:9` with `rustup` / cached `/cargo` + `target` bind-mounted and `HOST_UID:HOST_GID` `EXIT` trap ownership restore, or `--gnu-container rockylinux:9` when host glibc > `2.34`, enforcing `--max-glibc 2.34`!).
   - Verify locally that the built `x86_64-unknown-linux-gnu` bundle has `max-glibc <= 2.34` AND **zero** `GLIBC_2.39` weak warnings when running `/usr/bin/slipfork --version` inside `rockylinux:9`!

2. **Arch Linux `pacman` Partial-Upgrade & Docker Landlock Sandbox (`packages.yml`):**
   - `archlinux:latest` (`amd64`) failed with `openssl-3: /usr/lib/libcrypto.so.3 exists in filesystem (owned by openssl)` because `packages.yml` ran `pacman -Sy` (partial upgrade, forbidden on rolling Arch) instead of `pacman -Syu`!
   - `menci/archlinuxarm:latest` (`arm64`) failed with `restricting filesystem access failed because the Landlock ruleset could not be applied: Operation not permitted / switching to sandbox user 'alpm' failed!` because `pacman 7`'s `DownloadUser` / Landlock sandbox is blocked inside standard Docker containers!
   - Fix both Arch `verify` commands in `.github/workflows/packages.yml` to disable `pacman`'s container sandbox (`sed -i 's/^DownloadUser/#DownloadUser/' /etc/pacman.conf && grep -q '^DisableSandbox' /etc/pacman.conf || sed -i '/^\[options\]/a DisableSandbox' /etc/pacman.conf`) and run full `pacman -Syu --noconfirm --needed glibc crun btrfs-progs && pacman -U --noconfirm /pkg/*.pkg.tar.zst && /usr/bin/slipfork --version && slipfork-lsp --version`!
   - Test both `rockylinux:9` and `archlinux:latest` container installs locally!

# Acceptance

- `make packages` & `make packages-test` exit `0` with `max-glibc <= 2.34` enforced.
- `docker run --rm -v "$PWD/dist/packages:/pkg:ro" rockylinux:9 sh -c "dnf -y -q install /pkg/*.rpm && /usr/bin/slipfork --version 2>&1 | (! grep -q GLIBC_2.39) && slipfork-lsp --version"` exits `0`.
- `docker run --rm -v "$PWD/dist/packages:/pkg:ro" archlinux:latest sh -c "sed -i 's/^DownloadUser/#DownloadUser/' /etc/pacman.conf && (grep -q '^DisableSandbox' /etc/pacman.conf || sed -i '/^\[options\]/a DisableSandbox' /etc/pacman.conf) && pacman -Syu --noconfirm --needed glibc crun btrfs-progs && pacman -U --noconfirm /pkg/*.pkg.tar.zst && /usr/bin/slipfork --version && slipfork-lsp --version"` exits `0`.
