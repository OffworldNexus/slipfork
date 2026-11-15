# CLI Reference (`0.2.0`)

Exact commands and flags implemented by `crates/slipfork/src/main.rs` and `crates/slipfork/src/bin/slipforkd.rs` (`slipfork --help`, `slipforkd --help`).

```console
$ slipfork doctor                                    # probe CoW reflink, Incus socket, unprivileged netns
$ slipfork snow resolve --file snow.yml --mode local # validate modes and persist !! secrets (0600)
$ slipfork dag plan --file slipfork.toml --repo .    # print artifact and fixture rebuild waves
$ slipfork up                                        # boot the canonical world through zero-setup slipforkd
$ slipfork list                                      # one origin URL per world
```

## Global Options

| Option | Default | Description |
|---|---|---|
| `--json` | `false` | Emit JSON instead of human text |
| `--state-dir <STATE_DIR>` | `$XDG_CACHE_HOME/slipfork` or `~/.cache/slipfork` | State root for CoW snapshots, worlds, and DAG digest checks |
| `--runtime-dir <RUNTIME_DIR>` | `$XDG_RUNTIME_DIR/slipfork` or `<STATE_DIR>/run` | Runtime root holding the `0600` `daemon.sock` |
| `-h, --help` / `-V, --version` | — | Print help or the CLI version |

`--json` is global: `slipfork --json list`. Human diagnostics go to `stderr`; with `--json`, a failure prints `{"ok": false, "error": "..."}` to **stdout**.

| Command | `--json` payload |
|---|---|
| `doctor` | `{"ok", "uid", "gid", "state_root", "incus_daemon_socket", "reflink_64k_micros", "isolated_netns"}` |
| `snow resolve` | a flat `{"<VAR>": "<value>"}` map (secrets stay `[REDACTED]` without `--expose-secrets`) |
| `dag plan` | `{"artifacts": [...], "fixture_waves": [[...]]}` with `"action": "cached" \| "rebuild"` |
| `up` / `fork` | `{"world", "origin", "resources": [{"id", "strategy", "resolved", "reason"}]}` |
| `list` | `[{"world", "state", "origin", "worktree"}]` — exactly one `origin` URL per world |
| `prune` | `{"dry_run", "worlds": [...], "snapshots": [...]}` |
| `secret list` | `["KEY", ...]` (names only, never values) |
| `share` | `{"ok", "origin", "lease_id"}` |
| `daemon unit` | `{"socket": "...", "service": "..."}` (the unrequested unit is `null`) |

| Exit code | Meaning |
|---|---|
| `0` | Success |
| `1` | A `slipfork:` runtime error (invalid manifest, missing file, non-CoW filesystem, daemon refusal) |
| `2` | A `clap` usage error |

---

## Local Diagnostics

### `slipfork doctor`

Probes the host: Incus Unix socket presence (`/var/lib/incus/unix.socket`), a 64 KiB `ioctl(FICLONE)` round-trip in `<STATE_DIR>`, and unprivileged `unshare --map-user --map-group --keep-caps --net --pid` loopback bring-up.

```console
$ slipfork doctor
state_root: /home/acme/.cache/slipfork
incus:      absent (using reflink-netns)
reflink:    ok (64 KiB FICLONE in 447 us)
netns:      ok (uid=1000:1000)
```

Exits non-zero when reflink or the isolated network namespace fails.

### `slipfork snow resolve`

Validates `snow.yml`, persists any missing `!!` secret into the local `0600` vault, and prints the resolved variables of one mode (`[REDACTED]` by default).

| Option | Default | Description |
|---|---|---|
| `--file <FILE>` | `snow.yml` | Path to `snow.yml`, relative to the current directory |
| `--mode <MODE>` | `local` | Mode to resolve (`local`, `cloud`, `all`, or any declared mode) |
| `--component <COMPONENT>` | all groups | Restrict output to the environment groups of one component |
| `--expose-secrets` | `false` | Print plaintext secrets instead of `[REDACTED]` |

### `slipfork dag plan`

Hashes repository inputs with SHA-256 Merkle trees and prints `Cached` vs `Rebuild` for every `[artifacts.<id>]` and every topological `[fixtures.<id>]` wave. Planning only reads the digest cache, so every node reports `Rebuild` until something materializes its digest (M3).

| Option | Default | Description |
|---|---|---|
| `--file <FILE>` | `<vcs_root>/slipfork.toml` | Path to `slipfork.toml` |
| `--repo <REPO>` | `<vcs_root>` | Repository root walked for input globs (must be the VCS root of `--file`) |

---

## Daemon-Served Commands

Every command below drives the unprivileged `slipforkd` daemon over `<runtime_dir>/daemon.sock` (`0600`). When the socket does not answer, `slipfork` auto-spawns `slipforkd --listen 127.0.0.1:0 --state-dir <state> --runtime-dir <runtime>` in the background (diagnostics in `<runtime_dir>/daemon.log`, `0600`). `HOME` is checked fail-closed before any directory, socket, or daemon side effect.

