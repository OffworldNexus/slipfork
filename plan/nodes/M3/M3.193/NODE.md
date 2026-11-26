---
id: M3.193
title: "Docs: tutorial-first rewrite, install page, index polish (owner-edited drafts)"
kind: task
status: superseded
owner: "doc-writer"
priority: 3
depends_on: []
paths: ["docs/**", zensical.toml, README.md]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-26T07:19"
updated: "2026-11-26T07:26"
---

# Goal

The owner rewrote parts of `docs/` in rough language and asked to: (1) pump the docs up, (2) **replace the getting-started page with a tutorial** that introduces features one by one — a simple Python server, then a database, then a SvelteKit front — showing how the `Dockerfile` and `Forkfile` are built and how they work together. Lean, runnable, no fluff.

# Slices (one writer, serial, verify each)

- **A. Install page** (`docs/install.md`, currently 6 lines): curl|sh quick start, distro packages, tarball, editor/LSP plugins, and a real **“On ext4 or XFS: mount a Btrfs workspace image in your home folder”** section whose heading slug is `on-ext4-or-xfs-mount-a-btrfs-workspace-image-in-your-home-folder` (three existing links point at it), `slipfork doctor`, uninstall.
- **B. Tutorial** (`docs/tutorial.md`, replacing `docs/getting-started.md`): four short steps, each with the *Dockerfile* and *Forkfile* side by side plus one command and the expected result: (1) Python API only, (2) add `postgres` + `env`, (3) add a SvelteKit front on the same world domain (`route /` vs `route /api`), (4) `fork` the whole world and inspect it. Keep each step ≤40 lines.
- **C. Index + nav + links**: rewrite `docs/index.md` from the owner's draft (remove `(insert here…)` placeholders and the commented-out hints, `0.1.1`, real install link, link to Tutorial and Install), `zensical.toml` nav (`Tutorial` instead of `Getting started`), and every `getting-started.md` reference (`docs/forkfile.md:279`, `docs/cli.md:326`, `README.md:63,125`).
- **D. Light polish** elsewhere only where prose is rough or stale; do not restructure working pages.

# Accuracy rules (verify against specs/01 + docs/cli.md + docs/forkfile.md, do not invent)

- Header `# forkfile: 0.1`; `features { dev }` is redundant — never show it.
- `on dev: stage dev` requires a `dev` stage in the Dockerfile; a Dockerfile for `dev` must set `WORKDIR` (dev refuses `/`).
- Service ids are globally unique (`service api`, `service front`); `port` + `route /api` / `route /`.
- Resources: `postgres pg { database app_db }`, `env common { DATABASE_URL = app_db.url }`, `use env common` inside the image.
- `arg PORT = web.port` is valid; the Dockerfile consumes it with `ARG PORT`.
- Only use CLI verbs documented in `docs/cli.md` (`up`, `down`, `fork`, `drop`, `ps`, `status`, `logs [-r] [-f] [--last N]`, `exec`, `shell`, `doctor`, `fmt`, `features`, `docker …`). No `slipfork start`.
- URLs are `http://<world>.localhost`; forks `http://<fork>--<world>.localhost`. Omit `:80`.

# Acceptance

- `make docs-check` green (Zensical strict — zero broken links/anchors — plus the docs-honesty gate), and `docs/` builds.
- No placeholders, no stale `0.1.0`, no dead links, no `getting-started.md` references.
- Every snippet is valid according to the shipped grammar; a reader can copy-paste step 1 and boot it.
- Commit `[M3.192] ...` with explicit `docs/**`, `zensical.toml`, `README.md` paths, done, no push.
