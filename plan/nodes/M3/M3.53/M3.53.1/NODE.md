---
id: M3.53.1
title: "Unified offworld-packaging pipeline: verbatim stage-dist.py, zero-secret packages.yml, per-ecosystem nightly stamps"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths: [".github/workflows/** scripts/** packaging/** Makefile .opencode/**"]
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-22T19:20"
updated: "2026-11-22T19:31"
---

# Goal
Migrate Slipfork packaging 100% to the org `offworld-packaging` standard: byte-identical `stage-dist.py`, ONE zero-secret `.github/workflows/packages.yml` (stable tags + nightly cron + rehearsal + PR dry-run), exact per-ecosystem nightly stamps, and scorched-earth deletion of every Cloudsmith/Forgejo publishing path. **LOAD SKILL `offworld-packaging` FIRST** (`skill({id:"offworld-packaging"})`) and copy `scripts/stage_dist.py` from that skill directory verbatim.

# Context
Anchors verified 2026-11-22 on `release/0.1.0`:
- `.github/workflows/packages.yml` (15,301 B; jobs plan/packages/install/lsp/publish; secret refs `:310-311`, `:326`, `:339-340`, `:355` shell out to `scripts/publish_packages.py`; header comment `:8` claims Cloudsmith primary). Rewrite as the single packaging workflow.
- `.github/workflows/nightly.yml` (9,527 B; `CLOUDSMITH_API_KEY`/`FORGEJO_TOKEN` at `:174-175`, `:229`; publishes via `publish_packages.py` at `:190`). DELETE the file.
- DELETE `scripts/publish_packages.py` (12,663 B), `scripts/tests/test_publish_packages.py`, `scripts/tests/` and `scripts/__pycache__/`.
- `Makefile:184` `packages:` (all formats via `scripts/build-*.sh`), `Makefile:200` `packages-test:` line `$(UV) run --no-project python -m unittest discover -s scripts/tests` (remove it; keep `bash -n`/`dash -n`, `check-bundle-layout.sh`, `check-install.sh`), `Makefile:143-144` VERSION/PACKAGES_OUT, `:167` `packages-probe`.
- `.github/workflows/ci.yml:51-55` job "Shell syntax and publisher unit tests" runs the same unittest discover — retitle and keep only `bash -n scripts/*.sh docs/install.sh`.
- `scripts/check-install.sh:41-42` passes `--mirror cloudsmith` — drop the flag (M3.53.2 removes the flag from `docs/install.sh`; no unused optional flag may linger).
- Nightly stamp gaps: `scripts/build-deb.sh:45` rejects `*nightly*`; `scripts/build-apk.sh:73` hardcodes `pkgver=$version`; `scripts/build-arch.sh:75` writes `pkgver = $version-$release`; `scripts/build-rpm.sh:59,95` pass `--version $version --release $release` with no nightly branch; `scripts/build-standalone.sh` takes no `--version` (add one for nightly tarball naming).
- No `packaging/stage-dist.py` today; `packaging/` holds deb/rpm/arch/alpine recipes + systemd/apparmor/man.
- `.opencode/opencode.jsonc`: add read-only `external_directory` allow and an `edit` deny for `~/offworld-nexus/.opencode/skills/offworld-packaging/*`.

Stamp grammar (`offworld-packaging` §3): deb `<base>~nightly<YYYYMMDD>.r<run>+g<sha7>`; rpm `Version: <base>` + `Release: 0.nightly.<YYYYMMDD>.r<run>.g<sha7>`; apk `pkgver=<base>_alpha<YYYYMMDD><run>` (NO `~`/`+`) + `pkgrel=0`; arch nightly `pkgver=<base>r<run>.g<sha7>`, stable `<base>`.

Workflow contract: `permissions: contents: read` (no `secrets.*`); cron `'37 3 * * *'` on `develop`; `push: tags: ['v*']` with annotated-tag + `Cargo.toml` version match gate; `workflow_dispatch` input `rehearsal` boolean default `true`; `pull_request: branches: [develop]` dry-run; `version` job emits `channel`, `version`, `should_publish`; `verify` matrix `debian:12, debian:trixie, ubuntu:22.04, ubuntu:24.04, rockylinux:9, fedora:41, archlinux:latest, menci/archlinuxarm:latest, alpine:3.21` on `amd64` + `arm64` (native `ubuntu-24.04-arm`, never QEMU) installing and smoke-testing each artifact before `stage`; `stage` `needs: [version, packages, verify, lsp]` runs `uv run --no-project packaging/stage-dist.py --dry-run` on EVERY run and uploads `offworld-dist` (`actions/upload-artifact@v4`, `retention-days: 7`) only when `should_publish == 'true'`; `slipfork-lsp` staged with `--github-release`, `--homebrew-json` (macOS formula) and `--scoop-json` (Windows).

# Acceptance
- `cmp -s packaging/stage-dist.py /home/remy/offworld-nexus/.opencode/skills/offworld-packaging/scripts/stage_dist.py && test -x packaging/stage-dist.py`
- Stable dry-run exits 0: `uv run --no-project packaging/stage-dist.py --dry-run --source slipfork --channel stable --version 0.1.0 --out /var/tmp/od-stable --file 'deb:amd64:dist/packages/*_amd64.deb' --file 'rpm:x86_64:dist/packages/*.x86_64.rpm' --file 'apk:arm64:dist/packages/aarch64/*.apk' --file 'arch:x86_64:dist/packages/*.pkg.tar.zst' --file 'tarball:x86_64:dist/packages/*-linux-gnu.tar.gz'` (build once via `make packages`, or reuse existing `dist/packages/`).
- Nightly dry-run with `--channel nightly --version '0.1.0~nightly20261122.r42+g90cb2db'` also exits 0.
- `git grep -inE 'cloudsmith|forgejo|forge\.offworld\.nexus' -- . ':!plan'` is empty.
- `bash -n scripts/*.sh && make packages-test` exit 0.
- Every workflow yields `permissions: contents: read` and zero `secrets.` references beyond `GITHUB_TOKEN`.
- `uv run --no-project tools/plan.py validate && uv run --no-project tools/guard.py` exit 0.
