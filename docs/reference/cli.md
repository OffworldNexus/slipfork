# CLI Reference

All commands discover `snow.yml` (and optional `slipfork.toml`) by walking upwards from the current working directory. Pass `--json` to any command for deterministic machine-readable output.

## Lifecycle & Development

| Command | Description |
|---|---|
| `slipfork doctor` | Probe available CoW engines (`incus`, `reflink-netns`), filesystem `FICLONE` latency, UID mapping, and namespaces |
| `slipfork init` | Migrate a legacy `snow.yml` (`envs:`) to `modes:` (`all`, `cloud`, `local`) |
| `slipfork up [--mode local]` | Resolve `snow.yml` secrets/envs, materialize base services, run `pre-deploy` hooks, and start components |
| `slipfork down` | Stop running local world processes while keeping cached CoW snapshots on disk |
| `slipfork exec [--cwd DIR] -- <cmd...>` | Execute a command inside the CoW world bound to `DIR` (with `snow.yml` envs & shared `$UID`) |
| `slipfork shell` | Open an interactive `$SHELL` inside the active directory's CoW world |

## Worktree & Snapshot Forking

| Command | Description |
|---|---|
| `slipfork fork <name> [--from <snap>]` | Create git worktree `.worktrees/<name>` and fork an isolated CoW world in milliseconds |
| `slipfork snap <label> [--stateful]` | Capture a named CoW snapshot of the current world (disk + optional live RAM on Incus VM/CRIU) |
| `slipfork list` | List active worlds, bound directories, snapshot lineage, and disk usage |
| `slipfork drop <name>` | Destroy a forked world (and optionally its git worktree with `--remove-worktree`) |
| `slipfork prune` | Garbage-collect unreferenced artifact volumes and stale fixture snapshots |

## Testing & Manifest Resolution

| Command | Description |
|---|---|
| `slipfork test [-j N] [-- <runner...>]` | Build changed artifacts, materialize fixture snapshot DAG, and run tests in parallel CoW clones |
| `slipfork dag plan` | Print the content-addressed execution plan (Merkle hashes, cache hits/misses, topological layers) |
| `slipfork snow resolve [--mode <m>] [--component <id>]` | Validate `snow.yml`, persist any missing `!!` secrets, and print resolved/redacted env status |
| `slipfork secret set <KEY>` | Store a secret in the local `0600` project vault (`stdin`, never shell argv) |
