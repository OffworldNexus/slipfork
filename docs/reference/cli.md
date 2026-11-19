# CLI Reference (`0.1.0`)

```console
$ slipfork                 # bare command = status: every world, one URL each
$ slipfork up              # boot the world selected by --world or cwd
$ slipfork status          # the same tree, also as `--json`
```

`slipfork` is a thin client. `doctor` reads the host locally; every world command and every secret operation talks to the unprivileged `slipforkd` over a `0600` `<state_dir>/run/daemon.sock`.

Only `up` and `fork` start `slipforkd --listen 127.0.0.1:0` when nothing answers. Every other verb fails closed with one line:

```console
$ slipfork ps
slipfork: no slipforkd on /home/acme/.cache/slipfork/run/daemon.sock — run `slipfork up`
```

!!! info "Port-less by default"
    After `sudo slipfork daemon install`, systemd owns `127.0.0.1:80` and `[::1]:80`, so origins are `http://demo.localhost` with no port. Without it, the daemon uses an ephemeral port and warns on stderr.

## Global options

| Option | Default | Description |
|---|---|---|
| `--json` | `false` | Emit one JSON document on stdout; no ANSI escapes |
| `-v, --verbose` | `false` | Add lineage and `[resources]` detail to human output |
| `--state-dir <DIR>` | `$XDG_CACHE_HOME/slipfork` or `~/.cache/slipfork` | Subvolumes, worlds, caches; the client socket is always `<state_dir>/run/daemon.sock` |
| `-h, --help` / `-V, --version` | — | Help or version |

`--json` is global and precedes the verb (`slipfork --json status`). A failure under `--json` prints `{"ok": false, "code": "...", "error": "...", "next": "..."}` and exits `1`; human failures print `error:` plus a `next:` remedy.

## Commands

| Command | Behaviour |
|---|---|
| `status` | Fleet tree across running worlds; the default for bare `slipfork` (`○ No worlds running`, exit `0`) |
| `ps` | The same tree scoped to one world (`cwd` by default) |
| `up` | Boot or confirm a world; streams `[step/total]` phases and the origin |
| `restart` | Stop the world's components and services, then re-run the boot pipeline in place |
| `down` | Stop a world's services while keeping its worktree (idempotent) |
| `fork` | Create `.worktrees/<fork>` plus its CoW world; `[FORK]` defaults to the current branch |
| `drop` | Destroy a fork world and its `.worktrees/<fork>` checkout |
| `prune` | Preview worlds whose checkout is gone; `--yes` reaps them |
| `open` | Open the world's origin in the browser; `--remote` prints an SSH forward recipe |
| `exec` | Run a command non-interactively inside a service container |
| `run` | Run a `Forkfile` task inside its declared service |
| `env` | Print one service's resolved environment |
| `shell` | Enter a service container interactively (requires a TTY) |
| `logs` | Read or follow retained service and hook logs |
| `hook` | Re-run one `<service>:<phase>` hook |
| `db` / `redis` | `psql` / `redis-cli` on a TTY, the world URI on a pipe |
| `forward` | Bind `127.0.0.1:0` listeners bridged into a world until Ctrl-C |
| `secret` | Manage the project's local `0600` vault |
| `migrate` | Convert an upstream Snow manifest into a `Forkfile` (one-way, statically verified) |
| `lsp` | Serve `Forkfile` diagnostics over stdio |
| `auth` | Pair this machine with the daemon's OSPP endpoint |
| `daemon` | Render or install the `:80` units and the AppArmor profile |
| `doctor` | Probe Btrfs, the live subvolume round-trip, and unprivileged namespaces |

## World selection

Most world verbs accept `--world <WORLD>`: a bare `<fork>`, the `<project>:<fork>` id, or the canonical `<project>`.

