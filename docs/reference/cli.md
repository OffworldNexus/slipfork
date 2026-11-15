# CLI Reference (`0.1.0`)

Exact commands and flags implemented in `crates/slipfork/src/main.rs` (`slipfork --help`).

## Global Options

| Option | Default | Description |
|---|---|---|
| `--json` | `false` | Emit deterministic machine-readable JSON output |
| `--state-dir <STATE_DIR>` | `$XDG_CACHE_HOME/slipfork` or `~/.cache/slipfork` | Override state root for snapshots, worlds, and DAG digest checks |
| `-h, --help` / `-V, --version` | — | Print help or CLI version (`0.1.0`) |

## Subcommands

### `slipfork doctor`
Probe Incus Unix socket presence (`/var/lib/incus/unix.socket`), 64 KiB `ioctl(FICLONE)` latency on `<STATE_DIR>`, and unprivileged `unshare` (`--map-user`, `--map-group`, `--keep-caps`, `--net`, `--pid`) loopback bring-up. Exits non-zero if `FICLONE` or isolated `netns` fails.

### `slipfork snow resolve`
Validate `snow.yml`, persist any missing `!!` secrets into `$XDG_DATA_HOME/slipfork/vaults/<slug>.json` (or `~/.local/share/slipfork/vaults/<slug>.json`, mode `0600`), and print resolved variables (`[REDACTED]` by default).

| Option | Default | Description |
|---|---|---|
| `--file <FILE>` | `snow.yml` | Path to `snow.yml` |
| `--mode <MODE>` | `local` | Active mode to resolve (`local`, `cloud`, `all`, or custom mode) |
| `--component <COMPONENT>` | *(all groups)* | Restrict resolution to environment groups attached to `<COMPONENT>` |
| `--expose-secrets` | `false` | Print plaintext secret values instead of `[REDACTED]` |

### `slipfork dag plan`
Hash repository input globs via SHA-256 Merkle trees and print `Cached` vs `Rebuild` status across `[artifacts.*]` and topological `[fixtures.*]` waves.

| Option | Default | Description |
|---|---|---|
| `--file <FILE>` | `<vcs_root>/slipfork.toml` | Path to `slipfork.toml` (resolved at the canonical VCS root when omitted) |
| `--repo <REPO>` | `<vcs_root>` | Repository root directory walked for input globs (roots inputs at the same canonical VCS root when omitted) |

### `slipfork snap --source <SOURCE> <LABEL>`
Capture a CoW snapshot named `<LABEL>` from `<SOURCE>` into `<STATE_DIR>/snapshots/<LABEL>` via kernel `ioctl(FICLONE)` (fails closed on `tmpfs`).

### `slipfork fork <NAME> [--from <FROM>] [--workspace <WORKSPACE>]`
Clone snapshot `<FROM>` (default: `canonical`) into `<STATE_DIR>/worlds/<NAME>/state` via `ioctl(FICLONE)`, mounting `<WORKSPACE>` as the world's bind path (echoed as `workspace_dir` in `--json`; not persisted). Defaults to the current directory.

### `slipfork exec --world <WORLD> [--cwd <CWD>] [--snow-file <SNOW_FILE>] -- <ARGV>...`
Execute `<ARGV>...` inside `<WORLD>`'s isolated `CLONE_NEWUSER | CLONE_NEWNET | CLONE_NEWNS | CLONE_NEWPID` namespace (`127.0.0.1` loopback up, host `$UID:$GID` mapped, optional `local`-mode env vars from `<SNOW_FILE>` injected). Propagates `<ARGV>`'s exit code.

### `slipfork drop <NAME>`
Destroy `<STATE_DIR>/worlds/<NAME>` and free its CoW state directory.
