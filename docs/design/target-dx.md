# Slipfork M2–M3 Target Developer Experience Specification

- **Status:** Accepted Target DX Specification (`ADR-0001`..`ADR-0010`, `H-001`..`H-011`)
- **Visibility:** Internal design spec (`exclude_docs` in `zensical.toml`). Per **Core Rule #4 (Zero Vaporware in Published Docs)**, sections here promote into published `docs/` and `README.md` **only** as each binary capability ships and passes conformance tests (`M2.19`, `M3.18`).

---

## 1. World Lifecycle DX (`slipforkd`, Zero-Setup Auto-Spawn, `OSPP v1`, Btrfs Preflight & Rescue Tooling)

### 1.1 Zero-Setup Auto-Spawn vs Optional Port-80 Systemd Socket Activation

Installing the workspace places `slipfork`, `slipforkd`, and `git-remote-slipfork` onto `PATH` (source install today):

```console
$ cargo install --git https://github.com/OffworldNexus/slipfork slipfork

# Zero-setup default (containers, CI, laptops without root):
# Running `slipfork up` or `slipfork fork` automatically starts
# `slipforkd --listen 127.0.0.1:0 --state-dir <state_dir> --runtime-dir <runtime_dir>`
# on <runtime_dir>/daemon.sock (0600) and prints http://acme.localhost:<port>!

# Optional workstation port-80 setup (PID 1 systemd binds :80 + :7080; slipforkd runs as User=<developer-user>):
$ slipfork daemon unit --only socket  | sudo tee /etc/systemd/system/slipforkd.socket > /dev/null
$ slipfork daemon unit --only service | sudo tee /etc/systemd/system/slipforkd.service > /dev/null
# Ubuntu 24.04 LTS (kernel.apparmor_restrict_unprivileged_userns=1): the 4-line `userns,` profile (`M3.15`)
$ slipfork daemon unit --only apparmor | sudo tee /etc/apparmor.d/slipfork > /dev/null
$ sudo systemctl daemon-reload && sudo systemctl enable --now slipforkd.socket
```