| Option | Applies to | Description |
|---|---|---|
| `--world <W>` | Most world verbs | `<fork>`, `<project>:<fork>`, or `<project>`; a foreign project prefix fails closed |
| `-F, --features <f1,f2>` | `up`, `restart`, `fork`, `env` | Extra `Forkfile` features for this invocation |
| `--no-default-features` | `up`, `restart`, `fork`, `env` | Drop the manifest's `default` feature set |
| `--no-dump` / `--refresh-dump` | `up`, `fork` | Seed-dump policy; see [seed dumps](#seed-dumps-and-sealed-bases) |
| `--resources local` | `up`, `fork` | Force hermetic world-local resources |
| `--host-caches` | `up`, `fork` | Read-only Cargo/rustup/uv/pnpm mounts after an ABI probe |
| `--base <REV>` | `fork` | Commit-ish to fork from (default `HEAD`) |
| `-s, --service <SVC>` | `exec`, `env` | Service whose container runs the command |
| `--cwd <DIR>` | `exec` | Directory used to resolve and enter the world |
| `-r, --remote`, `-p <PORT>` | `open` | Print the SSH recipe; laptop-side port (default `8080`) |
| `-f, --force` | `drop`, `migrate` | Drop a dirty worktree; replace an existing `Forkfile` |

`--world` is omitted in almost every workflow. `cwd` inside `.worktrees/<fork>` selects that fork; the repository root selects the canonical world.

## Per-command flags

| Command | Flags |
|---|---|
| `up` | `-w`, `--no-dump`/`--refresh-dump`, `--resources local`, `-F`, `--no-default-features`, `--host-caches` |
| `fork` | `[FORK]`, `--base`, `--no-dump`/`--refresh-dump`, `--resources local`, `-F`, `--no-default-features`, `--host-caches` |
| `restart` | `-w`, `-F`, `--no-default-features` |
| `down`, `ps` | `-w` |
| `drop` | `[FORK]`, `-f/--force` |
| `prune` | `--dry-run`, `-y/--yes` |
| `exec` | `-w`, `-s/--service`, `--cwd` |
| `shell` | `[SERVICE]`, `-w` |
| `logs` | `[TARGET]`, `-w`, `-f/--follow`, `--stderr`, `-n/--lines` |
| `hook run` | `<service>:<phase>`, `-w`, `--no-tty` |
| `db` / `redis` | `[ID]`, `-w` |
| `env` | `-s`, `--reveal`, `-F`, `--no-default-features`, `-w` |
| `run` | `<TASK>`, trailing args (single-command tasks only), `-w` |
| `forward` | `[TARGET...]`, `-w` |
| `migrate` | `[FILE]`, `-o/--output`, `--stdout`, `-f/--force` |
| `secret set` | `<KEY>`, `--force` |
| `daemon unit` | `--only socket\|service\|apparmor`, `--forward-port <PORT>`, `--user <ACCOUNT>`, repeatable `--bin-dir <DIR>` |
| `daemon install` | `--user <ACCOUNT>`, repeatable `--bin-dir <DIR>`, `--forward-port <PORT>` |

## Inside a world

```console
$ slipfork exec -s api -- env | grep DATABASE_URL
$ slipfork exec --cwd .worktrees/feat-billing -- pytest -q
$ slipfork shell api
$ slipfork run <task>
$ slipfork env -s api                  # [REDACTED] for secret-tainted values
$ slipfork logs api -f
$ slipfork db demo
```

`exec`, `shell`, `db`, `redis`, `hook run`, and `run` mirror the child's exit code. `shell` requires a TTY; `exec` and `db` work non-interactively and fail closed with `ServiceNotRunning` when that container is stopped.

`db` and `redis` accept the declared database or cluster id. With exactly one declaration the id may be omitted, and on a pipe they print the world's URI instead of starting a client.

## `slipfork secret` (local vault)

`slipfork secret` manages the local tier only: one `0600` `<data_home>/slipfork/vaults/<project>.json` per project, moved only by `$XDG_DATA_HOME`.

It holds `secret(<KEY>)` values that must outlive a daemon restart: app secrets, passwords for databases you run yourself, and every generated `secret(KEY) ?? <generator>` value. CI-resolved secrets are the other tier ([two tiers](ospp.md#two-tiers)).

```console
$ printf '%s' "$PGPASSWORD" | slipfork secret set PG_PASSWORD
[secret] stored PG_PASSWORD (value never printed)
$ slipfork secret list
PG_PASSWORD
$ slipfork secret unset PG_PASSWORD
```

`set` reads stdin, never echoes the value, and refuses to replace an existing key without `--force`. `list` prints key names only. Every `secret` verb needs a running `slipforkd`.

## `slipfork daemon install`

`slipfork daemon install` is the single install path for port-less `.localhost` origins, because only PID 1 `systemd` may bind `:80`:

```console
$ sudo slipfork daemon install
wrote     /etc/systemd/system/slipforkd.socket
wrote     /etc/systemd/system/slipforkd.service
wrote     /etc/apparmor.d/slipfork
enabled   slipforkd.socket + slipforkd.service as acme, ingress 127.0.0.1:80 + [::1]:80
next      slipfork up
```

It writes the exact bytes `daemon unit` renders, reloads systemd, loads the AppArmor profile, and runs `systemctl enable --now slipforkd.socket slipforkd.service`. A file is rewritten only when its bytes differ.

Running it without root fails closed and changes nothing. `--user <ACCOUNT>` names the account `slipforkd.service` runs as; `--user root` fails closed. There is no `--force` and no user-unit alternative.

## `slipfork daemon unit`

Renders to stdout with zero filesystem writes. For `slipfork` in `/usr/local/bin`, `--only apparmor` prints:

```text
abi <abi/4.0>,
profile slipfork /{usr/local/bin/slipfork{,d,-world-init},usr/bin/unshare,.slipfork/bin/crun} flags=(unconfined) {
  userns,
}
```

The brace expression names every executable that performs `CLONE_NEWUSER`. Pass `--bin-dir` once per install location; `--user` pins the rendered `User=`.

## Seed dumps and sealed bases

A paired OSPP session can offer one PostgreSQL seed dump per database. `up` and `fork` stream it once, decrypt it in `slipforkd` memory, and restore it into a short-lived seed world.

The restored result is sealed as a read-only `<project>-base-<digest>` snapshot. Every later `up` or `fork` clones that base and never re-runs `pg_restore`; no dump byte is written to host disk.

| Flag | Behaviour |
|---|---|
| *(default)* | Reuse the sealed base, else stream each requested dump once while sealing it |
| `--no-dump` | Seal a clean `initdb` base and stream nothing |
| `--refresh-dump` | Require a live paired session and seed from a dump |
| `--resources local` | Force world-local resources |

Unpaired, `--refresh-dump` fails closed with `requires an authenticated OSPP session`.

## `slipfork migrate`

```console
$ slipfork migrate                     # ./snow.yml (else ./snow.yaml) -> ./Forkfile
✓ Migrated snow.yml → Forkfile (2 services, 1 postgres cluster, 2 env groups)
$ slipfork migrate ../billing/snow.yml -o billing/Forkfile
$ slipfork migrate --stdout            # print the conversion, write nothing
```

One command, one direction: an upstream Snow manifest in, a statically valid `Forkfile` out. `migrate` re-parses its own output with the whole-file checker, so a file it writes always parses. A refusal prints one `error:` line plus a `next:` remedy.

| Flag | Behaviour |
|---|---|
| `[FILE]` | Upstream manifest; defaults to `./snow.yml`, then `./snow.yaml` |
| `-o, --output <PATH>` | Destination; defaults to `./Forkfile` |
| `--stdout` | Print the converted text and write nothing (`--json` prints it as one JSON string) |
| `-f, --force` | Replace an existing `Forkfile` |

`migrate`, `doctor`, and `lsp` run locally and need no `slipforkd`. Under `--json` the write reports counts instead of the summary line:

```console
$ slipfork --json migrate
{"env_groups":2,"input":"snow.yml","ok":true,"output":"Forkfile","pg_clusters":1,"services":2}
```

Failures keep the standard shape with a stable `code`: `missing_input`, `output_exists`, `unknown_key`, `invalid_manifest`, `no_cwd`, or `write_failed`.

## `slipfork lsp`

`lsp` serves the same whole-file diagnostics plus hover documentation over stdio for Zed, Neovim, and VS Code. Your editor's LSP client starts it; you do not run it by hand.

## `git-remote-slipfork`

Git invokes this helper for a `slipfork://<fork>` remote. It lists and fetches the fork's branch and `refs/slipfork/<fork>/wip` from the running daemon:

```console
$ git remote add fork slipfork://feat-billing && git fetch fork
git-remote-slipfork: world 'demo:feat-billing' is not running in this slipforkd; run `slipfork up` first
```

Next: [getting started](../getting-started.md) for the end-to-end flow, or the [`Forkfile` reference](forkfile.md) for the manifest.
