# CLI Reference (`0.1.0`)

`slipfork` is a thin client. `doctor` reads the host locally; every world command and every secret operation talks to the unprivileged `slipforkd` over a `0600` `<state_dir>/run/daemon.sock`.

```console
$ slipfork                 # bare command = status: ingress, worlds, next steps
$ slipfork up              # boot the world selected by --world or cwd
$ slipfork list            # one row per world, one origin URL each
```

Only `up` and `fork` start `slipforkd --listen 127.0.0.1:0` when nothing answers. Every other verb fails closed with one line:

```console
$ slipfork list
slipfork: no slipforkd on /home/acme/.cache/slipfork/run/daemon.sock — run `slipfork up`
```

!!! info "Port-less by default"
    After `sudo slipfork daemon install`, systemd owns `127.0.0.1:80` and `[::1]:80`, so origins are `http://acme.localhost` with no port. Without it, the daemon uses an ephemeral port and prints a warning.

## `slipfork doctor`

```console
$ slipfork doctor
✗ 1 issue
  state_dir: /home/acme/.cache/slipfork
  filesystem: xfs
  btrfs_super_magic: mismatch
  subvolume_ioctl: skipped
  unshare_userns: ok
  netns: ok
  noatime: missing
  ingress: not installed (ephemeral port; run `sudo slipfork daemon install`)
  next: sudo mount -o user_subvol_rm_allowed,noatime /dev/<device> /home/acme/.cache/slipfork
```

`doctor` runs before any daemon and never needs one. It exits `0` only when the Btrfs checks and both namespace probes pass, and `1` otherwise with one `next:` line per failed check.

`--json` prints the same report as one object:

| Key | Meaning |
|---|---|
| `"state_dir"`, `"probed_path"` | Requested state root, and the existing directory actually `statfs`ed |
| `"fstype"`, `"filesystem"` | Raw `statfs` type as `"0x<hex>"` plus a label (`btrfs`, `xfs`, `ext4`, `tmpfs`, `overlayfs`) |
| `"btrfs_super_magic"` | `true` only for `BTRFS_SUPER_MAGIC` (`0x9123683e`) |
| `"subvolume_ioctl"` | `ok` after a live create → read-only snapshot → destroy round-trip; `skipped` off Btrfs; `failed` adds `errno` and a hint |
| `"noatime"` | Advisory `ST_NOATIME` evidence; never affects `ok` |
| `"unshare_userns"`, `"netns"` | `ok`, `failed` (+ `detail`, `hint`), or `skipped` when the parent probe failed |
| `"ingress"` | Advisory `:80` socket state: `active`, `active_other_listeners`, `inactive`, or `not_installed`; never affects `ok` |
| `"uid"`, `"gid"` | The invoking identity every world runs as |
| `"ok"`, `"fixes"` | `ok` is the required checks; `fixes` is empty exactly when `ok` is `true` |

## Global options

| Option | Default | Description |
|---|---|---|
| `--json` | `false` | Emit JSON on stdout |
| `-v, --verbose` | `false` | Add lineage and `[resources]` detail to human output |
| `--state-dir <DIR>` | `$XDG_CACHE_HOME/slipfork` or `~/.cache/slipfork` | Subvolumes, worlds, caches; the client socket is always `<state_dir>/run/daemon.sock` |
| `-h, --help` / `-V, --version` | — | Help or version |

`--json` is global (`slipfork --json list`). Human diagnostics go to stderr; a failure under `--json` prints `{"ok": false, "error": "..."}`.

Exit `0` is success, `1` a runtime error, and `2` a usage error. `exec`, `shell`, `db`, `redis`, and `hook run` exit with the child command's status instead.

## Commands