`slipforkd` runs **unprivileged as `User=<developer-user>`** (resolved once from the invoking account via `id -un` into non-template `slipforkd.service`) with an empty `CapabilityBoundingSet=` and acts as the single local authority (`H-010` #1): it holds the persistent `OSPP v1` WebSocket session, resolves `snow.yml` secrets/resources, supervises Btrfs subvolume worlds, and routes `.localhost` (via `snow.yml`'s `ingresses` longest-prefix table) + shared tunnel traffic. **OSPP is never required to boot local worlds:** offline laptops work out of the box from `snow.yml` + `~/.local/share/slipfork/vaults/<project>.json` (`0600`; `slipfork.toml` is optional).

### 1.2 `slipfork up`, Diagnostic `[resources]` Lines & Seed Dump Flags (`--no-dump | --refresh-dump`)

`up` passes the caller's explicit `cwd` over `<runtime_dir>/daemon.sock` to boot the world selected by `--world` or by `cwd`'s nearest ancestor (§1.6 is the single selection rule), waits for declared component `internal_ports` readiness — `BtrfsEngine::spawn_service`'s injected `ReadinessProbe` (`ADR-0002` §2), never a daemon-side poller — before printing `[origin]`, and is idempotent when re-run (`down` stops services immediately without waiting for readiness):

```console
$ slipfork up                           # Paired laptop (:80 socket activation; :<port> when auto-spawned)
[resources] media: remote -> remote (configured)
[resources] main: prefer_remote -> remote (mode_default)
[ready]     api (internal :8000 in world netns)
[hook]      api:pre-deploy: sh -eu -c uv run python manage.py migrate
[origin]    http://acme.localhost

$ slipfork up --no-dump                 # Skip seed dump and run clean initdb -> pre-deploy migrations
$ slipfork up --refresh-dump            # Require live OSPP dump check (mutually exclusive with --no-dump; fails closed if offline)
$ slipfork up --resources local         # Force hermetic world-local S3 + clean initdb (safe for agent runs)
```

- **One `[resources]` line per decision, bare ids:** each row is `[resources] <id>: <strategy> -> <resolved> (<reason>)` with the `snow.yml` resource id verbatim (`media`, `main`) and the `snake_case` labels of `ResourceStrategy` / resolved target / reason (`local|prefer_remote|remote`, `local|remote`, `configured|mode_default|not_offered|no_dump|refreshed_dump`).
- **Offline / Unpaired Transparency:** When unpaired or offline under default `prefer_remote`, `ResourcePlan::resolve` never degrades silently: it returns the typed `{strategy, resolved, reason}` triple (`prefer_remote`, `local`, `not_offered`), which `slipforkd` streams as a boot event and the CLI renders as `[resources] main: prefer_remote -> local (not_offered)` (and in `--json`).
- **Port-Aware Single Origin & In-Netns Loopback Bridge (`H-010` #2, `H-011` #4):** `WorldOrigin::local(&route, ingress_port)` renders `http://acme.localhost` on `:80` and `http://acme.localhost:<port>` under auto-spawned `--listen`, while the world's PID 1 `slipfork-world-init` (spawned by `BtrfsEngine`, `M3.13`, `ADR-0010` §6) binds an **in-netns loopback TCP bridge** on `127.0.0.1:<ingress_port>` inside each world and splices accepted streams to `slipforkd`'s `0600` `<state_dir>/http-ingress.sock`, so `$BASE_URL` resolves identically from host browsers and inside `slipfork exec` (and, in `M3.2` Phase 2 target state, `slipfork test`) (`snow.yml` `ingresses` prefixes route `/api`, `/`, etc. to their target component ports). **Never connect to host `127.0.0.1:5432` or `:8000`**; use the printed `origin`, `slipfork exec`, or `slipfork forward --json`.
- **World Rootfs & `--host-caches` (design of record: `ADR-0009` §1–§3; operational detail in `docs/design/world-base-image.md`):** every world's `L0` rootfs is built once from the project's own `snow.yml` `images:` `Dockerfile` stage, selected by `[world]` in `slipfork.toml` (`ADR-0009` §1: `world.image` / `world.stage`), seeded into the transient `<state_dir>/worlds/seed-<l0_sha12>` subvolume and sealed as the read-only base subvolume `<state_dir>/snapshots/<project>-base-<l0_sha12>`, whose `<subvol>/rootfs` tree is mounted read-only at `/`; every fork and test slot is then one `BTRFS_IOC_SNAP_CREATE_V2` clone of that base (`ADR-0010` §2–§3). `postgres:<v>`, `redis:<v>`, and SeaweedFS run as daemonless `crun` containers sharing the world's `127.0.0.1`. `--host-caches` adds the host toolchain caches **read-only** only when the ELF/libc ABI probe matches, failing closed with the measured mismatch otherwise — it is never a `slipfork.toml` key.

### 1.3 First-Class Hook Failure Rescue UX & Direct CLI TTY Entry (`hook run`, `logs`, `shell`, `exec`)

Hooks (`<component>:<phase>`, e.g. `api:pre-deploy`) stream stdout/stderr live from `slipforkd`. On non-zero exit, Slipfork prints the **full un-truncated output**, **preserves the failed world** (listed in `slipfork list`), and prints three copy-pasteable rescue commands using the `--world` selector (§1.6):

```console
$ slipfork fork billing
[hook]      api:pre-deploy: sh -eu -c uv run python manage.py migrate
... (full untruncated traceback) ...
error: hook 'api:pre-deploy' exited with code 1 in world 'acme:billing'
  argv: ["sh", "-eu", "-c", "uv run python manage.py migrate"]
  log:  /home/acme/.cache/slipfork/acme/worlds/acme:billing/logs/api.pre-deploy.log

Rescue commands (world 'acme:billing' kept alive for inspection):
  slipfork hook run api:pre-deploy --world billing          # add --no-tty in non-interactive shells
  slipfork logs --world billing api:pre-deploy
  slipfork shell --world billing                            # or: slipfork exec --world billing -- <cmd>
```

- **Retained log paths join the world id (not the bare fork):** `slipforkd` writes `<state_dir>/<project>/worlds/<world_id>/logs/<component>.<phase>.log` — the shipped `Layout::world_logs_dir` join, a daemon-owned plain directory outside the world's CoW subvolume (`ADR-0010` §2), so a failed world's logs outlive the world they came from. Nothing reaps them automatically in Phase 1 (`prune` releases world records only), so delete the directory to reclaim space. The `log:` line above is the real path on disk and stays unambiguous when a project has both the `<project>` and `<project>:<fork>` worlds.
- **Direct CLI World Entry (`H-010` #1):** `slipforkd` is the **sole owner** of `snow.yml` (`modes.local`) environment resolution (`@slipfork/opencode` only routes `shell` calls to `slipfork exec --cwd <cwd>`, `ADR-0004`). For `shell`, interactive `hook run` (`pdb`), and `exec`, the daemon returns the world's entry command — `nsenter --target <init_pid> --user --preserve-credentials --mount --net --ipc --pid` into the world's `slipfork-world-init` PID 1 (`ADR-0010` §6) — plus the resolved environment; the thin `slipfork` CLI then enters the world inheriting its native terminal `stdin`/`stdout`/`stderr`, so that CLI ↔ `slipforkd` boundary carries **zero file descriptors** (the in-daemon `netns.sock` bridge is a separate concern, `ADR-0010` §2). `db` / `redis` enter their service container instead (§1.4). `shell` and default `hook run` require a TTY, failing closed naming `hook run --no-tty` or `slipfork exec`.

### 1.4 Service Clients (`db`, `redis`, `forward`), Secrets, VCS (`git` + `jj`) & `OSPP v1` Pairing

```console
$ slipfork exec --world billing -- uv run pytest -k billing   # Non-interactive agent/CI execution (world entry, §1.3)
$ slipfork db --world billing                                 # Interactive psql in the postgres service container (rule below)
$ slipfork redis --world billing                              # Interactive redis-cli in the redis service container (rule below)
$ slipfork forward --world billing --json                     # Daemon-owned 127.0.0.1:0 bridge (lives until Ctrl-C/disconnect; payload in §1.5)

$ git pull slipfork billing                                                                # Committed branch refs/heads/billing (@- in Jujutsu)
$ git fetch slipfork '+refs/slipfork/billing/wip:refs/slipfork/billing/wip'                 # Dirty + untracked snapshot (@ in Jujutsu; equals base when clean)

$ printf '%s' "$STRIPE_KEY" | slipfork secret set STRIPE_SECRET_KEY          # --force to rotate existing key
$ printf '%s\n' "$PAIR_URL_WITH_FRAGMENT" | slipfork auth login -            # Stdin keeps #handle= out of argv
[auth] paired p256:02a1…8f90 (backend=tpmd:sealed)
```

- **Service clients enter the service container, never the app rootfs (`ADR-0009` §2–§3 — the single `crun exec` rule that the `db` / `redis` examples and table rows point at):** `slipforkd` answers `exec_target` with `WorldEngine::service_entry_command`, i.e. a `crun exec` entry into the world's running `postgres:<v>` / `redis:<v>` container, so `psql` and `redis-cli` come from the container's own rootfs and the project `Dockerfile` never needs DB client packages (and dump/restore versions can never skew). A stopped or absent backing service fails closed naming the service id (`ServiceNotRunning`) instead of silently entering the world. `slipfork forward` is unchanged: `slipforkd` binds a host `127.0.0.1:0` listener and bridges it with `dial_world_tcp` to the in-world port.

### 1.5 Normative `--json` Contracts, Lifecycle (`list`, `down`, `prune`) & `share`

```console
$ slipfork list
WORLD            STATE     ORIGIN                                   WORKTREE
acme             running   http://acme.localhost                    /repo
acme:billing     failed    http://billing.acme.localhost            /repo/.worktrees/billing

$ slipfork down --world billing            # Stop world services while keeping .worktrees/billing
$ slipfork prune                           # Preview orphaned worlds (--yes to delete)
```

- **`--json` schemas (single normative home; `--json` writes only JSON to `stdout` and suppresses spinners/ANSI):**
  - `slipfork list --json` -> `[{"world": "acme:billing", "state": "running|failed|stopped", "origin": "http://billing.acme.localhost", "worktree": "/repo/.worktrees/billing"}]` (**exactly one** `origin` URL per world, port-qualified when `ingress_port != 80`).
  - `slipfork up --json` / `fork --json` -> `{"world": "acme:billing", "origin": "http://billing.acme.localhost", "resources": [{"id": "media", "strategy": "remote", "resolved": "remote", "reason": "configured"}]}`.
  - `slipfork forward --json` -> `{"world": "acme:billing", "targets": [{"id": "pg_databases.main", "host_addr": "127.0.0.1:43182", "uri": "postgres://127.0.0.1:43182/main"}]}`.
  - `slipfork support --json` -> `[{"id": "...", "category": "...", "tier": "...", "evidence": {...}, "required_evidence": [...]}]`.
- **One world spelling:** every `--json` `"world"` field uses the same `world_id` as the `WORLD` column (§1.6); table columns are padded, never truncated, and `--json` carries the full values.
- **Sharing (`slipfork share [--stop] [--world <world>]`, `ADR-0005` §2):** until `tunnel-weaver` publishes `weaved`'s IPC specification (closed `H-006`; the concrete `WeavedRegistrar` is the isolated leaf `M4.1`), `slipfork share` **fails closed** telling the caller to use the world's `.localhost` origin (or `EdgeError::NoForwardListener` when `slipforkd` runs without a forward listener). With a registrar wired, toggling sharing inside `slipforkd` tears down the live world and re-materializes it from its `base`/fixture snapshot with the single new `WorldOrigin` (discarding uncommitted post-snapshot DB mutations while preserving worktree files); `share --stop` on an unshared world is an idempotent no-op.

### 1.6 Complete M2–M3 CLI Command Table

- **World selection — one rule (`M3.15`, `ADR-0005` §3):** every world-scoped command resolves its world against the request's `cwd` repository (`RepoRoot.project`) through the single `project::resolve_world_selector` parser, so **every same-project id printed by `slipfork list` or any `--json` payload is accepted verbatim**:
  - `<project>:<fork>` (e.g. `acme:billing`) — must match the `cwd` repository's project; a cross-project id fails closed with a remedy (`cd` into the project via the `WORKTREE` column of `slipfork list`, or `slipfork exec --cwd <worktree> --world acme:billing`), and `<fork>` equal to the project name is rejected.
  - `<project>` (e.g. `acme`) — selects the project's own world even from inside `.worktrees/<fork>`.
  - `<fork>` (e.g. `billing`) — repo-local shorthand for `<project>:<fork>`, validated by `validate_fork_name` (invalid DNS labels and reserved `test-*` slots fail closed). A bare label that is not this repository's project is always a fork name, never another project's world.
  - Omitting `--world` resolves `cwd`'s nearest ancestor: `<vcs_root>/.worktrees/<fork>[/...]` -> `<fork>`, repo root -> `<project>`, outside any repository -> fail closed naming `cd <repo>` or `slipfork exec --cwd <repo>`.
- **Positional forms:** `slipfork fork <fork>` takes a bare `<fork>`, and `slipfork drop <fork>` also accepts the printed `<project>:<fork>` id.
- **Milestone legend:** a `+` separates the shipped stage(s) from the node that completes the row's full surface (`M3.13` the `BtrfsEngine` behind it, `M3.15` `doctor` + the world selector + the `apparmor` profile, `M3.2` snapshot/cache GC). `M4.1` appears only on `share`, which ships but stays fail-closed until that leaf lands (§1.5).

| Command | Milestone | Behavior |
|---|---|---|
| `slipfork up [--world <world>] [--no-dump \| --refresh-dump] [--resources local] [--host-caches] [--json]` | M2 + `M3.13` | Boot world in `slipforkd` (auto-spawning if needed), seed the `Dockerfile` `L0` rootfs and seal the read-only base subvolume, clone the world from it, start `crun` backing services, await readiness, print single `origin` |
| `slipfork down [--world <world>]` | M2 + `M3.13` | Stop world services without deleting its worktree (idempotent) |
| `slipfork fork <fork> [--base <rev>] [--no-dump \| --refresh-dump] [--resources local] [--host-caches] [--json]` | M2 + `M3.13` | Create `.worktrees/<fork>` (`git` or `jj`) + Btrfs subvolume world in `slipforkd` (one `BTRFS_IOC_SNAP_CREATE_V2` clone, same `L0` rootfs digest, `0` rootfs copy) |
| `slipfork drop <fork>` | M2 + `M3.13` | Destroy fork world (clear read-only flags, `BTRFS_IOC_SNAP_DESTROY_V2`) and remove `.worktrees/<fork>` |
| `slipfork exec [--world <world>] [--cwd <dir>] -- <argv>...` | M1 + M2 + `M3.13` | Fetch `WorldExecTarget` from `slipforkd` and run non-interactively inside the world's `slipfork-world-init` PID 1 (`nsenter --target <init_pid>`, §1.3) |
| `slipfork list [--json]` | M2 | List worlds with state, worktree path, and the single `origin` URL (§1.5) |
| `slipfork prune [--dry-run \| --yes] [--json]` | M2 + `M3.2` | Preview or garbage-collect orphaned worlds now; unreferenced Btrfs snapshot and `cache/{rootfs,oci}` GC lands with `M3.2` |
| `slipfork shell [--world <world>]` | M2 + `M3.13` | Enter the world on a native TTY (`nsenter --target <init_pid>`, §1.3; requires TTY) |
| `slipfork logs [--world <world>] [<target>] [--follow] [--stderr] [--lines <n>]` | M2 | Read or follow retained component/hook logs from `slipforkd` (§1.3 log paths) |
| `slipfork hook run <hook_id> [--world <world>] [--no-tty]` | M2 | Re-run `<component>:<phase>` hook (direct native TTY `pdb` by default; `--no-tty` via daemon) |
| `slipfork db [<db_id>] [--world <world>]` | M2 | Interactive `psql` in the running `postgres:<v>` service container (§1.4); requires TTY |
| `slipfork redis [<cluster_id>] [--world <world>]` | M2 | Interactive `redis-cli` in the running `redis:<v>` service container (§1.4); requires TTY |
| `slipfork forward [--world <world>] [--json] [<target>...]` | M2 | Ask `slipforkd` to bind `127.0.0.1:0` bridge(s) for host GUIs (held open until disconnect) |
| `slipfork share [--stop] [--world <world>]` | M2 + `M4.1` | Re-materialize with/without a `weaved` origin (fail-closed rule in §1.5) |
| `slipfork secret set [--force] <KEY>` / `unset <KEY>` / `list` | M2 | Manage `0600` vault secrets via `slipforkd` (`stdin` input; `--force` rotates existing keys) |
| `slipfork auth login [- \| '<url>#handle=<64-hex>']` | M2 | Pair `tpmd`/P-256 device key over the `ospp-v1` subprotocol (`-` reads URL from `stdin`) |
| `slipfork daemon unit [--only socket\|service\|apparmor] [--forward-port <u16>]` | M2 + `M3.15` | Render non-template `slipforkd.socket`, `slipforkd.service` (`User=<developer-user>`), and the `/etc/apparmor.d/slipfork` `userns,` profile |
| `slipfork doctor` / `snow resolve` / `dag plan` | M1 + `M3.15` | Probe the Btrfs host contract (§1.7), inspect resolved `snow.yml` envs, and plan DAG waves |
| `slipfork support [--json]` | M2 | Report verified integration tiers & `required_evidence` (`support/integrations.toml`) |
| `slipfork test [-j N]` | `M3.2` Phase 2 target | Orchestrate `STP v1` DAG stages across parallel hermetic Btrfs `TestSlot` worlds (`SLIPFORK_PROTOCOL_FD`, `§2`) |

### 1.7 Host Preflight — `slipfork doctor` Btrfs Probe & Fail-Closed Mount Hint (`M3.15`)

Slipfork runs on Btrfs only: `<state_dir>` must sit on a Btrfs mount created with `mount -o user_subvol_rm_allowed,noatime`, because every world, snapshot, and artifact is an unprivileged `$UID:$GID` subvolume created, snapshotted, and destroyed through Btrfs ioctls. `slipfork doctor` runs **locally, before the daemon**: the filesystem and subvolume checks come from the same Btrfs probe owner `slipforkd` boots against (`SubvolumeDriver`, `ADR-0010` §4 is normative; `M3.15`), and the namespace checks come from the injected `HostProbe` (`probe_userns` + `probe_netns`), so `doctor` still explains a broken host when `slipforkd` refuses to boot:

```console
$ slipfork doctor
state_dir: /home/acme/.cache/slipfork
filesystem: btrfs
btrfs_super_magic: ok
subvolume_ioctl: ok
noatime: ok
unshare_userns: ok
netns: ok
```

- **Field provenance — no invented values:** `state_dir` is the requested state root and `filesystem` is `doctor`'s own label for the report's raw `fstype` code (`0x9123683e` -> `btrfs`, `0x58465342` -> `xfs`, `0xef53` -> `ext4`, `0x01021994` -> `tmpfs`, `0x794c7630` -> `overlayfs`, else `unknown(0x<hex>)`); `--json` carries both the label and `fstype` as `"0x<lowercase-hex>"`. Line order, and probe values unrelated to a failure, are illustrative — the `--json` keys are the contract.
- `filesystem` / `btrfs_super_magic` come from one `statfs` of `<state_dir>` (or its nearest existing ancestor, creating nothing on a non-Btrfs host); anything but `BTRFS_SUPER_MAGIC` (`0x9123683e`) is a hard failure.
- `subvolume_ioctl` is a live scratch round-trip under `<state_dir>/.doctor-probe-<pid>` (create -> read-only snapshot -> clear read-only -> destroy), proving unprivileged `user_subvol_rm_allowed` instead of advertising a cached capability; it renders `ok`, `failed (errno <n>)` with the mount hint, or `skipped` when the filesystem is not Btrfs.
- `noatime` reports the `ST_NOATIME` mount-option evidence behind the same mount hint; it is advisory only — the required checks are the Btrfs magic, the unprivileged subvolume round-trip, `unshare_userns`, and `netns` (`ADR-0010` §4).
- `unshare_userns` reports the identity-mapping probe (`unshare --user --map-user=<uid> --map-group=<gid>`); `failed` under `kernel.apparmor_restrict_unprivileged_userns=1` carries the hint naming the missing `/etc/apparmor.d/slipfork` `userns,` profile (`slipfork daemon unit --only apparmor`).
- `netns` proves the world network namespace can bring up loopback (`--net --ipc --pid`), and is `skipped` whenever `unshare_userns` is not `ok`.
- **No `crun` line, because `crun` is not a host dependency:** static `crun` is a world-rootfs asset (`ADR-0009` §1) — `slipforkd` provisions the compile-time SHA-256-pinned release into `<state_dir>/cache/runtime/` and `BaseImageBuilder` stages it at `/.slipfork/bin/crun`, failing closed on a digest mismatch when the rootfs is materialized. A host `PATH` probe would test the wrong machine, so `doctor` performs none (`ADR-0010` §4).

A non-Btrfs state dir fails closed (`exit 1`) with the one fix that matters and zero fallback (wording illustrative; the pinned string is `mount -o user_subvol_rm_allowed,noatime`):

```console
$ slipfork doctor
state_dir: /home/acme/.cache/slipfork
filesystem: xfs
btrfs_super_magic: mismatch
subvolume_ioctl: skipped
noatime: ok
unshare_userns: ok
netns: ok
slipfork: /home/acme/.cache/slipfork is not a Btrfs filesystem; Slipfork has no non-Btrfs mode
  fix: point --state-dir at a Btrfs mount made with mount -o user_subvol_rm_allowed,noatime
```

`--json` emits the typed `DoctorReport`; `ADR-0010` §4 is the single home for its key set and exit rule. The one nuance worth repeating here: a `skipped` probe (`subvolume_ioctl` on a non-Btrfs host, `netns` when `unshare_userns` is not `ok`) is never `ok`, so it exits `1` and lists the remediation in `fixes`.

**Three-stage enforcement — no false boot parity (`ADR-0010` §4):**

1. **`slipforkd` boot** — `ensure_supported`, magic-only: it proves the filesystem type and nothing else, and fails closed with `EngineError::BtrfsRequired` plus the mount hint before any world directory exists.
2. **`slipfork doctor`, pre-daemon** — the scratch round-trip is the only place unprivileged `user_subvol_rm_allowed` is proven, and it is the reason to run `doctor` before the first `up`.
3. **First subvolume teardown** — a Btrfs state dir mounted **without** `user_subvol_rm_allowed` passes boot, so a half-created world tree is still possible: the first `BTRFS_IOC_SNAP_DESTROY_V2` (`destroy_world`, `prune`, or the transient base-seed cleanup) fails closed on `EPERM` with the same `mount -o user_subvol_rm_allowed,noatime` hint.

---

## 2. World-Scoped `[resources]` `ResourceStrategy` & `M3.2` Phase 2 Testing (`slipfork.toml`, `STP v1`)

Every `M3.2` promise below is Phase 2 target state (`ADR-0006` §3 is normative for `STP v1`; `ADR-0006` §5 for `SLIPFORK_OUTPUT_DIR`).

Zero `[resources]` lines are needed for the standard workflow (`local` worlds default to `"prefer_remote"`, hermetic `TestSlot` worlds default to `"local"` via `ResourcePlan::resolve`). Declare `[resources]` **only** when overriding a default:

```toml title="slipfork.toml"
project = "acme"

# Deliberate overrides only (omit entirely to keep prefer_remote in local & hermetic local in test):
[resources.s3_buckets.media]
local = "remote" # Require OSPP shared dev S3 in up/fork; fail closed when offline

[fixtures.migrate]
inputs = ["api/migrations/*.sql"]
run = "uv run python manage.py migrate"

[fixtures.seed_catalog]
parent = "migrate"
inputs = ["api/fixtures/catalog.json"]
run = "uv run python manage.py loaddata catalog" # origin-independent -> reused across worlds

[fixtures.seed_sites]
parent = "seed_catalog"
per_world = true                                  # per-world seed runs once the world's URL is known
inputs = ["api/fixtures/configure_site.py"]
run = "uv run python api/fixtures/configure_site.py"

[test]
coverage = true
coverage_report = ["html", "xml"]
coverage_fail_under = 85
```

- **Hermetic-by-Construction `TestSlot` Worlds:** `ResourceMode::Test` derives from `WorldTarget::TestSlot` (`ADR-0008` §3), so pre-warmed `STP v1` test worlds never touch shared dev S3 or dev DB dumps unless `test = "remote"` / `"prefer_remote"` is set on that resource.
- **`STP v1` — What `slipfork test` Orchestrates:** `slipfork test -j N` speaks the **Slipfork Testing Protocol (`STP v1`)** over an inherited `SLIPFORK_PROTOCOL_FD` (an `AF_UNIX` `socketpair`); frameworks expose nodes of the global DAG, including **persistent setup/fixture stage nodes** that survive their own setup process as **read-only Btrfs subvolume snapshots** (`ADR-0010` §3), composed with the global `[artifacts.*]` / `[fixtures.*]` nodes. Coverage (typed `[test]` schema) and mutation testing (`mutants`) are **first-class protocol concepts**, keeping the Rust orchestrator language-blind: no per-language Rust drivers, no CLI-flag guessing, and no published plugin packages — `M3.2` proves the protocol with external, unpublished ad-hoc `pytest`, `vitest`, and `phpunit` plugins.
- **Blastit, Playwright Traces & Allure (`SLIPFORK_OUTPUT_DIR`):** Each test world mounts a slot-scoped `SLIPFORK_OUTPUT_DIR` (`ADR-0006` §5) that survives world teardown, keeping `slipfork.toml` 100% free of shell pipelines.
