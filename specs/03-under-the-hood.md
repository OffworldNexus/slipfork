# 03 — How It Works Under the Hood

Slipfork runs your cloud environment locally in a box; CI is the clutch. It boots full-stack worlds and clones live 8-component branches on **Btrfs CoW subvolumes** (`100%` Docker-free) — Slipfork's own fork snapshot measures **`~150–365ms`** on the 4-vCPU droplet (asserted **`< 500ms`**), while a full cold start of the real FastAPI (`uvicorn --reload`) + SvelteKit (`vite dev`) demo reaches first `200 OK` at a median **`~5.5s`**, asserted **`< 8.0s`** for 2-vCPU GitHub Actions loop-Btrfs runners.

<!-- sample output -->
```console
$ slipfork up
✓ Ready -> http://demo.localhost  (4.8s)
$ slipfork fork feat-auth
✓ feat-auth (4.8s) -> http://feat-auth--demo.localhost
```

*Sample transcript above; the asserted contract is two-tier: `snapshot_ms` `~150–365ms` measured with a `< 500ms` asserted ceiling for the CoW clone, and a median end-to-end cold start `~5.5s` measured with a `< 8.0s` asserted ceiling (1 warm-up + 3 measured samples) on the 4-vCPU dev VM and 2-vCPU CI runners, via `std::time::Instant` in `critical_fork_live`.*

---

## 1. Pure Btrfs Layout, `<world>/.slipfork-world-stamp.json` & Persistent Logs

Everything lives natively on one Btrfs mount — your checkout (`~/projects/demo`) works whether it is a Btrfs subvolume or a plain directory, and the store holds flat sibling subvolumes plus persistent component logs. The store root is `<btrfs_mount>/.slipfork-store` on a writable non-`/` mount, and `$XDG_DATA_HOME/slipfork/store` when the longest mount is `/` or is not writable, so a Btrfs-root host never writes `/.slipfork-store`:

`slipfork up` calls only `ensure_store` and never converts or rewrites your checkout. `slipfork fork` snapshots a subvolume checkout with one CoW ioctl and copies a plain checkout's tree instead, with no adoption or conversion step.

```text
<btrfs-mount>/
├── home/you/projects/demo/                       # Project subvolume (code + .venv + node_modules + .slipfork/)
│   └── .forks/feat-auth/                         # CoW project snapshot (one ioctl)
└── .slipfork-store/
    ├── images/                                   # Read-only OCI & Dockerfile step subvolumes
    ├── logs/<world>/<id>.log                     # Persistent pod/resource/service logs (survives failed up & down)
    ├── logs/<world>/runs/index.jsonl             # One catalog record per executed run (survives failed up & down)
    ├── logs/<world>/runs/<run_id>.log            # That run's captured stdout/stderr (`logs -r <id>`)
    ├── pods/<world>/                             # Ephemeral writable pod rootfs clones (deleted on down & drop)
    ├── templates/postgres-18/                    # Golden initdb subvolume (sealed BTRFS_SUBVOL_RDONLY)
    └── worlds/
        ├── demo/                                 # World subvolume: s3/ + redis/ + .slipfork-world-stamp.json
        ├── demo.pg.pg/                           # Sibling PGDATA subvolume + .slipfork-pg-stamp.json
        ├── feat-auth--demo/                      # CoW snapshot of world subvolume + world stamp
        └── feat-auth--demo.pg.pg/                # Crash-consistent CoW snapshot of live PGDATA + PG stamp
```

A project, world, or `PGDATA` clone is one `BTRFS_IOC_SNAP_CREATE_V2` ioctl (`~0.3ms` on local NVMe, single-digit ms on network block storage, `≤50ms` ceiling) — after a one-off direct-TCP PostgreSQL `CHECKPOINT` (`~1–3ms`, Simple Query over `Request::Dial { port: 5432 }` on `<pods>/control.sock`) and one `syncfs(2)` that commits its page-cache writes into the Btrfs tree.

