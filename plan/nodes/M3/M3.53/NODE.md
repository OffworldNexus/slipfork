---
id: M3.53
title: "Migrate packaging & distribution to offworld-packaging (stage-dist.py, *.offworld-nexus.app, zero Cloudsmith/Forgejo) & align H-013 live-test assertions"
kind: epic
status: done
priority: 3
depends_on: []
paths:
  - Makefile
  - AGENTS.md
  - ".opencode/**"
  - "specs/06-distribution-and-docs.md"
  - "docs/**"
  - README.md
  - CHANGELOG.md
  - "packaging/**"
  - "scripts/**"
  - ".github/workflows/**"
  - "crates/slipfork/tests/critical_*.rs"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-22T19:17"
updated: "2026-11-22T19:49"
---

# Goal

Migrate 100% of Slipfork's Linux & `slipfork-lsp` package distribution onto the organization's **`offworld-packaging`** pipeline (load skill `offworld-packaging`!), purging all Cloudsmith / Forgejo / secret-based publishing code, and align the two `H-013` protected-test assertions so `make live-test` in GitHub Actions passes 100%:

1. **Migrate Packaging & Workflows to `offworld-packaging` (LOAD SKILL `offworld-packaging`!):**
   - Copy `/home/remy/offworld-nexus/.opencode/skills/offworld-packaging/scripts/stage_dist.py` **verbatim** to `packaging/stage-dist.py` (`chmod +x`; also add read-only `external_directory` allow + `edit` deny for `~/offworld-nexus/.opencode/skills/offworld-packaging/*` in `.opencode/opencode.jsonc`).
   - **Scorched-earth delete all Cloudsmith & Forgejo code/references:** Delete `scripts/publish_packages.py` and `scripts/tests/` (Cloudsmith/Forgejo unit tests), and remove every mention of `CLOUDSMITH_*`, `FORGEJO_*`, `cloudsmith`, or `forge.offworld.nexus` across `.github/workflows/`, `Makefile`, `scripts/`, `packaging/`, `docs/`, `specs/06-distribution-and-docs.md`, and `AGENTS.md`.
   - **Zero secrets in GitHub Actions (`permissions: contents: read` everywhere):**
     - Consolidate/align `.github/workflows/packages.yml` (and either unify `nightly` schedule + `stable` `v*` tags + `workflow_dispatch` rehearsal + `pull_request` dry-run into `.github/workflows/packages.yml` deleting redundant `.github/workflows/nightly.yml`, or keep `nightly.yml` 100% on `stage-dist.py` — a single unified `packages.yml` matching `offworld-packaging` §2–§4 with staggered cron `'37 3 * * *'`, `push: tags: ['v*']` annotated-tag check, `workflow_dispatch` with `rehearsal: boolean`, and `pull_request: branches: [develop]` dry-run is cleanest!).
     - Enforce exact per-ecosystem nightly version stamps (`offworld-packaging` §3) in `scripts/build-{deb,rpm,apk,arch,standalone}.sh` & `packages.yml`:
       - Debian `.deb`: `<base>~nightly<YYYYMMDD>.r<run>+g<sha7>` (`deb:amd64:...`, `deb:arm64:...`)
       - RPM `.rpm`: `Version: <base>`, `Release: 0.nightly.<YYYYMMDD>.r<run>.g<sha7>` (`rpm:x86_64:...`, `rpm:aarch64:...`)
       - Alpine `.apk`: `pkgver=<base>_alpha<YYYYMMDD><run>`, `pkgrel=0` (NO `~` or `+` in `.apk` `pkgver`!) (`apk:x86_64:...`, `apk:aarch64:...`)
       - Arch `.pkg.tar.zst`: `pkgver=<base>r<run>.g<sha7>` in `nightly`, `<base>` in `stable` (`arch:x86_64:...`, `arch:aarch64:...`)
       - Standalone Linux tarballs: `tarball:x86_64:...`, `tarball:aarch64:...`
       - Multi-platform `slipfork-lsp` archives: `macos:arm64:...`, `macos:x86_64:...`, `windows:x86_64:...`, `windows:arm64:...`, plus `--github-release`, `--homebrew-json` (for `slipfork-lsp` formula on macOS), and `--scoop-json` (for `slipfork-lsp` on Windows) if validated cleanly by `packaging/stage-dist.py`!
     - Pre-publish `verify` matrix (`debian:12`, `debian:trixie`, `ubuntu:22.04`, `ubuntu:24.04`, `rockylinux:9`, `fedora:41`, `archlinux:latest`, `menci/archlinuxarm:latest`, `alpine:3.21` across `amd64` + `arm64`) MUST run BEFORE `stage` (`stage` `needs: [version, packages, verify, lsp]`).
     - Run `uv run --no-project packaging/stage-dist.py --dry-run ...` on **every** run (including `pull_request` and `rehearsal == true`), and gate `actions/upload-artifact@v4` (`name: offworld-dist`, `retention-days: 7`) strictly on `should_publish == 'true'` (`event != 'pull_request'` and `rehearsal != 'true'`)!
   - **Public URLs (`*.offworld-nexus.app` ONLY — NEVER internal Chulak hostnames!):**
     - Update `docs/install.md`, `docs/install.sh`, `README.md`, and `specs/06-distribution-and-docs.md` to use `apt.offworld-nexus.app`, `rpm.offworld-nexus.app`, `apk.offworld-nexus.app`, `arch.offworld-nexus.app`, and `dl.offworld-nexus.app` with the exact canonical snippets from `offworld-packaging` §5 (`Types: deb` — omit `deb-src` since we don't ship `.dsc` source packages — `Suites: any-distro`, `Components: main` / `nightly`, `Signed-By: /etc/apt/keyrings/offworld-nexus.gpg`; `rpm.offworld-nexus.app/{stable,nightly}/$basearch/`; `apk.offworld-nexus.app/{stable,nightly}`; `arch.offworld-nexus.app/{stable,nightly}/$arch` with key `E9DD32C63321505EBBCF86386E08120B3306C720`; `dl.offworld-nexus.app/tarballs/{stable,nightly}/...`).

2. **Align `H-013` Protected Live-Test Assertions (`crates/slipfork/tests/critical_world_journey.rs` & `critical_fork_live.rs`):**
   - Update `critical_world_journey.rs`'s failed-service assertion to match the shipped `M3.50.4` crisp remedy contract (`✗ Failed to ... — check logs with: slipfork logs ...`).
   - Update `critical_fork_live.rs`'s mismatched-epoch test so it modifies a real `Dockerfile` input (or accepts `cached` when the step digest already exists in `.slipfork-store/images`) and verify `make live-test` passes 8/8 suites cleanly!

# Acceptance

- Zero occurrences of `cloudsmith`, `CLOUDSMITH`, `FORGEJO`, `forge.offworld.nexus`, or `chulak` anywhere in tracked files (`git grep -iE 'cloudsmith|forgejo|chulak'` returns empty!).
- `packaging/stage-dist.py` matches `/home/remy/offworld-nexus/.opencode/skills/offworld-packaging/scripts/stage_dist.py` byte-for-byte (`cmp -s`).
- Both `stable` and `nightly` dry-runs of `uv run --no-project packaging/stage-dist.py --dry-run ...` against real built `dist/packages/*` artifacts exit `0`!
- `make check`, `make docs-check`, `make packages`, `make packages-test`, `plan.py validate`, and `guard.py` all exit `0`, and all 8 live suites pass on `165.232.86.174`.
