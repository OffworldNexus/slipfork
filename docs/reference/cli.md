# CLI Reference (`0.2.0`)

`slipfork` is a thin client: `doctor` and `dag plan` read the host locally, `snow resolve` asks `slipforkd` (it owns the vault), and every world command talks to the unprivileged `slipforkd` daemon over a `0600` `<runtime_dir>/daemon.sock`. When nothing answers there, the CLI starts `slipforkd --listen 127.0.0.1:0` in the background (`<runtime_dir>/daemon.log`, `0600`).

```console
$ slipfork doctor
state_root: /home/acme/.cache/slipfork
incus:      absent (using reflink-netns)
reflink:    ok (64 KiB FICLONE in 414 us)
netns:      ok (uid=1000:1000)

$ slipfork up
[resources] appdb: prefer_remote -> local (not_offered)
[ready]     api (internal :8000 in world netns)
[origin]    http://acme.localhost:41921
```

## Global options

| Option | Default | Description |
|---|---|---|
| `--json` | `false` | Emit JSON on stdout |
| `--state-dir <DIR>` | `$XDG_CACHE_HOME/slipfork` or `~/.cache/slipfork` | CoW snapshots, worlds, `cache/{rootfs,oci,runtime}` |
| `--runtime-dir <DIR>` | `$XDG_RUNTIME_DIR/slipfork` or `<state_dir>/run` | Holds the `0600` `daemon.sock` |
| `-h, --help` / `-V, --version` | — | Help or CLI version |

`--json` is global (`slipfork --json list`). Human diagnostics go to stderr; with `--json`, a failure prints `{"ok": false, "error": "..."}` on stdout. Exit `0` is success, `1` a `slipfork:` runtime error, and `2` a `clap` usage error. `exec`, `shell`, `db`, `redis`, and `hook run` instead exit with the child command's status (so `slipfork exec -- false` exits `1` with no `slipfork:` line), and their child stdout is not JSON even under `--json`.

## Commands

Every command below is in `slipfork --help`; each example is copied from a real run.

| Command | Behaviour | Example |
|---|---|---|
| `doctor` | Probe Incus socket, a 64 KiB `ioctl(FICLONE)` round-trip, and unprivileged netns | `slipfork doctor` |
| `snow resolve` | Validate `snow.yml`, persist missing `!!` secrets, print one mode (`[REDACTED]`) | `slipfork snow resolve --file snow.yml --mode local` |
| `dag plan` | Merkle-hash inputs and print `Cached` / `Rebuild` per artifact and fixture wave | `slipfork dag plan --file slipfork.toml --repo .` |
| `up` | Boot or confirm the world selected by `--world` or `cwd`; print one origin URL | `slipfork up` |
| `down` | Stop a world's services, keep its worktree (idempotent) | `slipfork down --world feat-billing` |
| `fork` | Create `.worktrees/<fork>` plus its CoW world and print its origin | `slipfork fork feat-billing` |
| `drop` | Destroy a fork world, its worktree, branch, and snapshot ref | `slipfork drop feat-billing` |
| `list` | One row per world: state, exactly one origin URL, worktree | `slipfork list` |
| `prune` | Preview (`--dry-run`, default) or reap (`--yes`) orphaned worlds | `slipfork prune --dry-run` |
| `secret set` | Store a value read from **stdin**; `--force` rotates an existing key | `printf '%s' "$V" \| slipfork secret set STRIPE_SECRET_KEY` |
| `secret unset` | Remove one stored key | `slipfork secret unset STRIPE_SECRET_KEY` |
| `secret list` | Print key names only, never values | `slipfork secret list` |
| `share` | Tunnel sharing is unavailable until `H-006`/M2.22 and fails closed; `--stop` releases a lease | `slipfork share` |
| `daemon unit` | Render the non-template systemd units on stdout (`--only socket\|service`) | `slipfork daemon unit --only socket` |
| `exec` | Run a command non-interactively inside a world | `slipfork exec -- id` |
| `shell` | Enter a world interactively (requires a TTY) | `slipfork shell` |
| `logs` | Read or follow retained component/hook logs (`--follow`, `--lines`, `--stderr`) | `slipfork logs --lines 3 api` |
| `hook run` | Re-run one `<component>:<phase>` hook (`--no-tty` when not interactive) | `slipfork hook run api:pre-deploy` |
| `db` | Open `psql` inside the world's running `postgres:<v>` container (TTY) | `slipfork db appdb` |
| `redis` | Open `redis-cli` inside the world's running `redis:<v>` container (TTY) | `slipfork redis cache` |
| `forward` | Bind daemon-owned `127.0.0.1:0` listeners bridged into a world, held until Ctrl-C | `slipfork forward pg_databases.appdb` |
| `auth login` | Pair an OSPP device key from a `#handle=` URL (`-` reads stdin) | `slipfork auth login -` |
| `support` | Report registry tiers and required evidence (`ADR-0007`) | `slipfork support` |

## World selection and boot options

