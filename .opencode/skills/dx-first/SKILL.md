---
name: DX-First (Dream Tool Documentation Before Code)
description: MANDATORY for every planner, doc-writer, designer, architect, reviewer, critic, lead, and strategist in Slipfork. Everything stems from Developer Experience: write and critique the lean user-facing dream-tool documentation BEFORE internal design or code.
---

# DX-First: Write the Dream Tool First, Then Build It

Slipfork exists to make spinning up dev environments, forking agent worktrees, and running heavy polyglot test suites feel **instantaneous and invisible**. If adopting Slipfork requires rewriting tests, fixing root-owned files, juggling `.env` files, or teaching an LLM custom shell syntax, we failed.

## 1. Docs Before Design, Design Before Code — Without Selling Vaporware!

Before writing internal Rust/TypeScript code for any feature, hypervisor backend, or language test adapter:

1. **Draft the Target DX Blueprint FIRST in `docs/design/dx-<slug>.md` (excluded from public docs!):**
   - Never publish documentation in `docs/` or `README.md` that advertises CLI commands, flags, or integrations that do not exist yet in the compiled `slipfork` binary!
   - Instead, write the dream user workflow first as an **unpublished DX spec** (`docs/design/dx-<slug>.md` or `docs/design/target-dx.md`, excluded via `exclude_docs` in `zensical.toml`), covering:
     * **Local developer machine:** `snow.yml` (`modes.all` + `modes.local`), shared `$UID:$GID` mounts, `.localhost` routing, and auto-generated `!!` secrets.
     * **Parallel tests:** World-owned stack + orthogonal **Artifact Volumes** + **Fixture Snapshot DAG**.
     * **AI coding agents (`@slipfork/opencode`):** Implicit worktree forks and `cwd`-based `shell` routing.
2. **Critique the DX spec with fresh eyes** (`critic` with `dx` and `ai-ergonomics` lenses). Every extra CLI flag, second config file, manual `chmod`, or wall of kernel prose is a defect.
3. **Promote to published `docs/` ONLY alongside the real working implementation:** Public `docs/` (`zensical.toml`) and `README.md` describe **strictly what runs today** (with future vision kept clearly in `docs/VISION.md` or a small explicitly labeled roadmap corner). Every CLI command and config block in published `docs/` must work against the current binary.

## 2. Ruthless Documentation Leanness

- **Lead with the 10-line working example.** Put Incus REST internals, `FICLONE` ioctls, CRIU mechanics, and Linux namespace flags in `ARCHITECTURE.md` — never at the top of user guides.
- **One canonical way per task.** Never document two syntaxes or a "legacy mode".
- **Scannable structure:** Short sentences, active voice, crisp comparison tables, ≤ 160 lines per user page, verified with `uvx --offline zensical build --strict`.
