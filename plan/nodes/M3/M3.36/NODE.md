---
id: M3.36
title: "Clean-sheet high-level specs/ folder: Forkfile (with image), CLI UX, Under the Hood (pure Btrfs) & Testing Plan"
kind: task
status: done
owner: architect
priority: 3
depends_on: []
paths: ["specs/**"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-20T16:24"
updated: "2026-11-20T16:32"
---

# Goal

Create a clean-sheet, high-level, simple specification suite in `specs/` from scratch so the owner can read and validate the entire design in 5–10 minutes before any code is rewritten:
- `specs/README.md` — Short overview & map of the 4 specs
- `specs/01-forkfile.md` — **The `Forkfile` Specification**
- `specs/02-cli-ux.md` — **The CLI UX**
- `specs/03-under-the-hood.md` — **How It Works Under the Hood**
- `specs/04-testing-plan.md` — **The Testing Plan**

# Non-Negotiable Product Truths (Direct From Owner)

1. **First-class `image` notion in `Forkfile`:**
   - `image <id>` is a core top-level construct (`image app .`, `image api ./api { dockerfile ...; on dev: stage dev }`, or `image mailpit from axllent/mailpit:latest`).
   - Services (`api`, `worker`, `scheduler`) and `task`s reference `image <id>` — multiple services/tasks share one built image, CI (Kerfufoo) builds & pushes each declared `image` once, and locally Slipfork builds & caches each `image` as a Btrfs subvolume rootfs.
2. **Pure Btrfs end-to-end — ZERO ext4/Btrfs mixing, ZERO `isolate` / `volume` bullshit:**
   - The whole project folder lives on a Btrfs drive (as a Btrfs subvolume), alongside Slipfork's image cache and world database/storage subvolumes.
   - Because the project directory itself (including `.venv`, `node_modules`, build caches, working tree) is a Btrfs subvolume, `slipfork fork <name>` snapshots BOTH the project subvolume AND the world's database/redis/storage subvolumes in milliseconds. There is **NO** `isolate .venv` or `volume node_modules` directive in `Forkfile`!
3. **Clean, high-signal, colorful CLI UX — zero split-brain or TTY bugs:**
   - Bare `slipfork` renders a clean, colorful, aligned tree (World -> Services & Resources with real ports `:8000`/`:5432`, routes `/api`, human ages `12m` -> Forks with their URLs & paths; never internal IDs like `pg_clusters.pg`).
   - Real TTY colors + live step spinners (`✓ Image ...`, `✓ Postgres ...`, `✓ Service ...`), strict `NO_COLOR`/`TERM=dumb`/non-TTY cleanliness, chronological multi-service `logs -f` surviving `restart`, single user daemon + thin `:80` `.localhost` proxy (zero root/user daemon split-brain, zero `slipfork` vs `slipforkd` binary skew).
4. **High-level & simple writing:**
   - Keep every file short, concrete, visual (1–2 small Mermaid diagrams where helpful), and easy to read — zero Rust struct/trait dumps, zero ioctl hex codes, zero `ADR-*`/`M3.*` archaeology.
   - `specs/04-testing-plan.md` follows the pure user-story E2E philosophy (~60 lines, zero file/crate/code names).

# Acceptance

- All 5 files in `specs/` exist, are concise, cohesive, internally 100% consistent (`image` everywhere, zero `volume`/`isolate`), and pass `uv run --no-project tools/guard.py`.
