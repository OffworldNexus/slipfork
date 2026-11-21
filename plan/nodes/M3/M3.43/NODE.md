---
id: M3.43
title: "Overhaul specs/ & ARCHITECTURE.md: clap + frozen-pane indicatif TUI, explicit s3 policy + native Btrfs S3, speed-of-light PG/Redis & reactive <500ms fork"
kind: task
status: ready
owner: architect
priority: 3
depends_on: []
paths: ["specs/**", TESTING_PLAN.md, ARCHITECTURE.md, .opencode/LESSONS.md]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-21T19:40"
updated: "2026-11-21T19:40"
---

# Goal

Lock in the exact UX and technical architecture across `specs/01-forkfile.md`, `specs/02-cli-ux.md`, `specs/03-under-the-hood.md`, `specs/04-testing-plan.md`, `TESTING_PLAN.md`, and `ARCHITECTURE.md` for the four owner-mandated overhauls:

1. **Explicit `s3` Access Policy (`policy private | public-read`) & Strict RFC 1123 DNS Labels (`specs/01-forkfile.md`):**
   - `s3 <bucket>` defaults to `policy private` (anonymous HTTP `GET`/`HEAD` returns `403`; SigV4 credentials required).
   - `s3 <bucket> { policy public-read }` allows anonymous `GET`/`HEAD` (`200`), while `PUT`/`DELETE` still require SigV4 credentials.
   - Declaring `cdn <id>` inside `s3 <bucket> { ... }` **statically requires `policy public-read`**! Omitting `policy public-read` (or pairing `policy private` with `cdn <id>`) is a static `Forkfile:line:col` check error (`next: add 'policy public-read' inside 's3 <bucket>' to enable 'cdn <id>'`).
   - Hyphenated identifiers (`static-cdn`, `static-cdn.domain`, `static-cdn.url`) are valid everywhere; non-RFC-1123 labels (underscores `_`, uppercase, leading/trailing `-`, `> 63` chars) are statically rejected on `domain <id>`, `cdn <id>`, `s3 <bucket>`, and `route ... domain <prefix>.*`, and `_` -> `-` is normalized on repo/branch `.localhost` slugs (purge `static_cdn` everywhere in favor of `static-cdn`).

2. **Delete SeaweedFS Completely -> Native Btrfs-Backed Rust S3 Server (`< 1ms` Boot, Zero Extra Containers, `specs/03-under-the-hood.md`):**
   - Zero `chrislusf/seaweedfs` container pulls or distributed master/raft/volume/filer daemons.
   - `slipfork-world-init` runs one lightweight native Rust S3 HTTP service inside the world's `127.0.0.1` netns (`:8333`) backed directly by `<world>/s3/<bucket>/` on the world's Btrfs data subvolume, supporting path-style & virtual-host-style `PUT`, `GET`, `HEAD`, `DELETE`, `ListObjectsV2` XML, AWS SigV4 HMAC-SHA256 verification, `Content-Type`/`ETag`/`Content-Length` sidecar metadata, and per-bucket `policy private` (`403` anonymous `GET`) vs `policy public-read` (`200` anonymous `GET`, `403` anonymous `PUT`/`DELETE`).

3. **Speed-of-Light Dev/Test Databases, True Reactive `tokio` Boot DAG & Genuine Sub-Second End-to-End `slipfork fork` (`< 500ms` Target, `< 1.0s` Hard Ceiling):**
   - **PostgreSQL 18 (`~30–50ms` start, `0ms` `initdb`):** Cache a golden `initdb` Btrfs subvolume once in `<btrfs_mount>/.slipfork-store/templates/postgres-18` (sealed `BTRFS_SUBVOL_RDONLY`); clone `PGDATA` via `< 1ms` Btrfs snapshot, remove stale `postmaster.pid`, run `postgres` with `-c fsync=off -c synchronous_commit=off -c full_page_writes=off -c wal_level=minimal -c max_wal_senders=0 -c autovacuum=off -c shared_buffers=32MB -c dynamic_shared_memory_type=posix`, and poll readiness every `2ms`.
   - **Redis 7 (`~5–10ms` start, live keys preserved on `fork`):** Run `redis-server` with `--save "" --appendonly yes --appendfsync no` + `2ms` readiness poll.
   - **True Reactive Parallel DAG (`tokio` task graph, ZERO phase barriers):** Every node (`Image`, `Resource`, `Setup`, `PreDeploy`, `Service`, `PostDeploy`) awaits strictly its own upstream parent edges and fires immediately on completion, recording **per-node duration** (`node_start.elapsed()`), never wall-clock since `t0`.
   - **Zero Image Rebuilds & Zero Redundant `pre-deploy` on `slipfork fork`:** Persist resolved image subvolumes and completed `pre-deploy` hook input stamps in `<world>/.slipfork-world-stamp.json` inside the world's Btrfs subvolume so `slipfork fork` reuses parent image subvolumes directly (`0ms`, zero `Dockerfile` rebuilds or context walks) and skips unchanged `@<tool>`, `before-start`, and `pre-deploy` hooks (`0ms`).

4. **`clap` CLI + Frozen-Bottom-Pane Live TUI (`indicatif::MultiProgress` + `console` + `owo-colors` + `crossterm`, `specs/02-cli-ux.md`):**
   - Replace 100% of hand-rolled argv/help/completions/manpage/ANSI code with `clap` (`derive`, `clap_complete`, `clap_mangen`) + `indicatif::MultiProgress` / `console` / `owo-colors`.
   - **Interactive TTY layout during `up`, `restart`, `fork`:**
     - **Frozen pane at the bottom:** active Braille spinners for each in-flight parallel node (with live elapsed timer + dimmed 1–3 line rolling tail of live `Dockerfile` `RUN` / hook stdout/stderr so builds/hooks are never opaque!) above an anchored bottom progress bar (`[██████████░░░░░░] 6/10  Booting demo  0.3s`).
     - **Scrolling event stream popping above the frozen pane (`MultiProgress::println`):** consistent, aligned visual hierarchy (`✓` / dim `•`, dimmed fixed-width kind `image` / `resource` / `setup` / `hook` / `service`, bold name, clear state `cached` / `pulled` / `built` / `ready` / `up to date` / `listening :8000 -> /api`, and dimmed right-aligned per-node duration `18ms` — zero inconsistent casing, zero stray blank lines).
     - Clean fallback (hidden draw target, zero `\x1b[` / `\r`) under `NO_COLOR`, `TERM=dumb`, pipes, or `--json`.
   - **Default `slipfork logs [-f]`:** Application `service`s only by default; `-a` / `--all` or explicit `<resource>` includes backing resources.

# Acceptance

- `specs/01..04`, `TESTING_PLAN.md`, and `ARCHITECTURE.md` are crisp, consistent, free of `static_cdn` or `SeaweedFS`, and pass `uv run --no-project tools/guard.py`.
