---
id: M3.45
title: "Final TESTING_PLAN.md E2E verification, make coverage sweep & live <1s fork + frozen-pane PTY demo gate on 165.232.86.174"
kind: epic
status: done
priority: 3
depends_on: [M3.44]
paths:
  - Cargo.toml
  - Cargo.lock
  - Makefile
  - TESTING_PLAN.md
  - "specs/**"
  - "crates/**"
  - "editors/**"
  - "packages/**"
  - "packaging/**"
  - "scripts/**"
  - ".github/**"
  - "tools/**"
  - "docs/**"
  - README.md
  - CHANGELOG.md
  - zensical.toml
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-21T19:40"
updated: "2026-11-22T09:42"
---

# Goal

Run the final end-to-end verification across local gates and DigitalOcean Btrfs VM `165.232.86.174`:
1. **`make coverage` & Dead-Code Sweep:**
   - Run `make coverage` (`tools/coverage_report.py` -> `target/cov`), delete any leftover dead code from the old hand-rolled CLI/TUI/SeaweedFS paths, and verify all source files stay `<= 500` LOC and warning-free.
2. **Live Btrfs VM Gate & Sub-Second `fork` + Frozen-Pane PTY Proof (`165.232.86.174`):**
   - Build and install the fresh `.deb` package (`dpkg -i`) on `165.232.86.174` (`sudo slipfork daemon install --ingress` + `slipfork daemon install`).
   - Run all live Btrfs E2E suites on `165.232.86.174` (using isolated `SLIPFORK_TEST_ROOT` + isolated `XDG_RUNTIME_DIR`), including:
     - Hard wall-clock E2E assertion that end-to-end `slipfork fork <name>` (from process spawn to `http://<name>--<world>.localhost` answering `200` with Postgres + Redis + S3 + multi-service app) completes in **`< 1.0s`** (with zero image rebuilds and zero redundant `pre-deploy` re-runs!).
     - Real `PUT` + `GET` + `HEAD` + `DELETE` + `ListObjectsV2` verification on native Btrfs S3 across `policy private` (`403` anonymous `GET`, `200` SigV4 `GET`) and `policy public-read` + `cdn static-cdn` (`200` anonymous `GET` via `http://static-cdn.<world>.localhost`, `403` anonymous `PUT`).
     - Real PTY capture verifying `indicatif::MultiProgress` frozen-bottom-pane progress bar & Braille spinners on interactive TTY, plus `0` escape bytes under `NO_COLOR=1` and `TERM=dumb`.
   - Refresh `/home/slipfork/demo` (`s3 assets { policy public-read; cdn static-cdn }`) + `feat-auth` fork live on `:80` and verify `slipfork fork` speed & visual TUI directly in `/home/slipfork/demo` (cleaning up any temporary probe fork, leaving `demo` + `feat-auth` live and clean on `:80`).

# Acceptance

- `make check`, `make docs-check`, `make packages`, `make packages-test`, and `make coverage` all exit `0` locally.
- Live Btrfs E2E suites pass `100%` on `165.232.86.174` with measured end-to-end `slipfork fork` `< 1.0s`, native Btrfs S3 verified, and `/home/slipfork/demo` + `feat-auth` live on `:80`.