### `slipfork up [--world <FORK>] [--no-dump | --refresh-dump] [--resources local]`

Boots (or idempotently confirms) the world selected by `--world` or the nearest ancestor of `cwd`: `.worktrees/<fork>[/...]` → fork `<fork>`, repository root → canonical. The daemon resolves the repo root, `snow.yml` components, `[resources]`, the single `WorldOrigin` (before materialization), attaches the world, starts components, streams `pre-deploy`/`post-deploy` hooks, and returns exactly one origin URL plus `[resources]` diagnostics. A failed hook keeps the world attached for inspection.

### `slipfork down [--world <FORK>]`

Releases the world from the daemon (services and CoW state are destroyed) while keeping `.worktrees/<fork>`. Repeating it is an idempotent success.

### `slipfork fork <FORK> [--base <REV>] [--no-dump | --refresh-dump] [--resources local]`

Creates `.worktrees/<fork>` (Git worktree with `refs/heads/<fork>` and `refs/slipfork/<fork>/wip`) plus its CoW world, mounts the fork checkout, and prints the fork's single origin URL.

### `slipfork drop <FORK>`

Destroys the fork world and runs the workspace adapter's `forget` (removes the worktree, branch, and snapshot ref).

### `slipfork list`

Prints one row per world with its state and **exactly one** origin URL. `--json` emits the same rows as objects.

```console
$ slipfork list
WORLD            STATE     ORIGIN                                 WORKTREE
acme             running   http://acme.localhost:41231            /home/acme/acme
```

### `slipfork prune [--dry-run | --yes]`

Previews (`--dry-run`, also the default when neither flag is passed) or reaps orphaned worlds whose worktree disappeared. `--json` emits the typed report. Snapshot/cache garbage collection lands with the engine's snapshot-deletion capability (M3).

### `slipfork secret set [--force] <KEY>` / `unset <KEY>` / `list`

Stores a value read from **stdin** (an interactive terminal is refused so the value is never echoed), refuses to overwrite an existing key unless `--force` is passed, prints key names only for `list`, and removes one key with `unset`. The vault is the local `0600` JSON file at `~/.local/share/slipfork/vaults/<project>.json`. Secret values never appear in stdout, stderr, logs, or `--json` output.

### `slipfork share [--stop] [--world <FORK>]`

Mints a tunnel lease for a world. Until the `weaved` tunnel adapter ships (`H-006`), `share` fails closed (`EdgeError::NoForwardListener` when `slipforkd` has no forward listener, or `EdgeError::TunnelUnavailable`), and `share --stop` on a local world is an idempotent success returning its `.localhost` origin.

### `slipfork exec [--world <FORK>] [--cwd <DIR>] -- <ARGV>...`

Asks the daemon for the world's direct-entry `WorldEntryCommand` and resolved `ExecEnv`, then spawns `nsenter`/`incus exec` with `env_clear()` and the inherited native stdio. The child exit code becomes `slipfork`'s exit code.

### `slipfork daemon unit [--only socket | service] [--forward-port <U16>]`

Renders non-template systemd units to **stdout** with zero filesystem writes: `User=<resolved-developer-account>` (never `%` template specifiers), `ExecStart=<slipforkd> --state-dir <DIR> --runtime-dir <DIR>`, empty `CapabilityBoundingSet=`, and `ListenStream=127.0.0.1:80`, `[::1]:80`, and `127.0.0.1:<forward-port>`. `--forward-port` defaults to `7080` and must not collide with `:80`.

```console
$ slipfork daemon unit --only socket  | sudo tee /etc/systemd/system/slipforkd.socket
$ slipfork daemon unit --only service | sudo tee /etc/systemd/system/slipforkd.service
$ sudo systemctl daemon-reload && sudo systemctl enable --now slipforkd.socket
```

---

## `slipforkd`

`slipforkd` is the sole stateful Composition Root and runs unprivileged. It requires explicit paths and never guesses them:

```console
$ slipforkd --state-dir <STATE_DIR> --runtime-dir <RUNTIME_DIR> --listen 127.0.0.1:0
```

| Option | Description |
|---|---|
| `--state-dir <STATE_DIR>` | CoW snapshots, worlds, generated world homes |
| `--runtime-dir <RUNTIME_DIR>` | `0600` `daemon.sock` |
| `--listen <ADDR>` | Bind the HTTP ingress (`127.0.0.1:0` for zero-setup); ignored when `LISTEN_FDS` is set |
| `--forward-port <PORT>` | Also bind `127.0.0.1:<PORT>` as the planned static forward listener |
| `--world-init <PATH>` | Explicit `slipfork-world-init` helper (defaults to the sibling executable) |

With `LISTEN_PID`/`LISTEN_FDS` present, `slipforkd` adopts the inherited descriptors by explicit fd order and fails closed on any mismatch; it never falls back to `--listen`. Without either activation or `--listen`, startup fails closed. `slipforkd` binds `<state_dir>/http-ingress.sock` (`0600`) for the in-world `$BASE_URL` bridge alongside its TCP HTTP/WS ingress.
