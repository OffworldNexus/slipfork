---
name: DX-First (Dream Tool Documentation Before Code)
description: MANDATORY for every planner, doc-writer, designer, architect, reviewer, critic, lead, and strategist in Slipfork. Everything stems from Developer Experience: write and critique the lean user-facing dream-tool documentation BEFORE internal design or code.
---

# DX-First: Write the Dream Tool First, Then Build It

Slipfork exists to make spinning up dev environments, forking agent worktrees, and running heavy polyglot test suites feel **instantaneous and invisible**. If adopting Slipfork requires rewriting tests, fixing root-owned files, juggling `.env` files, or teaching an LLM custom shell syntax, we failed.

## 1. Docs Before Design, Design Before Code

Before writing internal Rust/TypeScript code for any feature, hypervisor backend, or language test adapter:

1. **Write the lean user-facing documentation FIRST (`docs/`):**
   - **Local developer machine (`slipfork up`):** One `snow.yml` manifest -> instant shared-UID environment where Postgres, Redis, S3, and app components run with the developer's exact `$UID:$GID` on the shared source tree. Zero `root:root` files, zero `.env` drift, and auto-generated local secrets (`!! random_string(50)`).
   - **Parallel tests (`slipfork test`):** One command builds frontend/static artifacts once (cached orthogonally by Merkle hash), materializes the fixture snapshot DAG (`migrate -> seed_*`), and runs every heavy browser/BDD/transactional test in its own millisecond CoW clone on canonical ports.
   - **AI coding agents (`@slipfork/opencode`):** Creating a worktree (`worktree.create`) implicitly forks the running dev world in milliseconds, and standard `shell` commands route automatically to the right CoW world based on `cwd` — **zero custom tool names or flags for the LLM to remember**.
2. **Critique with fresh eyes** (`critic` with `dx` and `ai-ergonomics` lenses). Every extra CLI flag, second config file, manual `chmod`, or wall of kernel prose is a defect.
3. **Only after DX acceptance**, write the upward IoC/DI technical design (`docs/design/<slug>.md`) and implement the code so documented snippets work verbatim.

## 2. Ruthless Documentation Leanness

- **Lead with the 10-line working example.** Put Incus REST internals, `FICLONE` ioctls, CRIU mechanics, and Linux namespace flags in `ARCHITECTURE.md` — never at the top of user guides.
- **One canonical way per task.** Never document two syntaxes or a "legacy mode".
- **Scannable structure:** Short sentences, active voice, crisp comparison tables, ≤ 160 lines per user page, verified with `uvx --offline zensical build --strict`.
