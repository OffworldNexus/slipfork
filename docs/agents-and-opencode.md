# OpenCode V2 Plugin (`0.1.0`)

`@slipfork/opencode` makes parallel agent sessions safe. When OpenCode creates a worktree, the plugin forks a Slipfork world with it; when the agent runs a shell command there, the command executes inside that world.

```console
$ slipfork up                    # canonical world for the repository
$ slipfork fork feat-billing     # the plugin's worktree create runs the same daemon call
$ slipfork --json list           # the payload the plugin decodes
```

```jsonc title=".opencode/opencode.jsonc"
{
  "$schema": "https://opencode.ai/config.json",
  "worktree": { "directory": ".worktrees" },
  "plugins": [
    { "package": "../packages/opencode-slipfork", "options": { "binary": "slipfork" } }
  ]
}
```

!!! warning "Worktrees belong on disk"
    Keep `.worktrees` on the CoW filesystem, never RAM-backed `/tmp` `tmpfs`. Slipfork always owns `<repo>/.worktrees/<fork>`, and the OpenCode `worktree.directory` value must be `.worktrees` so both tools agree.

!!! info "Relative plugin paths"
    A relative `package` resolves from the config file's directory, so `.opencode/opencode.jsonc` needs `../packages/...`. A plugin entry must be a directory or package name; a single `.ts` file is skipped.

`options.binary` defaults to `slipfork` on `PATH`. Every command runs from the repository root so the daemon can resolve worlds.

## What the plugin wires

| Hook | Behaviour |
|---|---|
| `ctx.worktree.transform` | Registers the project root and daemon-owned worktrees; create runs `slipfork --json fork <fork>`, remove runs `slipfork drop <fork>`, and a failed fork rolls back |
| `ctx.shell.hook("create.before")` | A command whose `cwd` is inside a plugin-owned worktree is rewritten to `/bin/sh -c 'slipfork exec --world <fork> --cwd <cwd> -- <shell> -c <command>'`, with `SLIPFORK_ACTIVE_WORLD` set |
| `ctx.session.hook("context")` | Appends a `<context key="slipfork-world">` note naming the world, its checkout, and its origin URL |

The plugin never resolves `snow.yml`. `slipforkd` owns environment resolution; the plugin only builds CLI arguments and decodes `--json` payloads.

## Session flow

```mermaid
sequenceDiagram
  participant OC as OpenCode
  participant P as @slipfork/opencode
  participant CLI as slipfork
  participant D as slipforkd
  OC->>P: create worktree feat-billing
  P->>CLI: slipfork --json fork feat-billing
  CLI->>D: fork world + subvolume + checkout
  D-->>CLI: origin http://feat-billing.acme.localhost
  OC->>P: shell command with cwd .worktrees/feat-billing
  P->>CLI: slipfork exec --world feat-billing -- <shell> -c ...
  CLI->>D: run inside the fork world
```

Canonical and unrelated directories pass through untouched. A worktree created outside the plugin keeps normal shell behaviour.

## Project state and failure modes

The plugin probes `<repo>/snow.yml` once per session. A missing or unreadable manifest produces a `<context key="slipfork-project">` notice and leaves shell commands untouched, rather than half-routing a session.

A daemon that refuses a fork surfaces as a `slipfork fork failed: <stderr>` error and the worktree is removed again. No silent fallback to a host shell.

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

`origin` is port-less when the world is on `:80`, and carries the answered port otherwise.

## Tests

`packages/opencode-slipfork/tests` covers the CLI argument contract, the pure coordinator, and a live end-to-end run against the real `slipfork` binary:

```console
$ bun test packages/opencode-slipfork/tests
```
