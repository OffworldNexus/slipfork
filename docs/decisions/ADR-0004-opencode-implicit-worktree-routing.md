# ADR-0004: Zero-Confusion OpenCode Worktree Worlds via Native `worktree` & `shell` Hooks

- **Status:** Accepted — amended for M2 (single env-resolution owner in `slipfork exec`)
- **Date:** 2026-11-15

## Context

Replacing OpenCode's built-in `shell` tool with a custom tool (`slipfork_exec`) or requiring agents to prepend wrapper commands causes frequent LLM confusion: built-in prompts, skills, and subagents expect `shell`, and forgotten prefixes corrupt the canonical checkout's database or fail with port conflicts.

## Decision

`@slipfork/opencode` uses three native OpenCode V2 server plugin hooks without renaming or replacing `shell`:

1. **`ctx.worktree.transform`:** Registers a `WorktreeDefinition` that creates git worktrees under `.worktrees/<name>` (on non-`tmpfs` disk, never `/tmp` `tmpfs`) and atomically calls `slipfork fork` to clone the canonical `local` world; `remove` destroys both the CoW world and the git worktree.
2. **`ctx.shell.hook("create.before")`:** Intercepts every shell invocation, resolves the world bound to `event.cwd` from explicit `SlipforkProjectState` (`M2.18`), and wraps execution through `slipfork exec --cwd <cwd> -- ...` inside that world's isolated network/service namespace. **Environment resolution has one owner:** `slipforkd` (`H-010` #1) resolves `snow.yml` (`modes.local`) environment variables and returns `WorldExecTarget` over the `0600` control socket to the thin `slipfork exec` client; the OpenCode plugin only routes commands and never resolves or injects `snow.yml` envs itself.
3. **`ctx.session.hook("context")`:** Appends a 2-line `<context key="slipfork-world">` note naming the bound world + single `WorldOrigin` URL, stating that `shell` runs inside the isolated world with `snow.yml` envs injected by `slipfork exec` (never connect to host `127.0.0.1:5432`/`:8000`).

## Consequences

- LLMs use ordinary `shell`, `read`, `edit`, `glob`, and `grep` tools seamlessly while parallel sessions in `.worktrees/*` enjoy complete database, Redis, S3, and port isolation.
