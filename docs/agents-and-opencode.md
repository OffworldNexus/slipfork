# OpenCode V2 Worktree Plugin (`0.2.0`)

`packages/opencode-slipfork` (`@slipfork/opencode`) is the OpenCode V2 Server plugin that binds a git worktree to a Slipfork CoW world, so parallel sessions get isolated PostgreSQL, Redis, S3, and ports **without replacing or renaming the built-in `shell` tool**.

!!! warning "Plugin gateway is updated in `M2.18`"
    The plugin drives `slipfork fork`, `exec`, and `drop`, which now run through the zero-setup `slipforkd` daemon ([CLI reference](reference/cli.md#daemon-served-commands)). The plugin's own gateway and end-to-end test against a live daemon are updated in `M2.18`; until that lands, treat the configuration below as the target contract, not a shippable plugin.

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
| `ctx.worktree.transform` | On worktree create: `slipfork fork wt-<slug>` (the daemon creates the CoW world and its checkout); rollback on failure. On remove: `slipfork drop wt-<slug>`. |
| `ctx.shell.hook("create.before")` | Any shell command whose `cwd` is inside a registered worktree is rewritten to run through `slipfork exec --world wt-<slug> --cwd <cwd> -- <shell> -c <command>`, with `SLIPFORK_ACTIVE_WORLD` set. Commands outside a registered worktree pass through untouched. |
| `ctx.session.hook("context")` | Appends a short `<context key="slipfork-world">` hint so the model knows its worktree owns an isolated world. |

The gateway rewrite itself (including the plugin's argument construction) is updated in `M2.18`; `slipforkd` remains the sole owner of `snow.yml` environment resolution, and the plugin only routes.

---

## Status and Roadmap

- Today: configuration, worktree rollback, `cwd` routing, and the session hint are implemented and covered by `packages/opencode-slipfork/tests`.
- `M2.18`: the plugin gateway is updated to the daemon-served CLI and becomes end-to-end against a live `slipforkd`.
- The plugin never resolves `snow.yml` itself — `slipforkd` is the sole owner of environment resolution, and the plugin only routes.
