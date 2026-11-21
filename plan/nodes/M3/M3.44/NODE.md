---
id: M3.44
title: "Implement clap + frozen-pane indicatif TUI, explicit s3 policy + native Btrfs S3, fast PG/Redis & reactive <500ms fork"
kind: epic
status: ready
priority: 3
depends_on: [M3.43]
paths:
  - Cargo.toml
  - Cargo.lock
  - Makefile
  - ARCHITECTURE.md
  - "specs/**"
  - TESTING_PLAN.md
  - "crates/**"
  - "editors/**"
  - "docs/**"
  - README.md
  - CHANGELOG.md
  - "packaging/**"
  - "scripts/**"
  - "tools/**"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-21T19:40"
updated: "2026-11-21T19:40"
---

# Goal

Implement the complete `M3.43` overhaul cleanly across the workspace using cached crates (`clap`, `clap_derive`, `clap_complete`, `clap_mangen`, `indicatif`, `console`, `crossterm`, `owo-colors`, `tokio`):
1. **`Forkfile` (`slipfork-forkfile`, `slipfork-lsp`, `editors/`, `docs/`):**
   - `s3 <bucket> { policy private | public-read; cdn <id> }` (default `private`; `cdn <id>` statically requires `policy public-read` with `Forkfile:line:col` caret error + `next:` remedy).
   - Hyphenated identifiers (`static-cdn`, `static-cdn.domain`, `static-cdn.url`) + strict RFC 1123 DNS/bucket validation rejecting underscores/uppercase/invalid labels on `domain <id>`, `cdn <id>`, `s3 <bucket>`, and `route ... domain <prefix>.*`, plus `_` -> `-` slug normalization on repo/branch `.localhost` hostnames (`static_cdn` purged repo-wide).
2. **Delete SeaweedFS -> Native Btrfs-Backed Rust S3 Server (`crates/slipfork-world`):**
   - Delete every reference to `chrislusf/seaweedfs` / `weed`.
   - Implement a cohesive, `< 1ms`-boot native Rust S3 HTTP service inside `slipfork-world` over `<world>/s3/<bucket>/` supporting path & virtual-host addressing, `PUT`/`GET`/`HEAD`/`DELETE`/`ListObjectsV2`, AWS SigV4 HMAC-SHA256 auth, object metadata sidecars, and `policy private` (`403` anonymous `GET`) vs `policy public-read` (`200` anonymous `GET`, `403` anonymous `PUT`/`DELETE`).
3. **Speed-of-Light PG/Redis, Reactive `tokio` Boot DAG & `< 500ms` End-to-End `slipfork fork` (`crates/slipfork-world`, `crates/slipfork`):**
   - Golden `initdb` Btrfs template subvolume in `<btrfs_mount>/.slipfork-store/templates/postgres-18` + `postgres` dev flags (`fsync=off`, `synchronous_commit=off`, `full_page_writes=off`, `wal_level=minimal`, `max_wal_senders=0`, `autovacuum=off`, `shared_buffers=32MB`, `dynamic_shared_memory_type=posix`) + `2ms` readiness poll.
   - `redis-server --save "" --appendonly yes --appendfsync no` + `2ms` readiness poll.
   - True reactive `tokio` DAG executor (every node spawns immediately when its own upstream edges finish, reporting per-node elapsed duration `node_start.elapsed()` and streaming live `Dockerfile` `RUN` / hook lines over daemon events).
   - Persist `<world>/.slipfork-world-stamp.json` (resolved image subvolumes + `pre-deploy` input stamps) so `slipfork fork` performs **zero** image rebuilds/pulls/walks and **zero** redundant `pre-deploy` runs, booting cloned pods concurrently in parallel (`< 500ms` target, `< 1.0s` hard ceiling end-to-end!).
4. **`clap` CLI + Frozen-Bottom-Pane `indicatif::MultiProgress` TUI (`crates/slipfork`, `docs/`, `packaging/`, `scripts/`):**
   - Delete all hand-rolled argv parsing, help formatting, completion strings, and manpage templates; wire `clap` (`derive`, `clap_complete` for `slipfork completions bash|zsh|fish`, and `clap_mangen` for `slipfork(1)` manpage in packaging).
   - Build the `indicatif::MultiProgress` + `console` + `owo-colors` frozen-bottom-pane TUI (anchored bottom progress bar + active Braille spinners per parallel node + dimmed rolling build/hook log tail, with aligned typographic completion events popping cleanly above via `mp.println`, and clean non-TTY/`NO_COLOR`/`TERM=dumb`/`--json` behavior).
   - Service-only default `slipfork logs [-f]` (`-a` / `--all` for resources), and sync `docs/` + `check_docs_honesty.py` with `clap --help`.

# Acceptance

- `make check`, `make docs-check`, `make packages`, and `make packages-test` all exit `0`.
- Zero `seaweedfs`/`weed`, zero `static_cdn`, and zero hand-rolled argv parser remain anywhere in the repo.