```mermaid
flowchart LR
  Tpl["templates/postgres-18\n(BTRFS_SUBVOL_RDONLY)"] -.->|"up (one CoW ioctl)"| Pg["worlds/demo.pg.pg\n(+ world_epoch)"]
  Proj["~/projects/demo\n(code + deps + .slipfork/)"] ==>|"fork (one CoW ioctl; plain checkout is copied)"| FProj[".forks/feat-auth"]
  World["worlds/demo\n(s3 + redis + .slipfork-world-stamp.json)"] ==>|"fork (one CoW ioctl)"| FWorld["worlds/feat-auth--demo"]
  Pg ==>|"fork (CHECKPOINT + syncfs, then CoW)"| FPg["worlds/feat-auth--demo.pg.pg"]
```

!!! info "Epoch-paired stamps, run catalog & persistent logs"
    When a world finishes booting, Slipfork writes one `world_epoch` into **both** `<world>/.slipfork-world-stamp.json` and `worlds/<world>.pg.<id>/.slipfork-pg-stamp.json` so `fork` reuses stamps in `0ms` when epochs match.

    Every executed image pull/build, setup, hook, resource, and service start mints a six-character `run_id` and appends one record (`run_id, kind, target, command, state, started_ms, finished_ms, duration_ms, project, world`) to `<store>/logs/<world>/runs/index.jsonl` **before** the step runs. The step's output lands in `runs/<run_id>.log`, while a live pod also keeps `<store>/logs/<world>/<id>.log`; so `slipfork logs <image>` replays a failed build and `slipfork logs -r <id>|last|failed` replays one record even after a failed cold `up` or `down`.

    Retention keeps the newest 20 successful runs per world and never prunes a `failed` or crashed (`running`) record, so the artifact an operator needs outlives every teardown; `down` deletes only `<runtime_dir>/pods/<world>` and `<store>/pods/<world>/`, and `drop` removes the world's logs.

**Pod user baseline.** Every pod runs as the invoking `$UID`/`$GID` (identity-mapped into the world user namespace) with a synthetic two-line `/etc/passwd`. Unprivileged pods get a writable `0755` `/home/slipfork` inside their CoW rootfs and `HOME=/home/slipfork` in `config.json` whenever the merged image/Forkfile env leaves `HOME` unset, empty, or `/root`; a declared non-`/root` `HOME` always wins and uid `0` keeps `/root`. Images such as `python:3.13-slim` and `node:22-slim` therefore need no `ENV HOME` change for `uv`, `pnpm`, `git`, or `bash` to write caches.

**Pod DNS.** Every pod joins its own network namespace, so the resolver file bound as `/etc/resolv.conf` always carries a non-loopback `nameserver`; when the host's `/etc/resolv.conf` only lists loopback stubs (`127.0.0.53`) and `/run/systemd/resolve/resolv.conf` holds the real upstream servers, pods bind that upstream file instead. An absent or still-loopback upstream keeps `/etc/resolv.conf` untouched, so pods fail closed rather than resolving through an invented server.

**World egress.** The world's network namespace ships only `lo`, so Slipfork supervises one `slirp4netns` forwarder for the world's whole lifetime: pods get `tap0` on `10.0.2.100/24` with a default route, which is how `uv sync`, `pnpm install`, and any HTTPS call reach PyPI, npm, and the wider internet without Docker. `--disable-host-loopback` keeps the host's `127.0.0.1` unreachable from pods, the forwarder is reaped with the world, and every pod still binds a non-loopback `nameserver` (see **Pod DNS**).

---

## 2. Native Btrfs-Backed Rust S3 (`< 1ms` Boot, Zero Extra Containers)

Slipfork pulls zero external S3 images and runs zero distributed daemons. Detached `__supervise` sends **one typed `Request::ConfigureS3(S3WorldConfig)` frame** over `0600` `control.sock`, and `slipfork-world-init` constructs `S3Server::new(config)` on `127.0.0.1:8333` in `< 1ms`:

