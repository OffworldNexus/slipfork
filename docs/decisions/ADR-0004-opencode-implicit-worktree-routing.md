# ADR-0004: Zero-Confusion OpenCode Worktree Worlds via Native `worktree` & `shell` Hooks

- **Status:** Accepted
- **Date:** 2026-11-14

## Context

Replacing OpenCode's built-in `shell` tool with a custom tool (`slipfork_exec`) or requiring agents to prepend wrapper commands causes frequent LLM confusion: built-in prompts, skills, and subagents expect `shell`, and forgotten prefixes corrupt the canonical checkout's database or fail with port conflicts.

## Decision

`@slipfork/opencode` uses three native OpenCode V2 server plugin hooks without renaming or replacing `shell`:

1. **`ctx.worktree.transform`:** Registers a `WorktreeDefinition` that creates git worktrees under `.worktrees/<name>` (on XFS disk, never `/tmp` `tmpfs`) and atomically calls `slipfork fork` to clone the canonical `local` world; `remove` destroys both the CoW world and the git worktree.
2. **`ctx.shell.hook("create.before")`:** Intercepts every shell invocation, looks up the CoW world bound to `event.cwd`, injects resolved `snow.yml` (`modes.local`) environment variables, and wraps execution through `slipfork exec --cwd <cwd>` inside that world's isolated network/service namespace.
3. **`ctx.session.hook("context")`:** Appends a 2-line `<context key="slipfork-world">` note stating that the session's directory has its own isolated services and `snow.yml` envs pre-injected.

## Consequences

- LLMs use ordinary `shell`, `read`, `edit`, `glob`, and `grep` tools seamlessly while parallel sessions in `.worktrees/*` enjoy complete database, Redis, S3, and port isolation.