| Command | Behaviour | Example |
|---|---|---|
| `status` | Daemon ingress, every world's tree, lineage, and next steps; the default for bare `slipfork` | `slipfork status` |
| `doctor` | Probe Btrfs, the live subvolume round-trip, and unprivileged namespaces | `slipfork doctor` |
| `snow resolve` | Validate `snow.yml`, persist missing `!!` secrets, print one mode | `slipfork snow resolve --mode local` |
| `up` | Boot or confirm the world selected by `--world` or `cwd` | `slipfork up` |
| `down` | Stop a world's services, keep its worktree (idempotent) | `slipfork down --world feat-billing` |
| `restart` | Stop components and services, then re-run the boot pipeline in place | `slipfork restart` |
| `ps` | One world's state, lineage, and every component / `crun` service row | `slipfork ps` |
| `open` | Open the world's origin in the desktop browser; `--remote` prints an SSH forward recipe | `slipfork open --remote` |
| `fork` | Create `.worktrees/<fork>` plus its Btrfs world; `[FORK]` defaults to the current branch | `slipfork fork feat-billing` |
| `drop` | Destroy a fork world, its worktree, branch, and snapshot ref | `slipfork drop feat-billing` |
| `list` | One row per world: state (`running`/`stopped`/`failed`), one URL, worktree | `slipfork list` |
| `prune` | Preview worlds whose checkout is gone; `--yes` reaps them | `slipfork prune --yes` |
| `secret set` / `unset` | Store a local-vault value read from stdin (`--force` rotates) or remove one key | `slipfork secret set PG_PASSWORD` |
| `secret list` | Print local-vault key names only, never values | `slipfork secret list` |
| `daemon unit` | Render units and the AppArmor profile on stdout | `slipfork daemon unit --only apparmor` |
| `daemon install` | Write `/etc/systemd/system/slipforkd.{socket,service}` plus the AppArmor profile, then `enable --now` | `sudo slipfork daemon install` |
| `exec` | Run a command non-interactively inside a world | `slipfork exec -- id` |
| `shell` | Enter a world interactively (requires a TTY) | `slipfork shell` |
| `logs` | Read or follow retained component and hook logs | `slipfork logs --lines 3 api` |
| `hook run` | Re-run one `<component>:<phase>` hook (`--no-tty` when not interactive) | `slipfork hook run api:pre-deploy --no-tty` |
| `db` | Open `psql` inside the world's running `postgres:<v>` container (TTY) | `slipfork db appdb` |
| `redis` | Open `redis-cli` inside the world's running `redis:<v>` container (TTY) | `slipfork redis cache` |
| `forward` | Bind daemon-owned `127.0.0.1:0` listeners bridged into a world until Ctrl-C | `slipfork forward pg_databases.appdb` |
| `auth [ENDPOINT]` | Pair this machine with the daemon's OSPP endpoint and print the server-supplied page | `slipfork auth` |

## World selection

`up`, `down`, `restart`, `ps`, `open`, `exec`, `shell`, `logs`, `hook run`, `db`, `redis`, and `forward` accept `--world <WORLD>`: a bare `<fork>`, the `<project>:<fork>` id from `slipfork list`, or the canonical `<project>`.

`--world` only picks which world of the current project to use, so run these from that project's checkout. Omitted, the world is inferred from `cwd`: `.worktrees/<fork>[/...]` selects that fork; the repository root selects the canonical world.