`up`, `down`, `share`, `exec`, `shell`, `logs`, `hook run`, `db`, `redis`, and `forward` accept `--world <FORK>`; omitted, the world is inferred from `cwd`: `.worktrees/<fork>[/...]` selects fork `<fork>`, the repository root selects the canonical world.

| Option | Applies to | Description |
|---|---|---|
| `--world <FORK>` | every world command | Explicit world fork |
| `--base <REV>` | `fork` | Commit-ish to fork from (default: requesting checkout's `HEAD`) |
| `--no-dump` / `--refresh-dump` | `up`, `fork` | Skip seed dumps and run a clean `initdb` / require a live dump check (fails closed offline) |
| `--resources local` | `up`, `fork` | Force hermetic world-local resources |
| `--host-caches` | `up`, `fork` | Mount host toolchain caches read-only **after** the ELF/libc ABI probe passes; never a `slipfork.toml` key |
| `--cwd <DIR>` | `exec` | Directory used to resolve and enter the world |

`--host-caches` mounts `~/.cargo/{registry,git,bin}`, `~/.rustup`, `~/.cache/uv`, `~/.local/share/uv`, and `~/.local/share/pnpm/store` read-only, and fails closed naming both ABIs when the host and world architectures, libc family, or glibc versions disagree.

| Command | Command-specific flags |
|---|---|
| `snow resolve` | `--file <FILE>` (default `snow.yml`), `--mode <MODE>` (default `local`), `--component <ID>`, `--expose-secrets` |
| `dag plan` | `--file <FILE>` (default `<vcs_root>/slipfork.toml`), `--repo <DIR>` (default `<vcs_root>`) |
| `logs` | `--follow`, `--lines <N>`, `--stderr`, optional target (`api`, `api:pre-deploy`) |
| `hook run` | `--no-tty` (run through the daemon instead of a native TTY) |
| `forward` | positional targets (`pg_databases.main`, `components.api`); no target forwards all |
| `daemon unit` | `--only socket` / `--only service`, `--forward-port <PORT>` (default `7080`) |
| `secret set` | `--force` rotates an existing value |

## Failure modes you will actually see

```console
$ slipfork share
slipfork: slipforkd has no forward listener, so it cannot share a world: start it from the slipforkd.socket unit, or pass --listen <addr> --forward-port <port>

$ slipfork db
slipfork: `slipfork pg_databases.appdb client` needs an interactive terminal; `slipfork db` / `slipfork redis` need an interactive terminal

$ slipfork shell
slipfork: `slipfork shell` needs an interactive terminal; use `slipfork exec -- <cmd>` for non-interactive commands
```

- `share` fails closed until the `weaved` tunnel adapter lands (`H-006`/`M2.22`); `share --stop` on a local world is an idempotent success.
- `db`/`redis` fail closed with `ServiceNotRunning` unless that service's container is running in the world.
- `prune` reaps orphaned worlds; it does not garbage-collect `<state_dir>/cache/{rootfs,oci}` yet.

## `slipforkd`

`slipforkd` is the sole stateful Composition Root and runs unprivileged. It requires explicit paths and never guesses them:

```console
$ slipforkd --state-dir ~/.cache/slipfork --runtime-dir ~/.local/state/slipfork --listen 127.0.0.1:0
```

It runs in the foreground and logs to stderr; `slipfork` starts the same binary for you when no daemon answers.

| Option | Description |
|---|---|
| `--state-dir <DIR>` | CoW snapshots, world clones, `cache/rootfs`, `cache/oci` |
| `--runtime-dir <DIR>` | `0600` `daemon.sock` |
| `--listen <ADDR>` | HTTP ingress (`127.0.0.1:0` for zero-setup); ignored when `LISTEN_FDS` is set |
| `--forward-port <PORT>` | Also bind `127.0.0.1:<PORT>` as the static forward listener |
| `--world-init <PATH>` / `--crun <PATH>` | Explicit helper and pinned static `crun` binaries |

With `LISTEN_PID`/`LISTEN_FDS` present, `slipforkd` adopts inherited descriptors by fd order and fails closed on any mismatch. `slipfork daemon unit` renders the matching units to stdout with zero filesystem writes (`User=<your-account>`, empty `CapabilityBoundingSet=`, `ListenStream=127.0.0.1:80`, `[::1]:80`, `127.0.0.1:7080`):

```console
$ slipfork daemon unit --only socket | sudo tee /etc/systemd/system/slipforkd.socket
$ slipfork daemon unit --only service | sudo tee /etc/systemd/system/slipforkd.service
$ sudo systemctl daemon-reload && sudo systemctl enable --now slipforkd.socket
```

## `git-remote-slipfork`

`git` invokes this helper for a `slipfork://<fork>` remote: it lists and fetches the fork's branch and `refs/slipfork/<fork>/wip` from the running daemon, so another checkout or agent can pull a fork's work. The world must be running in the same `slipforkd`.

```console
$ git remote add fork slipfork://feat-billing && git fetch fork
git-remote-slipfork: world 'acme:feat-billing' is not running in this slipforkd; run `slipfork up` first
```
