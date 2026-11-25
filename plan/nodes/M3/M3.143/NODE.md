---
id: M3.143
title: "Unpacked layers lose their mtimes: baked apt lists look freshly written, mirrors answer 304, apt dies on Valid-Until"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - "crates/slipfork-image/src/unpack.rs"
  - "crates/slipfork-image/src/metadata.rs"
  - "crates/slipfork-image/src/copy.rs"
  - "crates/slipfork-image/src/lib.rs"
  - "crates/slipfork-image/Cargo.toml"
  - "crates/slipfork-image/tests/image_core_lab.rs"
  - "crates/slipfork-image/tests/critical_dockerfile_live.rs"
  - "crates/slipfork-image/tests/support/**"
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-25T03:04"
updated: "2026-11-25T03:27"
---

# Symptom (owner, real host)

`apt-get update` inside a `RUN` fails on a *fresh* Debian image with `Release file … is not valid yet` /
`no longer valid` `Valid-Until`, and the log shows the mirror answering `Hit:`/`304 Not Modified` for
`InRelease` instead of `Get:`. Host clock is correct and the Debian archive is fresh: apt is rejecting
the **local** `InRelease` that is 12–13 days expired. So the local copy is stale *and* the mirror thinks
we already have the current one.

# Diagnosis

`crates/slipfork-image/src/unpack.rs:152-164` hand-writes every tar entry
(`File::create` + `io::copy` + `set_mode`) and never restores the header mtime, so every file in a pulled
layer gets `mtime = now`. Directories (`:116-119`, stamped by `set_mode` in the deferred loop at
`:93-95`) and symlinks (`:166-171`) are stamped `now` too.

Apt's conditional GET sends `If-Modified-Since: <local InRelease mtime>`. Because the baked list looks
written *just now* (newer than the mirror's copy), the mirror answers `304 Not Modified`, apt keeps the
bytes it has, and then the freshness check rejects the image-build-era `Valid-Until`. Restoring header
mtimes makes the lists carry their **image build date**, so apt re-fetches and installs.

# Job

1. **One metadata owner.** Add `crates/slipfork-image/src/metadata.rs` (registered in `lib.rs`) owning
   "apply mode" and "apply mtime" for materialized entries: `set_mode(path, mode)` (the current
   `unpack.rs:344-347` body) and an mtime apply that uses `utimensat` with `AT_SYMLINK_NOFOLLOW` for
   symlinks and `UTIME_OMIT` for atime (add `libc.workspace = true` to `crates/slipfork-image/Cargo.toml`
   dependencies; it is already a workspace dependency). No per-caller copies of the syscall.
2. **`unpack.rs` restores every entry's mtime** from `entry.header().mtime()`: regular files after the
   copy, symlinks without following, and directories **deferred with their mode** to the existing
   post-children loop (`:93-95`) so a child write cannot re-stamp the parent. Hardlink entries inherit
   the target inode's mtime — leave them alone and say why in a comment.
3. **Audit the other materializers, no duplicated logic.**
   - `puller.rs:430-459` stages through `LayerUnpacker` only — fixed for free, confirm.
   - `copy.rs:285-320` (`COPY` file/symlink) uses `fs::copy`, which stamps `now`: restore the *source*
     entry's mtime on the destination (Docker `COPY` preserves source mtime), via the same owner.
     Directories go through the existing deferred loop (`:103-105`, `:165-167`): pair each recorded
     directory with its source so its mtime is restored after its children, same as `unpack`.
   - `copy.rs:196-215` `ADD` archive extraction already honors the `tar` crate's default
     `preserve_mtime = true`; verify, do not reimplement.
   - `builder/materialize.rs:125-128` (`WORKDIR`) and anything a `RUN` writes keep `now` — that is
     correct, assert it instead of changing it.

# Acceptance

- Lab test (`crates/slipfork-image/tests/image_core_lab.rs`, real disk `tempfile`, no mocks): a
  hand-built layer with `mtime = 2026-11-05T…` for a file, a directory and a symlink unpacks with
  exactly those mtimes (symlink read with `symlink_metadata`), a later-layer file keeps its own header
  mtime, and a file written by a `RUN` afterward keeps `now` (assert `>=` the build start instant).
- Live on a real Debian image with baked lists: unpack shows `InRelease` carrying the image-build date
  (days old), `apt-get update` logs `Get:` (not `Hit:`) for the current `InRelease`, and a small
  `apt-get install` succeeds on a host clock 12+ days past the baked lists' `Valid-Until`. If the live
  host cannot reproduce the skew, fake it deterministically by choosing an image whose baked lists are
  already expired — that is exactly the owner's case.
- `make check`, Triad, commit `[M3.143] ...`, done, no push.
