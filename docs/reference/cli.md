# CLI Reference (`0.1.0`)

`slipfork` is a thin client: `doctor` and `dag plan` read the host locally, `snow resolve` asks `slipforkd` (it owns the vault), and every world command talks to the unprivileged `slipforkd` daemon over a `0600` `<state_dir>/run/daemon.sock`. Only `up` and `fork` start `slipforkd --listen 127.0.0.1:0` in the background (`<state_dir>/run/daemon.log`, `0600`); every other verb fails closed with one line when nothing answers:
```console
$ slipfork list
slipfork: no slipforkd on /home/acme/.cache/slipfork/run/daemon.sock — run `slipfork up`
```

```console
$ slipfork doctor
state_dir: /home/acme/.cache/slipfork
filesystem: xfs
btrfs_super_magic: mismatch
subvolume_ioctl: skipped
noatime: missing
unshare_userns: ok
netns: ok
ingress: not installed (ephemeral port; run `slipfork daemon install`)
fix: sudo mount -o user_subvol_rm_allowed,noatime /dev/<device> <state_dir>
```

`doctor` runs **before** any daemon and never needs one: it exits `0` only when `btrfs_super_magic` and `subvolume_ioctl` are `ok` and both namespace probes executed, and `1` otherwise, printing one `fix:` line per failed check in `btrfs -> unshare_userns -> netns` order (replace `<device>` with your Btrfs device and `<state_dir>` with the `state_dir:` line above). On a Btrfs host mounted `user_subvol_rm_allowed,noatime` the report reads `filesystem: btrfs`, `btrfs_super_magic: ok`, `subvolume_ioctl: ok`, `noatime: ok`, with no `fix:` line.

`--json` prints the same report as one object, keyed exactly as follows (values below are from the non-Btrfs run above):

| Key | Meaning |
|---|---|
| `"state_dir"`, `"probed_path"` | Requested state root, and the existing directory actually `statfs`ed (nearest ancestor when the root does not exist yet) |
| `"fstype": "0x58465342"`, `"filesystem": "xfs"` | Raw `statfs` type as `"0x<lowercase-hex>"`, plus its label (`btrfs`, `xfs`, `ext4`, `tmpfs`, `overlayfs`, `unknown(0x…)`) |
| `"btrfs_super_magic": false` | `true` only for `BTRFS_SUPER_MAGIC` (`0x9123683e`) |
| `"subvolume_ioctl": {"status": "skipped"}` | `ok` after a live scratch create → read-only snapshot → clear read-only → destroy; `skipped` off Btrfs; `failed` adds `errno` and the mount `hint` |
| `"noatime": false` | Advisory `ST_NOATIME` evidence; never affects `ok` |
| `"unshare_userns": {"status": "ok"}`, `"netns": {"status": "ok"}` | `ok` or `failed` (+ `detail`, + `hint`); `netns` is `skipped` when `unshare_userns` failed |
| `"ingress": {"status": "active", "port": 80, "listen": ["127.0.0.1:80", "[::1]:80"], "hint": null}` | Advisory `:80` systemd socket activation. `active` carries `port: 80`; `active_other_listeners` carries `port: null`, `listen: [...]` (or `null` when `systemctl show -p Listen` failed) and a non-null `hint`; `inactive` and `not_installed` carry `port: null`, `listen: null`, and a non-null `hint`. Never affects `ok` |
| `"uid": 1000`, `"gid": 1000` | The invoking identity every world runs as |
| `"ok": false`, `"fixes": [...]` | `ok` is the four required checks; `fixes` is empty exactly when `ok` is `true` |

## Global options

| Option | Default | Description |
|---|---|---|
| `--json` | `false` | Emit JSON on stdout |
| `--state-dir <DIR>` | `$XDG_CACHE_HOME/slipfork` or `~/.cache/slipfork` | Btrfs subvolumes, worlds, `cache/{rootfs,oci,runtime}`; the client's control socket is always `<state_dir>/run/daemon.sock` |
| `-h, --help` / `-V, --version` | — | Help or CLI version |

`--json` is global (`slipfork --json list`). Human diagnostics go to stderr; with `--json`, a failure prints `{"ok": false, "error": "..."}` on stdout. Exit `0` is success, `1` a `slipfork:` runtime error, and `2` a `clap` usage error. `exec`, `shell`, `db`, `redis`, and `hook run` instead exit with the child command's status (so `slipfork exec -- false` exits `1` with no `slipfork:` line), and their child stdout is not JSON even under `--json`.

## Commands

