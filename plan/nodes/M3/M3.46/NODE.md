---
id: M3.46
title: "Fix GitHub Actions CI Btrfs loop-mount (user_subvol_rm_allowed) & enable packages.yml dry-run matrix on PRs"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths: [".github/workflows/**", Makefile, "packaging/**", "scripts/**", "crates/*/tests/critical_*.rs"]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-22T10:16"
updated: "2026-11-22T10:25"
---

# Goal

Fix all GitHub Actions workflow issues discovered on PR #2 so `CI`, `Docs`, and `Packages` (dry-run rehearsal) all pass 100% green on GitHub Actions runners:

1. **`.github/workflows/ci.yml` (`live` job):**
   - Line 101 mounted `/mnt/slipfork-live` with `-o loop` instead of `-o loop,user_subvol_rm_allowed,noatime`, causing unprivileged `BTRFS_IOC_SNAP_DESTROY_V2` to fail `EPERM` (`Operation not permitted`) in `critical_btrfs_live`!
   - Fix mount to `sudo mount -o loop,user_subvol_rm_allowed,noatime "$image" /mnt/slipfork-live` (and increase `$image` to `12G` on `/mnt` so pulling `postgres:18`, `redis:7`, `python:3.13-slim`, and `axllent/mailpit` never hits `ENOSPC`).
   - Use `sudo mount -o remount,size=4G /dev/shm` when resizing `/dev/shm`, and `sudo rm -f /mnt/slipfork-live.img || true` in the `always()` cleanup step (currently `rm -f` fails `Permission denied` because `sudo truncate` created it as root!).
2. **`Makefile` & `.github/workflows/nightly.yml` `MUSL_IMAGE` (`rust:alpine`, NOT `rust:1.100-alpine`):**
   - `rust:1.100-alpine` is a local dev image tag, NOT an official Docker Hub tag (use `rust:alpine` on Docker Hub!).
3. **`.github/workflows/packages.yml` & `.github/workflows/nightly.yml` GitHub Repository Casing (`OffworldNexus/slipfork`):**
   - Fix `github.repository == 'offworld-nexus/slipfork'` -> `github.repository == 'OffworldNexus/slipfork'` (the GitHub org is `OffworldNexus`, while the Cloudsmith org slug remains `offworld-nexus`).
4. **`.github/workflows/packages.yml` PR Rehearsal & `rockylinux:9` RPM Dependencies:**
   - Add `pull_request: branches: [develop]` to `packages.yml` (`should_publish` stays `false` on `pull_request`, running `--dry-run` in `publish`) so PR #2 executes the full `amd64` + `arm64` `.deb`/`.rpm`/Arch/Alpine/portable build + 12-distro `install` matrix + 6-target `slipfork-lsp` matrix live on GitHub Actions!
   - Check `packaging/rpm/slipfork.spec` vs `rockylinux:9`: RHEL 9 / Rocky Linux 9 removed `btrfs-progs` from base repositories (only Fedora/openSUSE carry `btrfs-progs` in base repos; `crun` is in Rocky 9 AppStream). Use `Recommends: btrfs-progs` (keep `Requires: crun`) in `packaging/rpm/slipfork.spec` so `dnf install /pkg/*.rpm` succeeds cleanly on both `fedora:41` and `rockylinux:9`! Test `docker run --rm -v ... rockylinux:9 dnf -y install /pkg/*.rpm` locally to prove it!

# Acceptance

- `make check`, `make docs-check`, `make packages`, and `make packages-test` (including real `rockylinux:9` `dnf install` check) exit `0` locally.
- `uv run --no-project tools/guard.py` and `uv run --no-project tools/plan.py validate` exit `0`.
