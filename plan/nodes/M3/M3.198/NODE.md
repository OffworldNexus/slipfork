---
id: M3.198
title: "Docs voice: restore the 'Docker Compose on crack' edge on the home page"
kind: task
status: done
owner: "doc-writer"
priority: 3
depends_on: []
paths: [docs/index.md, docs/tutorial.md, zensical.toml]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-26T07:36"
updated: "2026-11-26T07:38"
---

# Problem

The tutorial-first rewrite sanded the voice down. The owner's point: "When I say that it's docker compose on crack I expected you to keep it." The home page now reads like a product page — correct but toothless.

# Restore the edge (behaviour claims stay honest)

Rewrite `docs/index.md` (and at most one intro line in `docs/tutorial.md` if it helps) with the owner's voice back:

- Open with the line they wrote: "Slipfork is like Docker Compose with opinions, without Docker and on crack." Keep the mascot and the two warnings.
- Then consequences, not feature soup. Keep/restore the punchy bullets with a concrete fact each, e.g.:
  - "No Docker daemon — every `RUN` is a cached Btrfs subvolume."
  - "No port collisions — each world owns `<world>.localhost`, each fork `<fork>--<world>.localhost`."
  - "No setup script — PostgreSQL, Redis and S3 are declared resources, booted with the world and cloned into every fork."
  - "Your agent can fork the whole running stack — code, dependencies and data — in milliseconds."
- Voice: short declarative sentences, contractions, second person, zero marketing adjectives ("seamless", "powerful", "effortless" banned). One opinion per sentence. The confidence comes from specifics, not adjectives.
- Keep it lean: ≤25 lines before the first snippet; ≤2-sentence paragraphs; no new pages.

# Do not

- Do not invent behaviour or numbers. No `0.1.0`, no `auth`, no `slipfork test`, no "<1.0s fork" claims unless measured. `docs/cli.md` is the verb list; `specs/01` the grammar.
- Do not turn the tutorial into a hype piece — at most its opening sentence may carry the voice.

# Acceptance

- The page leads with the owner's line and keeps three or four sharp consequence bullets.
- No banned adjectives, no unshipped features; `make docs-check` green.
- Commit `[M3.198] ...` with `docs/index.md` (+ optionally `docs/tutorial.md`), done, no push.