| Command | Behaviour | Example |
|---|---|---|
| `doctor` | Probe the Btrfs substrate, the live subvolume round-trip, and unprivileged userns/netns | `slipfork doctor` |
| `snow resolve` | Validate `snow.yml`, persist missing `!!` secrets, print one mode (`[REDACTED]`) | `slipfork snow resolve --file snow.yml --mode local` |
| `dag plan` | Merkle-hash inputs and print `Cached` / `Rebuild` per artifact and fixture wave | `slipfork dag plan` |
| `up` | Boot or confirm the world selected by `--world` or `cwd`; print one origin URL | `slipfork up` |
| `down` | Stop a world's services, keep its worktree (idempotent) | `slipfork down --world feat-billing` |
| `fork` | Create `.worktrees/<fork>` — a Git worktree, or a Jujutsu workspace when the checkout carries `.jj` — plus its Btrfs world subvolume | `slipfork fork feat-billing` |
| `drop` | Destroy a fork world's subvolume, its worktree, branch, and snapshot ref | `slipfork drop feat-billing` |
| `list` | One row per world: state (`running`/`stopped`/`failed`), exactly one origin URL, worktree | `slipfork list` |
| `prune` | Preview worlds whose checkout directory is gone (default, or `--dry-run`); `--yes` reaps them. Sealed snapshots and caches are retained | `slipfork prune` |
| `secret set` / `unset` | Store a value read from **stdin** (`--force` rotates it) or remove one key | `printf '%s' "$V" \| slipfork secret set STRIPE_SECRET_KEY` |
| `secret list` | Print key names only, never values | `slipfork secret list` |
| `share` | Tunnel sharing is unavailable in this release and fails closed; `--stop` releases a lease | `slipfork share` |
| `daemon unit` | Render the non-template systemd units and the AppArmor profile on stdout (`--only socket\|service\|apparmor`, repeatable `--bin-dir`) | `slipfork daemon unit --only apparmor` |
| `daemon install` | Write `/etc/systemd/system/slipforkd.{socket,service}` + `/etc/apparmor.d/slipfork`, then `enable --now` (`sudo`) | `sudo slipfork daemon install` |
| `exec` | Run a command non-interactively inside a world | `slipfork exec -- id` |
| `shell` | Enter a world interactively (requires a TTY) | `slipfork shell` |
| `logs` | Read or follow retained component/hook logs (`--follow`, `--lines`, `--stderr`) | `slipfork logs --lines 3 api` |
| `hook run` | Re-run one `<component>:<phase>` hook (`--no-tty` when not interactive) | `slipfork hook run api:pre-deploy --no-tty` |
| `db` | Open `psql` inside the world's running `postgres:<v>` container (TTY) | `slipfork db appdb` |
| `redis` | Open `redis-cli` inside the world's running `redis:<v>` container (TTY) | `slipfork redis cache` |
| `forward` | Bind daemon-owned `127.0.0.1:0` listeners bridged into a world, held until Ctrl-C | `slipfork forward pg_databases.appdb` |
| `auth login` | Pair an OSPP device key from a `#handle=` URL (`-` reads stdin) | `slipfork auth login -` |
| `support` | Report registry tiers and required evidence (`ADR-0007`) | `slipfork support` |

## World selection and boot options

`up`, `down`, `share`, `exec`, `shell`, `logs`, `hook run`, `db`, `redis`, and `forward` accept `--world <WORLD>`: a bare `<fork>`, the `<project>:<fork>` id `slipfork list` prints, or the canonical `<project>`. `--world` only picks which world of the current project to use, so run these from inside that project's Git/JJ checkout; omitted, the world is inferred from `cwd`: `.worktrees/<fork>[/...]` selects fork `<fork>`, the repository root selects the canonical world.