| Capability | Exact Contract (`resources/s3/{server.rs,object_ops.rs,listing.rs,policy.rs}`) |
| :--- | :--- |
| `object_ops.rs` | Payload at `<world>/s3/<bucket>/objects/<key>` and atomic sidecar JSON at `<world>/s3/<bucket>/meta/<key>.json` (`content_type`, quoted `etag`, `content_length`, `last_modified`) — plain files cloned instantly on `fork`. |
| `server.rs` | `hyper` `1` (`http1`, `server`) on `127.0.0.1:8333` dispatching path-style (`/<bucket>/<key>`) and the ingress-forwarded `Host: <bucket>.<world>.localhost` `PUT`, `GET`, `HEAD`, `DELETE`, `HeadBucket`, and `?list-type=2`; `cdn <id>` adds no local authority and aliases the bucket host. |
| `listing.rs` | `ListObjectsV2` XML (`prefix`, `delimiter`, `max-keys`, `continuation-token`) sorted lexicographically over Btrfs bucket keys. |
| `policy.rs` | `BucketPolicy` (`Private` \| `PublicRead`) + `SigV4Verifier` (`hmac = "0.12"` `Mac::verify_slice` + `sha2 = "0.10"`, `SecretValue` from `slipfork-forkfile`, `<sha256-hex>` & `UNSIGNED-PAYLOAD`): `Private` denies anonymous `GET` (`403`); `PublicRead` allows anonymous `GET`/`HEAD` (`200`) and denies anonymous `PUT`/`DELETE`/list (`403`). |

---

## 3. Direct-Exec Databases, Parallel Fork Boot DAG & Two-Tier Latency Budget

- **PostgreSQL 18 (`~30–50ms` start, direct `postgres` exec, one `~1–3ms` pre-fork `CHECKPOINT` + `syncfs(2)`):**
  - When `PG_VERSION` already exists in `PGDATA`, `PostgresLauncher` unlinks stale `postmaster.pid`, `postmaster.opts`, and `pg_stat`, mounts `/dev/shm`, and `exec`s the `postgres` binary **directly** by absolute path (never `docker-entrypoint.sh` bash wrappers).
  - Dev flags `-c fsync=off -c synchronous_commit=off -c full_page_writes=off -c wal_level=minimal -c max_wal_senders=0 -c autovacuum=off -c shared_buffers=32MB -c dynamic_shared_memory_type=posix` replay any tail WAL in `< 5ms` while readiness polls `.sock/.s.PGSQL.5432` every **`2ms`**.
  - Before `fork`, `PostgresLauncher` sends `CHECKPOINT;` as a PostgreSQL v3 Simple Query over `Request::Dial { port: 5432 }` (`SCM_RIGHTS`, no `crun exec`/`psql` spawn) and then one `syncfs(2)` commits the flushed `shared_buffers`/`wal_buffers` page-cache writes into the Btrfs tree, so `INSERT`s committed immediately before `fork` survive.
- **Redis 7 (`~5–15ms` start, direct `redis-server` exec):** `RedisLauncher` `exec`s `redis-server` directly by absolute path (never `docker-entrypoint.sh`) with `--bind 127.0.0.1 --dir /data --save "" --appendonly yes --appendfsync no` on `<world>/redis/<id>`, polling `:6379` every **`2ms`**.
- **Parent `image` subvolume reuse (`ImageCache`) & verbatim Python commands:** On `fork` with a matching `world_epoch`, `ImageCache` adopts the parent world's resolved read-only `image` subvolumes directly from `<world>/.slipfork-world-stamp.json` (`cached`, `~1ms`). Runtime Python commands exec the project's `.venv/bin/<prog>` directly or run the `Forkfile`-authored command verbatim; Slipfork never injects `uv` flags, so a non-`dev` world writes `uv run --no-sync …` itself.
- **Direct-bin service launch (`@uv run <bin>` / `@pnpm exec <bin>`):** Once the setup node installed dependencies, the launcher resolves the verb straight to `<workdir>/.venv/bin/<bin>` or `<workdir>/node_modules/.bin/<bin>` and `exec`s it, skipping `uv`/corepack wrapper startup on every pod boot.

