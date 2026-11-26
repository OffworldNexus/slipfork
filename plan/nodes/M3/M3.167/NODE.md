---
id: M3.167
title: "Cached from-only image fails with 'no recorded USER' (M3.153 quirk)"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths: ["crates/slipfork-image/src/**", crates/slipfork/src/world/runtime/helpers.rs]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-25T12:50"
updated: "2026-11-25T16:56"
---

# Problem

`M3.157` hit this on the VM: a real one-instruction Dockerfile build was needed to seed the store because HEAD's **from-only `cached`** image path errors `no recorded USER`. Pulled/`from` images (`image x from repo:tag`) must resolve their identity as `0:0` (Docker semantics) without a recorded `USER`.

# Fix

- The sealed-image record only carries `ImageConfig.user` when a Dockerfile actually set `USER`; absence means root (`0:0`), never an error.
- `Identity::resolve(rootfs, None)` already defaults to root — make sure the from/pulled path passes `None` rather than failing on a missing field, and that a *cached* from-only node (no build ran) still reports the image fact.
- Keep the fail-closed behaviour for an explicit `USER` that names a missing account.

# Acceptance

- Lab + live: `image pub from postgres:17` (or any pulled tag) boots from the second `up` (`cached`) with no USER error; `exec -- id` is root-in-userns; an image with `USER ghost` still fails closed with the `useradd` remedy.
- `make check`; Triad-light (`review-tests` only); commit `[M3.167] ...`, done, no push.
