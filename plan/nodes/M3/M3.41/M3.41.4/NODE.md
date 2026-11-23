---
id: M3.41.4
title: "GitHub Actions: CI, docs deploy, distro package matrix, nightly"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.41.2, M3.41.3]
paths: [".github/**"]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-21T16:35"
updated: "2026-11-21T17:26"
---

# Goal

Add the four GitHub Actions workflows: loop-backed Btrfs CI, strict docs deploy, multi-distro package matrix, and nightly publishing with retention.

# Context

- No `.github/` exists. Mirror the shape of `/home/remy/offworld-nexus/blastit/.worktrees/release-0.2.0/.github/workflows/{ci,docs,packages,nightly}.yml`.
- Depends on M3.41.2 (all 9 docs pages in nav, so `make docs-check` is meaningful) and M3.41.3 (real `make packages` / `make packages-test` and `scripts/publish_packages.py --dry-run`).
- `Makefile:92` `live-test` is the Linux loop-Btrfs E2E entry; CI must create a loopback Btrfs file and export the device to the job, else skip with an explicit notice — never fake it. `js-test` (`Makefile:35`) and `packages-check` (`Makefile:44`) cover `@slipfork/opencode`.
- `editors/zed/` has `check.sh` for the extension build; run it as its own job. Docs leg must also assert `docs/install.sh` version consistency with `Cargo.toml`.
- Publishing legs call `scripts/publish_packages.py` (M3.41.3) with `--dry-run` on PRs and real uploads only on tags/nightly; tokens come from `env:` (`secrets.CLOUDSMITH_API_KEY`, `secrets.FORGEJO_TOKEN`, `GITHUB_TOKEN`), never inline.
- Use maintained actions only: `actions/checkout@v4`, `dtolnay/rust-toolchain@stable` (+ targets), `Swatinem/rust-cache@v2`, `actions/upload-artifact@v4`, `actions/setup-node@v4`, `oven-sh/setup-bun@v2`.
- No `actionlint` on the host; validate every file parses with `bun -e 'Bun.YAML.parse(...)'` and cross-check job/step shape against the blastit references.

# Acceptance

- `.github/workflows/ci.yml`: Btrfs loop E2E (`make live-test`), cross-OS `slipfork-lsp` + `forkfile` builds on linux/macos/windows, `@slipfork/opencode` `bun test` / `make js-test`, `editors/zed/check.sh`, `make docs-check`.
- `.github/workflows/docs.yml`: `uvx --offline zensical build --strict` + three Pages deploy actions, plus installer/documentation version consistency check.
- `.github/workflows/packages.yml`: native `amd64` + `arm64` build matrix; install verification inside `debian`, `ubuntu`, `fedora`, `rockylinux`, `archlinux`, `alpine` containers; six `slipfork-lsp` archives; Cloudsmith + Forgejo + GitHub Release publish that degrades safely under `--dry-run`.
- `.github/workflows/nightly.yml`: `schedule:` cron, nightly version stamp, publish, 7-nightly group retention prune.
- All four files parse under `bun -e 'Bun.YAML.parse(...)'`; zero hardcoded secrets; `make check` exits 0.
