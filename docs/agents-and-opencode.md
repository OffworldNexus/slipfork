# OpenCode V2 Worktree Plugin (`0.2.0`)

`packages/opencode-slipfork` (`@slipfork/opencode`) is the OpenCode V2 Server plugin that binds a git worktree to a Slipfork CoW world, so parallel sessions get isolated PostgreSQL, Redis, S3, and ports **without replacing or renaming the built-in `shell` tool**.

```console
$ slipfork up                    # canonical world for the repository
$ slipfork fork feat-billing     # the plugin's worktree transform runs the same daemon call
$ slipfork --json list           # the plugin resolves world/worktree/origin from this payload
```

## Configuration in `.opencode/opencode.jsonc`

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

Relative plugin paths resolve from the **config file's** directory (so `.opencode/opencode.jsonc` needs `../packages/...`), and a plugin entry must be a directory or package name — a single `.ts` file is skipped. `../packages/opencode-slipfork/package.json` exposes `./src/index.ts`; from a Slipfork checkout use that path. `options.binary` defaults to `slipfork` on `PATH`, and every command runs from the repository root so the daemon can resolve worlds.

Keep worktrees on a CoW-capable disk directory (`.worktrees`, never RAM-backed `/tmp` `tmpfs`). Slipfork always owns `<repo>/.worktrees/<fork>`; the `worktree.directory` value must be `.worktrees` so OpenCode and the CLI agree.

## What the plugin wires

| Hook | Behaviour |
|---|---|
| `ctx.worktree.transform` | Registers the project root plus daemon-owned worktrees; worktree create runs `slipfork --json fork <fork>`, remove runs `slipfork drop <fork>`, and a failed fork rolls back. |
| `ctx.shell.hook("create.before")` | A command whose `cwd` is inside a plugin-owned worktree is rewritten to `/bin/sh` running `slipfork exec --world <fork> --cwd <cwd> -- <shell> -c <command>`, with `SLIPFORK_ACTIVE_WORLD` set. Canonical and unrelated directories pass through untouched. |
| `ctx.session.hook("context")` | Appends a `<context key="slipfork-world">` note naming the world, its checkout, and its single origin URL so the model knows `shell` already runs inside it. |

The plugin never resolves `snow.yml`: `slipforkd` is the sole owner of environment resolution, and the plugin only builds CLI arguments and decodes `--json` payloads.

## Project state and failure modes

The plugin probes `<repo>/snow.yml` once per session. A missing or unreadable manifest produces a `<context key="slipfork-project">` notice and leaves shell commands untouched, rather than half-routing a session. A daemon that refuses a fork surfaces as a `slipfork fork failed: <stderr>` error and the worktree is removed again.

```console
$ slipfork --json list
[
  {
    "origin": "http://acme.localhost:41921",
    "state": "running",
    "worktree": "/home/acme/acme",
    "world": "acme"
  }
]
```

## Tests

`packages/opencode-slipfork/tests` covers the CLI argument contract, the pure coordinator, and a live end-to-end run against the real `slipfork` binary:

```console
$ bun test packages/opencode-slipfork/tests
```
