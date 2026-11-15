# CLI Reference (`0.1.0`)

Exact commands and flags implemented by `crates/slipfork/src/main.rs` (`slipfork --help`).

```console
$ slipfork doctor                                   # probe CoW reflink, Incus socket, unprivileged netns
$ slipfork snow resolve --file snow.yml --mode local # validate modes and persist !! secrets (0600)
$ slipfork dag plan --file slipfork.toml --repo .    # print artifact and fixture rebuild waves
$ slipfork snap --source ./pgdata canonical          # capture a CoW snapshot via ioctl(FICLONE)
```

## Global Options

| Option | Default | Description |
|---|---|---|
| `--json` | `false` | Emit JSON instead of human text |
| `--state-dir <STATE_DIR>` | `$XDG_CACHE_HOME/slipfork` or `~/.cache/slipfork` | State root for snapshots and DAG digest checks |
| `-h, --help` / `-V, --version` | — | Print help or the CLI version |

`--json` is global: `slipfork --json doctor`.

| Command | `--json` payload |
|---|---|
| `doctor` | `{"ok", "uid", "gid", "state_root", "incus_daemon_socket", "reflink_64k_micros", "isolated_netns"}` |
| `snow resolve` | a flat `{"<VAR>": "<value>"}` map (secrets stay `[REDACTED]` without `--expose-secrets`) |
| `dag plan` | `{"artifacts": [...], "fixture_waves": [[...]]}` with `"action": "cached" \| "rebuild"` |
| `snap` | `{"ok", "snapshot", "elapsed_ms"}` |

Human diagnostics go to `stderr`; with `--json`, a failure prints `{"ok": false, "error": "..."}` to **stdout**.

| Exit code | Meaning |
|---|---|
| `0` | Success |
| `1` | A `slipfork:` error (invalid manifest, missing file, non-CoW filesystem, reflink failure) |
| `2` | A daemon-owned command refusing to run, or a `clap` usage error |

---

## `slipfork doctor`

Probes the host: Incus Unix socket presence (`/var/lib/incus/unix.socket`), a 64 KiB `ioctl(FICLONE)` round-trip in `<STATE_DIR>`, and unprivileged `unshare --map-user --map-group --keep-caps --net --pid` loopback bring-up.

```console
$ slipfork doctor
state_root: /home/acme/.cache/slipfork
incus:      absent (using reflink-netns)
reflink:    ok (64 KiB FICLONE in 447 us)
netns:      ok (uid=1000:1000)
```

Exits non-zero when reflink or the isolated network namespace fails.

## `slipfork snow resolve`

Validates `snow.yml`, persists any missing `!!` secret into the local `0600` vault, and prints the resolved variables of one mode (`[REDACTED]` by default).

| Option | Default | Description |
|---|---|---|
| `--file <FILE>` | `snow.yml` | Path to `snow.yml`, relative to the current directory (unlike `dag plan`, there is no VCS-root fallback) |
| `--mode <MODE>` | `local` | Mode to resolve (`local`, `cloud`, `all`, or any declared mode) |
| `--component <COMPONENT>` | all groups | Restrict output to the environment groups of one component (an unknown id fails with `component '<id>' not found`) |
| `--expose-secrets` | `false` | Print plaintext secrets instead of `[REDACTED]` |

```console
$ slipfork snow resolve --file snow.yml --mode local
APP_URL=http://acme.localhost
DATABASE_URL=postgres://127.0.0.1:5432/appdb
DEBUG=True
LOG_LEVEL=INFO
SECRET_KEY=[REDACTED]
```

## `slipfork dag plan`

Hashes repository inputs with SHA-256 Merkle trees and prints `Cached` vs `Rebuild` for every `[artifacts.<id>]` and every topological `[fixtures.<id>]` wave. Planning only reads the digest cache, so every node reports `Rebuild` until a command materializes its digest (M3).

| Option | Default | Description |
|---|---|---|
| `--file <FILE>` | `<vcs_root>/slipfork.toml` | Path to `slipfork.toml` |
| `--repo <REPO>` | `<vcs_root>` | Repository root walked for input globs (must be the VCS root of `--file`) |

```console
$ slipfork dag plan --file slipfork.toml --repo .
artifact front            Rebuild (5fe1ef1ecec6)
wave 0 fixture migrate      Rebuild (cd305423c952)
```

## `slipfork snap --source <SOURCE> <LABEL>`

Captures `<SOURCE>` into `<STATE_DIR>/snapshots/<LABEL>` with kernel `ioctl(FICLONE)`. It fails closed with exit `1` when the filesystem cannot reflink — `errno 95` means the destination is not CoW-capable (for example `tmpfs`), `errno 18` means source and destination are on different devices:

```console
$ mkdir -p ./pgdata && echo seeded > ./pgdata/marker.txt
$ slipfork snap --source ./pgdata canonical
snapshot 'canonical' created in 0.36 ms
```

---

## Daemon-Owned Commands (`0.2.0`)

`slipfork fork`, `slipfork exec`, and `slipfork drop` are **not runnable in this tree**. They parse their flags (an invalid flag exits `1` before the refusal), then exit `2` because a world is owned by the `slipforkd` daemon and no cross-process world handle exists:

```text
slipfork fork is served by slipforkd in 0.2.0 (lands in M2.17); world 'exp' is daemon-owned and no cross-process world handle exists — run `slipfork doctor` to verify this host
```

| Command | Today | Arrives with |
|---|---|---|
| `slipfork fork <NAME> [--from <FROM>] [--workspace <WORKSPACE>]` | exits `2` (`--from` defaults to `canonical`, `--workspace` to the current directory) | `slipforkd` world creation (`0.2.0`) |
| `slipfork exec --world <WORLD> [--cwd <CWD>] [--snow-file <SNOW_FILE>] -- <ARGV>...` | exits `2` | `slipforkd` world execution (`0.2.0`) |
| `slipfork drop <NAME>` | exits `2` | `slipforkd` world teardown (`0.2.0`) |

Both human and `--json` output report the same refusal, so scripts fail closed instead of mistaking it for success.
