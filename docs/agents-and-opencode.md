# OpenCode V2 Worktree Plugin (`0.1.0`)

`packages/opencode-slipfork` (`@slipfork/opencode`) is the OpenCode V2 Server plugin that binds a git worktree to a Slipfork CoW world, so parallel sessions get isolated PostgreSQL, Redis, S3, and ports **without replacing or renaming the built-in `shell` tool**.

!!! warning "Do not enable it against a `0.1.0` binary"
    The plugin drives `slipfork fork`, `exec`, and `drop`, which refuse to run until the `slipforkd` daemon lands in `0.2.0` (they exit `2`, see the [CLI reference](reference/cli.md#daemon-owned-commands-020)). Enabling the plugin today makes every OpenCode worktree creation fail — the plugin rolls the worktree back and surfaces the refusal. The configuration and hooks below are wired and tested; the end-to-end flow starts with `0.2.0`.

---

## Configuration in `.opencode/opencode.jsonc`

Keep worktrees on a CoW-capable disk directory (`.worktrees`, never RAM-backed `/tmp` `tmpfs`) and load the plugin:

```jsonc title=".opencode/opencode.jsonc"
{
  "$schema": "https://opencode.ai/config.json",
  "worktree": {
    "directory": ".worktrees"
  },
  "plugins": [
    { "package": "../packages/opencode-slipfork", "options": { "binary": "slipfork" } }
  ]
}
```

Relative plugin paths resolve from the **config file's** directory (so `.opencode/opencode.jsonc` needs `../packages/...`), and a plugin entry must be a directory or package name — a single `.ts` file is skipped. `../packages/opencode-slipfork/package.json` exposes `./src/index.ts`. From a Slipfork checkout use that path (the npm package publishes in `0.2.0`); `options.binary` defaults to `slipfork` on `PATH`.

---

## What the Plugin Wires

| Hook | Behaviour |
|---|---|
| `ctx.worktree.transform` | On worktree create: `git worktree add`, then `slipfork fork wt-<slug> --workspace <worktree>`. On failure it removes the new worktree and re-raises the error. On remove: `slipfork drop wt-<slug>`, then `git worktree remove`. |
| `ctx.shell.hook("create.before")` | Any shell command whose `cwd` is inside a registered worktree is rewritten to run through `slipfork exec --world wt-<slug> --cwd <cwd> -- <shell> -c <command>`, with `SLIPFORK_ACTIVE_WORLD` set. Commands outside a registered worktree pass through untouched. |
| `ctx.session.hook("context")` | Appends a short `<context key="slipfork-world">` hint so the model knows its worktree owns an isolated world. |

`--snow-file <worktree>/snow.yml` is added to the `exec` invocation when that file exists, so `modes.local` variables resolve inside the world instead of on the host.

---

## Status and Roadmap

- Today: configuration, worktree rollback, `cwd` routing, and the session hint are implemented and covered by `packages/opencode-slipfork/tests`.
- `0.2.0`: with `slipforkd` running, the three commands succeed and the plugin becomes end-to-end.
- The plugin never resolves `snow.yml` itself — `slipforkd` is the sole owner of environment resolution, and the plugin only routes.
