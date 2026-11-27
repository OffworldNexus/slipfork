---
id: M3.200
title: "Restore the owner's index.md voice; fill the placeholders instead of rewriting the prose"
kind: task
status: done
owner: "doc-writer"
priority: 3
depends_on: []
paths: [docs/index.md]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-26T07:40"
updated: "2026-11-26T07:41"
---

# Problem

The M3.192 rewrite replaced the owner's own prose with drafted copy. Owner: "Essentially bring back the pitches and sentences that I had written your crap has LLM touch all over it. I expected you to fill up things not water down my stuff."

Their draft is preserved verbatim at `/tmp/opencode/owner-index-draft.md` (recovered from their uncommitted edit). Rebuild `docs/index.md` on **their** text.

# Rules

- Keep their sentences **verbatim** — the "Docker Compose with opinions, without Docker and on crack" line, "cloud in box", the three bullets (UID mapping / `.localhost` / bootstrapped infrastructure), "The target user runs this on a cloud machine, alongside a coding agent". Do not paraphrase, do not add new marketing copy.
- Fix only what is factually wrong or a typo: `bootstraped` → `bootstrapped`; `<link to how to mount Btrfs loopback device>` → `[mount a Btrfs workspace image](install.md#on-ext4-or-xfs-mount-a-btrfs-workspace-image-in-your-home-folder)`; `0.1.0 … very experimental release with breaking changes ahead` → the agreed `**Pre-1.0.** The Forkfile and CLI can change between releases; report what breaks in the [issue tracker](…).`; fix the install code block to `$ curl -fsSL https://offworldnexus.github.io/slipfork/install.sh | sh`.
- **Fill the placeholders, do not delete the sections**:
  - `# add in there a database` → real `postgres pg { database app_db }`;
  - `# and of course add there the database url in the env` → `env common { DATABASE_URL = app_db.url }` plus `use env common` in the image;
  - drop the `features { dev }` block (dev is built in — a no-op) but keep the rest of their Forkfile shape, including `arg port = web.port`;
  - `granian --reload blah blah blah` → the real dev command (`@uv run granian --reload --interface asgi app:app --host 0.0.0.0 --port $port` or uvicorn — pick one the tutorial does not contradict);
  - `(insert here the other files …)` → the actual minimal project: `uv init`, `uv add`, `app.py`, and the `Dockerfile` with the `dev` stage and `WORKDIR` (dev refuses `/`);
  - `(and then show how to run it …)` → `slipfork up`, the resulting `http://get-started.localhost`, and one line that the project directory name becomes the world name.
- End with one short line pointing to the [tutorial](tutorial.md) for adding a database, a SvelteKit front and forking — do not duplicate the tutorial here.
- Keep the mascot image and both admonitions (platform support + pre-1.0).

# Acceptance

- Every sentence from the draft that still applies survives verbatim; only placeholders/facts/typos changed.
- Home page boots step 1 exactly as the tutorial does (Dockerfile `dev` stage + `WORKDIR`, `on dev: stage dev`, `$port`), and `make docs-check` is green.
- Commit `[M3.200] ...` with `docs/index.md` only, done, no push.
