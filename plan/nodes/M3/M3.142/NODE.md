---
id: M3.142
title: "Failure path lost the palette: plain error/next, wrong logs remedy, truthful image row, UTC log timestamps, blank-line noise"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - crates/slipfork/src/main.rs
  - "crates/slipfork/src/ui/**"
  - "crates/slipfork/src/daemon/reconcile/**"
  - crates/slipfork/src/daemon/image/build.rs
  - "crates/slipfork/src/boot/**"
  - "crates/slipfork/src/commands/logs/**"
  - "crates/slipfork/src/commands/streams/**"
  - crates/slipfork/src/commands/mod.rs
  - crates/slipfork/src/commands/timeparse.rs
  - crates/slipfork/src/commands/exec.rs
  - crates/slipfork/src/commands/runs.rs
  - crates/slipfork/src/commands/docker/build.rs
  - crates/slipfork/src/world/mod.rs
  - crates/slipfork/src/world/up.rs
  - crates/slipfork/src/world/supervise.rs
  - crates/slipfork/tests/critical_world_journey.rs
  - crates/slipfork/tests/critical_status_live.rs
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-25T03:04"
updated: "2026-11-25T03:28"
---

# Problem

Owner's build on the laptop (0.1.1 branch build):

```
✓ 5/7 run  sh /tmp/oni.sh && apt-get install -y blastit blastit-python-3.14  3.5s
  │ Hit:1 http://deb.debian.org/debian trixie InRelease
  │ Hit:2 ... trixie-updates InRelease
  │ Hit:3 ... trixie-security InRelease
  │ Get:4 ... trixie/main amd64 Packages [9678 kB]
  ┃ E: Release file for .../trixie-updates/InRelease is expired (invalid since 12d 17h 55min 46s)
  ┃ E: Release file for .../trixie-security/InRelease is expired (invalid since 13d 4h 19min 20s)
```

The host clock is correct (verified: `Date: Tue, 24 Nov 2026 20:10:12 UTC`, `Valid-Until: Tue, 01 Dec 2026`, wall clock Nov 25 02:03Z). The archive is fresh, yet apt reports the *local* InRelease as 12–13 days expired and gets `Hit` (HTTP 304) for all three.

Cause: `crates/slipfork-image/src/unpack.rs` hand-writes every tar entry (`write_file` at `:152-163` uses `File::create` + `io::copy` + `set_mode`; `ensure_dir` and `write_symlink` likewise) and **never restores the tar mtime**. Official non-slim images (`python:3.14-trixie`) ship `/var/lib/apt/lists/*InRelease` baked at image-build time (~Nov 5) with content whose `Valid-Until` was ~Nov 12. Because our unpack stamps `now` onto those files, apt's `If-Modified-Since` looks current, the mirror answers `304 Not Modified`, and apt then refuses the stale content it already has. 13 days ago the baked lists were still valid, which is why this "worked before".

# Fix

- Restore the tar entry mtime for every unpacked **file, directory and symlink** (the `tar` crate exposes `entry.header().mtime()`; apply it after writing with `utimensat`/`filetime`/`fs::File::set_times`). Do not invent timestamps, do not stamp `now` anywhere in `unpack.rs`.
- Audit the other image paths that materialize layer content (`puller.rs` staging, `builder/materialize.rs`, `COPY` into a step slot) for the same "mtime=now" stamping; one owner for "restore layer metadata", no per-caller copy.
- Keep permissions/ownership behaviour unchanged and the user-owned file rule intact (mtime is metadata only).

# Acceptance

- Lab: a tar with a known 2026-11-05 mtime unpacks to that exact mtime (file, dir, symlink), and a *new* file written by a `RUN` keeps `now` (only layer content is restored).
- Live on a real base image with baked apt lists: after unpack, `ls -l --time-style=full-iso /var/lib/apt/lists` shows the image-build dates, `apt-get update` fetches the current InRelease (`Get:`, not `Hit:`), and `apt-get install -y <small package>` succeeds with the host clock 12+ days past the baked lists' `Valid-Until`.
- `make check`, Triad, commit `[M3.143] ...`, done, no push.