```mermaid
flowchart LR
  Img["Image: backend / frontend\n(~1ms cached subvol on fork)"] --> Stamp{"before-start & pre-deploy\nworld_epoch stamp current?"}
  Res["Resources: pg (~40ms) + cache (~10ms) + s3 (<1ms)\n(direct binary exec in parallel)"]
  Stamp -->|"yes (up to date, 0ms)"| FastSvc["Services: api / worker / front\n(crun spawn IN PARALLEL with pg & redis)"]
  Stamp -->|"no (cold up / stale hook)"| GateHook["Run setup / before-start;\npre-deploy awaits pg + redis + s3 ready"]
  Res --> GateHook --> SlowSvc["Services spawn after migration"]
  FastSvc & SlowSvc & Res --> Ready["World Ready + :80 route swap\n(all resources & services probed ready)"]
```

**Boot-DAG Concurrency & Readiness Rule (`BootScheduler`):**
1. **Stale or unstamped hook (cold `up`, schema/lockfile edit, or `world_epoch` mismatch):** `@<tool>` setup and `before-start` run after their `image` resolves, `pre-deploy` awaits `image` + `setup`/`before-start` + **all backing `resource`s (`pg`, `redis`, `s3`) ready** before running migrations, and `service` container spawn waits for `pre-deploy` completion.
2. **Current stamps (`fork` or warm `up` with matching `world_epoch` + input digests):** `setup`, `before-start`, and `pre-deploy` verify stamps immediately upon `image` completion (`0ms` `up to date`, **without** waiting on `pg`/`redis`), and `service` containers spawn **in parallel** alongside `pg` and `redis` startup — while `:80` ingress activation, `✓ Ready`, and `post-deploy` still await **every** `resource` and `service` readiness probe.

!!! note "Where the seconds go"
    Snapshot, world init and pod spawn stay in the low hundreds of milliseconds; the median is set by `uvicorn --reload` import/transform and `vite dev` first-request SSR inside the app pods. On a warm fork those steps print `cached`/`up to date`, mint no run id, and re-run zero commands.

| `slipfork fork <name>` Stage (Full 8-Component Stack) | Target | Hard Ceiling |
| :--- | :--- | :--- |
| `CHECKPOINT` + `syncfs(2)` on live PG, then parallel Btrfs CoW snapshots (project, world, PG) + git branch switch — reported as `snapshot_ms` | `150–365ms` | **`< 500ms`** |
| World `unshare` + `slipfork-world-init` + `ConfigureS3` `:8333` + `world_epoch` image/hook reuse (`cached` `~1ms`) | `15–35ms` | — |
| Parallel `crun` pod boot (direct-exec PG18 `~40ms`, Redis `~10ms`, services spawning concurrently with `2ms` poll) + `:80` swap | `150–350ms` | — |
| App boot inside the pods: real `uvicorn --reload` import/transform + `vite dev` first-request SSR (external to Slipfork, dominates the total) | `1.5–3.5s` | — |
| **End-to-end (`Command::spawn` -> all 8 components ready + `http://<fork>--<world>.localhost` `200 OK`)** | **median `~5.5s`** | **`< 8.0s` (`8000ms`)** on 4-vCPU dev VM and 2-vCPU Actions loop-Btrfs |

---

## 4. Daemon `daemon.sock`, World `control.sock` & Real-PTY `SCM_RIGHTS`

```mermaid
flowchart LR
  CLI["CLI: slipfork shell <target>\n(RawModeGuard + SIGWINCH TIOCSWINSZ)"] <-->|"daemon.sock: shell/exec-pty\n(<- SCM_RIGHTS PTY master fd)"| Daemon["__daemon\nIngressRouter + RPC"]
  Daemon <-->|"control.sock: ExecPty / Dial\n(<- SCM_RIGHTS PTY master or netns fd)"| Init["slipfork-world-init PID 1\ncrun exec --tty --console-socket + :8333 S3"]
```