| Option | Applies to | Description |
|---|---|---|
| `--world <WORLD>` | Most world verbs | `<fork>`, `<project>:<fork>`, or canonical `<project>`; a foreign project prefix fails closed |
| `--base <REV>` | `fork` | Git commit-ish to fork from (default: `HEAD`); a `.jj` checkout ignores it |
| `--no-dump` / `--refresh-dump` | `up`, `fork` | Seed-dump policy; see [seed dumps](#seed-dumps-and-sealed-bases) |
| `--host-caches` | `up`, `fork` | Read-only mounts of Cargo, rustup, uv, and pnpm caches after an ABI probe; never a manifest key |
| `--cwd <DIR>` | `exec` | Directory used to resolve and enter the world |
| `--remote` | `open` | Print the SSH local-forward recipe instead of opening a browser |

| Command | Command-specific flags |
|---|---|
| `snow resolve` | `--file <FILE>` (default `snow.yml`), `--mode <MODE>` (default `local`), `--component <ID>`, `--expose-secrets` |
| `logs` | `--follow`, `--lines <N>`, `--stderr`, optional target (`api`, `api:pre-deploy`) |
| `hook run` | `--no-tty` |
| `forward` | positional targets (`pg_databases.main`, `components.api`); empty forwards everything |
| `daemon unit` | `--only socket` / `--only service` / `--only apparmor`, `--forward-port <PORT>` (default `7080`), repeatable `--bin-dir <DIR>` |
| `daemon install` | `--user <ACCOUNT>` (default: the invoking `sudo` account), repeatable `--bin-dir <DIR>`, `--forward-port <PORT>` (default `7080`); no `--force` |
| `secret set` | `--force` rotates an existing value |

## `slipfork secret` (local vault)

`slipfork secret` manages the local tier only: one `0600` `<data_home>/slipfork/vaults/<project>.json` per project, moved only by `$XDG_DATA_HOME`.

It holds `secrets::<KEY>` values that must outlive a daemon restart: app secrets, passwords for databases you run yourself, and every local-mode `!!` value. CI-resolved secrets are the other tier ([two tiers](ospp.md#two-tiers)).

```console
$ printf '%s' "$PGPASSWORD" | slipfork secret set PG_PASSWORD
[secret] stored secrets::PG_PASSWORD (value never printed)
$ slipfork secret list
PG_PASSWORD
```

`set` reads stdin only, refuses an interactive terminal, never echoes the value, and refuses to replace a key without `--force`. `list` prints key names. `vars::` keys are read-only here. Like `snow resolve`, every `secret` verb needs a running `slipforkd`.

## Failure modes you will see

- A non-Btrfs state directory fails at daemon boot: `daemon.log` names the filesystem code and the `mount -o user_subvol_rm_allowed,noatime` line, and nothing is created.
- `db`/`redis` fail closed with `ServiceNotRunning` unless that service's container is running in the world.
- `prune` reaps orphaned worlds; it does not garbage-collect sealed snapshots or `<state_dir>/cache/{rootfs,oci}` yet.

## Seed dumps and sealed bases

A paired OSPP session can offer one PostgreSQL seed dump per database. `up`/`fork` stream it once, decrypt it in `slipforkd` memory, and pipe it into `zstd -d | pg_restore --clean --if-exists` inside a short-lived seed world.

The restored result is sealed as a read-only `<project>-base-<digest>` snapshot. Every later `up` or `fork` clones that base and never re-runs `pg_restore`. No dump byte is written to host disk.

| Flag | Behaviour |
|---|---|
| *(default)* | Reuse the sealed base, else stream each requested dump once while sealing it |
| `--no-dump` | Seal a clean `initdb` base and stream nothing |
| `--refresh-dump` | Require a live paired session and seed from a dump |
| `--resources local` | Force world-local resources; implies `--no-dump` and cannot combine with `--refresh-dump` |

Unpaired, `local = "remote"` and `--refresh-dump` fail closed with `requires an authenticated OSPP session`; `prefer_remote` degrades to local with a `no_remote_session` row.

## `slipforkd`

`slipforkd` is the sole stateful composition root and runs unprivileged. It requires explicit paths, runs in the foreground, logs to stderr, and is started by `up`/`fork` or by `slipforkd.socket`.

| Option | Description |
|---|---|
| `--state-dir <DIR>` | Subvolumes, world clones, `cache/{rootfs,oci,runtime}` |
| `--runtime-dir <DIR>` | `0600` `daemon.sock`; `slipforkd`-only (clients always use `<state_dir>/run`) |
| `--listen <ADDR>` | HTTP ingress (`127.0.0.1:0` for zero-setup); ignored with `LISTEN_FDS` |
| `--forward-port <PORT>` | Also bind `127.0.0.1:<PORT>` as a static forward listener |
| `--world-init <PATH>` / `--crun <PATH>` | Explicit helper and pinned static `crun` binaries |
| `--ospp-endpoint <URL>` | First-run OSPP server identity; omit to reuse `<state_dir>/ospp.json` |

With `LISTEN_PID`/`LISTEN_FDS` present, `slipforkd` adopts inherited descriptors by fd order and fails closed on any mismatch.

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

It writes the exact bytes `daemon unit` renders, reloads systemd, loads the AppArmor profile, and runs `systemctl enable --now slipforkd.socket slipforkd.service`. It first releases any zero-setup daemon holding `daemon.sock`.

A file is rewritten only when its bytes differ, so a second run performs zero writes. A failed half-run converges when you re-run the same command.

Running it without root fails closed and changes nothing:

```console
$ slipfork daemon install
slipfork: daemon install writes /etc and enables system units; run `sudo slipfork daemon install`
```

`--user <ACCOUNT>` names the account `slipforkd.service` runs as; omitted, it is the account behind `sudo`. `--user root`, an unknown account, or direct root with no invoking account fails closed. There is no `--force` and no user-unit alternative.

## `slipfork daemon unit`

Renders to stdout with zero filesystem writes. For `slipfork` in `/usr/local/bin`, `--only apparmor` prints:

```text
abi <abi/4.0>,
profile slipfork /{usr/local/bin/slipfork{,d,-world-init},usr/bin/unshare,.slipfork/bin/crun} flags=(unconfined) {
  userns,
}
```

The single brace expression names every executable that performs `CLONE_NEWUSER`: the three Slipfork binaries, host `/usr/bin/unshare`, and in-world `crun`. Pass `--bin-dir` once per install location.

`--only socket` and `--only service` print the units, including `Requires=slipforkd.socket`, `After=slipforkd.socket`, and `WantedBy=multi-user.target`. `--user <ACCOUNT>` pins the rendered `User=` so `daemon unit` reproduces exactly what `daemon install` writes.

## `git-remote-slipfork`

Git invokes this helper for a `slipfork://<fork>` remote. It lists and fetches the fork's branch and `refs/slipfork/<fork>/wip` from the running daemon:

```console
$ git remote add fork slipfork://feat-billing && git fetch fork
git-remote-slipfork: world 'acme:feat-billing' is not running in this slipforkd; run `slipfork up` first
```

Next: [getting started](../getting-started.md) for the end-to-end flow, or the [manifest reference](manifest.md) for both schemas.
