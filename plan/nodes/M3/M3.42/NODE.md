---
id: M3.42
title: "Full TESTING_PLAN.md user-story E2E verification, make coverage dead-code sweep & live Btrfs proof on 165.232.86.174"
kind: epic
status: superseded
priority: 3
depends_on: [M3.41]
paths:
  - Cargo.toml
  - Cargo.lock
  - Makefile
  - ARCHITECTURE.md
  - TESTING_PLAN.md
  - "specs/**"
  - "crates/**"
  - "editors/**"
  - "packages/**"
  - "packaging/**"
  - "scripts/**"
  - ".github/**"
  - "support/**"
  - "tools/**"
  - "docs/**"
  - README.md
  - CHANGELOG.md
  - zensical.toml
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-21T00:44"
updated: "2026-11-21T19:40"
---

# Goal

Prove every single user story in `TESTING_PLAN.md` (§2–§7) end-to-end with zero mocks, run `make coverage` (`tools/coverage_report.py` -> `target/cov`) to prune dead/unreachable code and any non-§7 inline `#[cfg(test)]` unit tests, verify Linux package installation, and validate the complete system live on DigitalOcean Btrfs VM `165.232.86.174`:
1. **Consolidated User-Story E2E Suites (`TESTING_PLAN.md` §2–§7):**
   - Every test maps directly to `TESTING_PLAN.md`: enclosing `image` native `Dockerfile` multi-stage build + `arg` vs `env` + HTTPS OCI pull (`mailpit`), DO-aligned `route` (keep-prefix default, `strip-prefix`, `rewrite`, `exact`, `domain <id | prefix.*>`), `@<tool>` deduplicated `(image, lockfile_dir)` DAG setup + `before-start when changed(...)`, PostgreSQL 18 + Redis 7 + default-private `s3` (`403`) + `s3 { cdn <id> }` (`public-read` `.domain`/`.url`), single-binary `:80` ingress + single per-user daemon (zero root/user split-brain), transparent directory-to-Btrfs-subvolume conversion, `doctor`, `migrate`, colorful status tree & `ps`, `logs -f` across `restart`, `exec`/`shell`/`db`/`redis`/`open`, `< 50ms` Btrfs project+world `fork` & dirty-refusing `drop`, `@slipfork/opencode` parallel agent sessions, `OSPP v1` HPKE secrets + live rotation + encrypted dump seed + offline fallback, and §7 focused `Forkfile`/OSPP/LSP/Zed suites.
2. **Coverage-Driven Cleanup (`make coverage`):**
   - Run `make coverage` (`tools/coverage_report.py` writing to `target/cov`), delete 0%-covered fallback dead code and any non-§7 inline unit tests, and ensure every source file stays `<= 500` LOC and warning-free.
3. **Live Cloud Btrfs Proof & Demo (`165.232.86.174` via `tools/do_vm.py`):**
   - Clean any stale old `slipforkd` processes/units on `165.232.86.174`, install our freshly built `.deb` package (`dpkg -i`) + `sudo slipfork daemon install` (`:80` socket-activated proxy + AppArmor), run the full live Btrfs E2E suite on `165.232.86.174` (`0` failures, `0` leaked containers/subvolumes), and deploy `/home/slipfork/demo` (`# forkfile: 0.1` full-stack demo with `api`, `worker`, `front`, `mailpit`, `postgres 18`, `redis 7`, `s3` private & `cdn`, plus an active `feat-auth` fork) verified live over `:80` `.localhost` and real PTY (`COLORTERM=truecolor`, `NO_COLOR=1`, `TERM=dumb`).

# Acceptance

- `make check`, `make docs-check`, `make packages-test`, and `make coverage` all exit `0` locally.
- Live Btrfs E2E suite + `.deb` install + PTY/HTTP `:80` multi-service demo + `< 50ms` `fork` proof pass `100%` on `165.232.86.174`, leaving `/home/slipfork/demo` running cleanly on `:80`.
- Consolidated one rich E2E journey per user story, clean `-pgid`/snapshot reaping, and deletion of 0%-covered dead fallbacks.
- Protected `critical_ospp.rs`, `critical_world_journey.rs`, and `critical_btrfs_live.rs` remain green.