| Socket (`0600`) | Direction & `SCM_RIGHTS` | Canonical Frames |
| :--- | :--- | :--- |
| `<runtime_dir>/daemon.sock` | CLI / `__ingress` → `__daemon`; `SCM_RIGHTS` passes `:80` client fd (`ingress.accept`, client → daemon) and `/dev/pts/N` master fd (`shell` TTY, daemon → CLI). | Request `{"v":1,"id","method","params"}` → streamed `event` frames (`started`, `log`, `finished`, `up-to-date`, `failed`, `ready`, `stopped`) → final `ok` / `error` (`log` streams `logs -f` / piped `exec`; `BootTui` never prints `log` frames). |
| `<pods_dir>/control.sock` | `__supervise` / `__daemon` → PID 1 `slipfork-world-init`; `SCM_RIGHTS` returns world-netns stream fd on `Dial` and `crun exec --tty --console-socket` PTY master fd on `ExecPty` (init → daemon). | `ConfigureS3(S3WorldConfig)`, `Dial { port }`, `List`, `Shutdown`, `ExecPty` (`"terminal": true`), `Exec` (piped stdio), `Logs` (live tail over `<store>/logs/<world>/<id>.log`; `__daemon` reads `<store>/logs/<world>/<id>.log` directly when `<pods_dir>` is absent). |

---

## 5. Composition Roots, Crate Map & Canonical Ports

Slipfork has **five explicit composition roots** and **zero** ambient OS/env probes in library crates:
1. `crates/slipfork/src/main.rs` (CLI, `__daemon`, `__ingress`) — parses `Cli`, builds `UiMode`, `BootTui` (`ScrollRegionGuard` / `AsciiLine`), `RawModeGuard`, `DaemonClient`, and `DaemonServer`.
2. Detached `__supervise` (`crates/slipfork/src/main.rs` child mode) — owns the single boot `tokio::Runtime` and constructs `BootScheduler` (injecting `tokio::runtime::Handle`), `ImageCache`, `PostgresLauncher`, `RedisLauncher`, `WorldStampStore`, `BtrfsEngine`, `NativeImageBuilder`, `FileSecretVault`, and `S3WorldConfig`.
3. `crates/slipfork-world/src/bin/world_init/main.rs` (world PID 1) — binds `0600` `control.sock`, serves `ConfigureS3`/`Dial`/`List`/`Shutdown`/`ExecPty`/`Exec`/`Logs`, and hosts `S3Server` on `:8333`.
4. `crates/slipfork-lsp/src/main.rs` — standalone cross-platform stdio language server.
5. `packages/opencode-slipfork/src/index.ts` — OpenCode V2 plugin shelling the `slipfork` binary.

| Crate | Canonical Cohesive Structs / Modules (`<= 500` LOC each) | Port → Adapter |
| :--- | :--- | :--- |
| `slipfork` | `Cli` (`clap` 4.6 + `clap_complete` + `clap_mangen`), `UiMode`, `BootTui` (`ScrollRegionGuard` `DECSTBM` + `AsciiLine`), `RawModeGuard`, `TableLayout`, `ImageCache` (`world/runtime/images.rs` parent image-subvolume adoption), `DaemonClient`, `DaemonServer` | — |
| `slipfork-forkfile` | `ForkfileCompiler`, `SecretValue` (redacted `Debug`, zeroize-on-drop), `BucketPolicy`, `DnsLabel`, `Formatter`, `SnowMigrator` | — |
| `slipfork-world` | `BtrfsEngine`, `BootScheduler` (`Handle` + `spawn_blocking`), `WorldStampStore` (`world_epoch`), `PostgresLauncher`, `RedisLauncher`, `resources/s3/{server.rs,object_ops.rs,listing.rs,policy.rs}`, `WorldSupervisor` | `WorldEngine` → `BtrfsEngine` |
| `slipfork-image` | `NativeImageBuilder`, `OciPuller`, `DockerfileExecutor` | `BaseImageBuilder` → `NativeImageBuilder` |
| `slipfork-ingress` | `IngressRouter`, `HttpRelay` (`httparse` raw byte/WS proxy) | — |
| `slipfork-ospp` | `WsOsppClient`, `FileSecretVault`, `DeviceIdentity`, `DumpReader` | `OsppClient` → `WsOsppClient`, `SecretVault` → `FileSecretVault` |
| `slipfork-vcs` | `GitWorkspace` | `VcsWorkspace` → `GitWorkspace` |
| `slipfork-lsp` | `LspServer` | — |

The canonical port set is strictly `WorldEngine`, `BaseImageBuilder`, `SecretVault`, `OsppClient`, and `VcsWorkspace` — zero single-impl mock traits sit beside them.
