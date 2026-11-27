---
id: M3.201
title: "Tutorial rewrite for humans: code in blocks, breathe, unpack every Forkfile key"
kind: task
status: done
owner: "doc-writer"
priority: 3
depends_on: []
paths: [docs/tutorial.md]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-26T07:47"
updated: "2026-11-26T07:50"
---

# Problem

The tutorial reads like tool output: code mashed into sentences, dense snippets, and almost no explanation of the Forkfile syntax even though each step introduces new keys. Owner: "You love to mash code in paragraphs instead of readable blocks. And your code is so fucking dense. It's not tool calls, it's intended at humans. And you're explaining very few things about the forkfile syntax while there is actually a lot to unpack."

# Rewrite rules (human-first)

- **Code belongs in blocks.** Never chain more than two identifiers in a sentence; anything the reader would type or paste is its own fenced block (console, dockerfile, forkfile, python).
- **Snippets breathe.** Blank lines between sections, one concept per line, and short `#` comments that name the part (`# image id + build context`). No line longer than ~80 chars; no several flags glued on one giant command.
- **Every new block is unpacked.** After each Forkfile/Dockerfile block add a short "What this says" list — one bullet per *new* key introduced in that step (`image <id> <dir>`, `service <id>`, `port`, `route`, `env`, `use env`, `on dev`, `stage`, `arg`, `run:`, `postgres … { database … }`, `$port`). Say what the key means, not just that it exists.
- **Dockerfile ↔ Forkfile, explicitly.** Each step ends with one short paragraph on how the two files cooperate for that step: which stage is picked, where `WORKDIR` puts the live checkout, what `$port` becomes, what `env` reaches at runtime but never at build.
- **Prose is for humans:** ≤3 sentences per paragraph, second person, no marketing, no tool-call tone.
- **Lean:** only explain the key that step introduces; don't repeat earlier bullets. Aim ≤250 lines total.

# Suggested shape (keep four steps)

1. **A Python API** — `uv init`, `app.py`, `Dockerfile` (dev stage, `WORKDIR`, `ARG port`), minimal `Forkfile` (`image`/`service`/`port`/`route`/`on dev` stage/`run:`/`$port`), `slipfork up` → `http://<dir>.localhost`. Unpack both files; explain the dev mount.
2. **A database** — three new lines (`postgres`, `env`, `use env`) shown as one small block, then the updated image block. Unpack `.url`, runtime-vs-build, and why the Dockerfile does not change. A small Python block using `DATABASE_URL`, then `slipfork up` + `curl`.
3. **A SvelteKit front** — its Dockerfile block, its service block, and the API's `route /` → `route /api` change. Unpack `route` prefix routing, longest prefix, one origin (no CORS/proxy), and the two `WORKDIR`s.
4. **Fork the world** — `fork`, `status`, `drop` each as their own block, with the URL shape and what is inherited (code, deps, data) and what stays isolated.

# Accuracy

- Only shipped syntax (`specs/01`, `docs/forkfile.md`); only shipped verbs (`docs/cli.md`); no `slipfork start`, no `auth`.
- `dev` needs a Dockerfile `WORKDIR` (refuses `/`); `on dev: stage dev` requires that stage.
- Service ids are globally unique; ports are declared unconditionally.
- `make docs-check` green.

# Acceptance

- No paragraph contains more than two inline identifiers; every command/config is a block.
- Each step has a "What this says" list covering its new keys, and a Dockerfile↔Forkfile paragraph.
- A reader can paste each step in order and it works; `make docs-check` green.
- Commit `[M3.201] ...` with `docs/tutorial.md` only, done, no push.