| Option | Applies to | Description |
|---|---|---|
| `--world <WORLD>` | `up`, `down`, `share`, `exec`, `shell`, `logs`, `hook run`, `db`, `redis`, `forward` | `<fork>`, `<project>:<fork>`, or canonical `<project>`; a foreign project prefix fails closed |
| `--base <REV>` | `fork` | Git checkouts: commit-ish to fork from (default: requesting checkout's `HEAD`). A `.jj` checkout always forks the working-copy parent `@-` (`@` when dirty) and ignores `--base` |
| `--no-dump` / `--refresh-dump` | `up`, `fork` | Seal a clean `initdb` base / re-stream every offered dump into a new base (fails closed unpaired or offline) |
| `--resources local` | `up`, `fork` | Force hermetic world-local resources |
| `--host-caches` | `up`, `fork` | Mount `~/.cargo/{registry,git,bin}`, `~/.rustup`, `~/.cache/uv`, `~/.local/share/uv`, and `~/.local/share/pnpm/store` read-only **after** the ELF/libc ABI probe passes; never a `slipfork.toml` key |
| `--cwd <DIR>` | `exec` | Directory used to resolve and enter the world |

`--host-caches` fails closed naming both ABIs when the host and world architectures, libc family, or glibc versions disagree.

| Command | Command-specific flags |
|---|---|
| `snow resolve` | `--file <FILE>` (default `snow.yml`), `--mode <MODE>` (default `local`), `--component <ID>`, `--expose-secrets` |
| `dag plan` | `--file <FILE>` (default `<vcs_root>/slipfork.toml`) |
| `logs` | `--follow`, `--lines <N>`, `--stderr`, optional target (`api`, `api:pre-deploy`) |
| `hook run` | `--no-tty` (run through the daemon instead of a native TTY) |
| `forward` | positional targets (`pg_databases.main`, `components.api`); no target forwards all |
| `daemon unit` | `--only socket` / `--only service` / `--only apparmor`, `--forward-port <PORT>` (default `7080`), repeatable `--bin-dir <DIR>` (default: this executable's directory) |
| `daemon install` | `--user <ACCOUNT>` (default: the invoking `sudo` account), repeatable `--bin-dir <DIR>`, `--forward-port <PORT>` (default `7080`); no `--force` |
| `secret set` | `--force` rotates an existing value |

## Failure modes you will actually see

```console
$ slipfork list
slipfork: no slipforkd on /home/acme/.cache/slipfork/run/daemon.sock — run `slipfork up`
```

- A non-Btrfs state directory fails closed at `slipforkd` boot: `daemon.log` names the filesystem code and the `mount -o user_subvol_rm_allowed,noatime` line to run, and nothing is created.
- `share` fails closed until tunnel sharing lands (`slipfork: slipforkd has no forward listener, …`), and a forward listener alone does not change that; `share --stop` on a local world is an idempotent success.
- `db`/`redis` fail closed with `ServiceNotRunning` unless that service's container is running in the world.
- `prune` reaps orphaned worlds; it does not garbage-collect sealed snapshots or `<state_dir>/cache/{rootfs,oci}` yet.

## Seed dumps and sealed bases

A paired OSPP session can offer one PostgreSQL seed dump per database; `up`/`fork` then stream it once over `OSPP v1` into `<state_dir>/dumps/<sha256>.pgdump`, decompress it to `<sha256>.pgcustom`, `pg_restore --no-owner --no-privileges --clean --if-exists` + `CHECKPOINT` it inside a short-lived seed world's `postgres:<v>` container, and seal the result as a read-only `<project>-base-<digest>` Btrfs snapshot keyed by the service versions and every dump digest.

Every later `up`/`fork` clones that sealed base and never re-runs `pg_restore`; a deleted `.pgcustom` only means decompressing the cached `.pgdump` again.

| Flag | Behaviour |
|---|---|
| *(default)* | Reuse a cached dump, else stream it once; restore into the sealed base |
| `--no-dump` | Seal a clean `initdb` base and stream nothing; a dump-backed base is a distinct digest |
| `--refresh-dump` | Re-stream every offered dump and seal a new base; fails closed without a live paired session |
| `--resources local` | Force hermetic world-local resources; implies `--no-dump` and cannot combine with `--refresh-dump` |

Unpaired, `local = "remote"` on a database and `--refresh-dump` both fail closed with `requires a remote OSPP offer`; `prefer_remote` degrades to local with a `not_offered` row. No OSPP v1 provider ships in `0.1.0`; this flow requires a compatible provider.

## `slipforkd`

`slipforkd` is the sole stateful Composition Root and runs unprivileged. It requires explicit paths and never guesses them, runs in the foreground, logs to stderr, and is started for you by `slipfork up` / `slipfork fork` (or by `slipforkd.socket` after `slipfork daemon install`). Every other verb fails closed instead of spawning one.

| Option | Description |
|---|---|
| `--state-dir <DIR>` | Btrfs subvolumes, world clones, `cache/{rootfs,oci,runtime}` |
| `--runtime-dir <DIR>` | `0600` `daemon.sock`; a `slipforkd`-only flag (clients always use `<state_dir>/run`) |
| `--listen <ADDR>` | HTTP ingress (`127.0.0.1:0` for zero-setup); ignored when `LISTEN_FDS` is set |
| `--forward-port <PORT>` | Also bind `127.0.0.1:<PORT>` as the static forward listener |
| `--world-init <PATH>` / `--crun <PATH>` | Explicit helper and pinned static `crun` binaries |

With `LISTEN_PID`/`LISTEN_FDS` present, `slipforkd` adopts inherited descriptors by fd order and fails closed on any mismatch.

## `slipfork daemon install`

`slipfork daemon install` is the **single** install path for port-less `.localhost` origins. `systemd` (PID 1) is the only component allowed to bind `127.0.0.1:80` / `[::1]:80`, so it installs and enables the system socket:

```console
$ sudo slipfork daemon install
wrote     /etc/systemd/system/slipforkd.socket
wrote     /etc/systemd/system/slipforkd.service
wrote     /etc/apparmor.d/slipfork
enabled   slipforkd.socket + slipforkd.service as acme, ingress 127.0.0.1:80 + [::1]:80
next      slipfork up
```

It writes the **exact bytes** `slipfork daemon unit` renders, reloads `systemd`, loads the AppArmor profile with `apparmor_parser -r`, and runs `systemctl enable --now slipforkd.socket slipforkd.service`, after releasing any zero-setup `slipforkd` that still holds `daemon.sock`. A file is rewritten only when its bytes differ, so a second run performs zero writes; if a run fails halfway, re-running the same command converges (every write is idempotent and `enable --now` is safe to repeat).

```console
$ sudo slipfork daemon install
unchanged slipforkd.socket, slipforkd.service, /etc/apparmor.d/slipfork; enabled
```

Running it without root fails closed and changes nothing:

```console
$ slipfork daemon install
slipfork: daemon install writes /etc and enables system units; run it as root (sudo slipfork daemon install --user acme)
```

`--user <ACCOUNT>` names the unprivileged account `slipforkd.service` runs as; omitted, it is the account behind `sudo` (`SUDO_USER`). `--user root`, an unknown account, or direct root with no valid invoking account fails closed. There is no `--force` and no user-unit alternative.

## `slipfork daemon unit`

Renders to stdout with zero filesystem writes (`User=<your-account>`, empty `CapabilityBoundingSet=`, `ListenStream=127.0.0.1:80`, `[::1]:80`, `127.0.0.1:7080`, `Requires=slipforkd.socket`, `After=slipforkd.socket`, `WantedBy=multi-user.target`). `--bin-dir <DIR>` is repeatable (default: this executable's directory) and feeds the AppArmor profile: each directory becomes one flat alternative inside the single four-line brace expression. `--user <ACCOUNT>` pins the rendered `User=` (and the account home the state/runtime paths derive from), so `slipfork daemon unit --user <ACCOUNT> --bin-dir <DIR>` reproduces exactly what `sudo slipfork daemon install --user <ACCOUNT> --bin-dir <DIR>` writes.

`--only apparmor` renders the `/etc/apparmor.d/slipfork` profile instead. Ubuntu 24.04 LTS enables `kernel.apparmor_restrict_unprivileged_userns=1`, so unprivileged `CLONE_NEWUSER` is denied (`doctor` reports `unshare_userns: failed` and `netns: skipped`) until that profile is installed and loaded by `slipfork daemon install`.

For `slipfork` in `/usr/local/bin` it prints exactly:

```text
abi <abi/4.0>,
profile slipfork /{usr/local/bin/slipfork{,d,-world-init},usr/bin/unshare,.slipfork/bin/crun} flags=(unconfined) {
  userns,
}
```

AppArmor 4.0 accepts one attachment expression per profile header, so that single leading-slash brace expression names every executable that performs `CLONE_NEWUSER` for a world: `<bin_dir>/slipfork`, `<bin_dir>/slipforkd`, `<bin_dir>/slipfork-world-init`, host `/usr/bin/unshare` (outer world namespaces and `doctor`), and in-world `/.slipfork/bin/crun` (inner OCI namespaces). Pass `--bin-dir` once per install location on the host; the body is `userns,` only, so file and mount mediation stay inactive.

## `git-remote-slipfork`

`git` invokes this helper for a `slipfork://<fork>` remote: it lists and fetches the fork's branch and `refs/slipfork/<fork>/wip` from the running daemon, so another checkout or agent can pull a fork's work. The world must be running in the same `slipforkd`:

```console
$ git remote add fork slipfork://feat-billing && git fetch fork
git-remote-slipfork: world 'acme:feat-billing' is not running in this slipforkd; run `slipfork up` first
```

Next: [getting started](../getting-started.md) for the end-to-end flow, or the [manifest reference](manifest.md) for both schemas.
