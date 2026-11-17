# Slipfork M2–M3 Target Developer Experience Specification

- **Status:** Accepted Target DX Specification (`ADR-0001`..`ADR-0011`, `H-001`..`H-011`; `0.1.0` UX transcript of record in `docs/design/dx-0.1.0-ux.md`)
- **Visibility:** Internal design spec (`exclude_docs` in `zensical.toml`). Per **Core Rule #4 (Zero Vaporware in Published Docs)**, sections here promote into published `docs/` and `README.md` **only** as each binary capability ships and passes conformance tests (`M2.19`, `M3.18`, `M3.28.14`).

---

## 1. World Lifecycle DX (`slipforkd`, `:80` `daemon install`, Bare `status`, `restart`, `OSPP v1`, Btrfs Preflight & Rescue Tooling)

### 1.1 Port-80 System Socket Activation (`slipfork daemon install`) vs Rootless Auto-Spawn

Installing the workspace places `slipfork`, `slipforkd`, `slipfork-world-init`, and `git-remote-slipfork` onto `PATH` (source install today):

```console
$ cargo install --git https://github.com/OffworldNexus/slipfork slipfork slipfork-engine

# Recommended workstation / VM setup (PID 1 systemd binds 127.0.0.1:80 + [::1]:80 + 127.0.0.1:7080,
# loads /etc/apparmor.d/slipfork userns profile, and starts slipforkd as the invoking developer
# account resolved through SUDO_USER; ADR-0011). `--user <account>` is the override for a host
# whose daemon account is not the operator's login account:
$ sudo slipfork daemon install

# Zero-setup rootless fallback (containers & CI without root/systemd):
# Running `slipfork up` or `slipfork fork` — and only those two — automatically starts
# `slipforkd --listen 127.0.0.1:0 --state-dir <state_dir> --runtime-dir <state_dir>/run`
# on <state_dir>/run/daemon.sock (0600), prints http://demo.localhost:<port>, and emits one
# actionable stderr line naming `sudo slipfork daemon install`. Read verbs never spawn a
# daemon; they fail closed with the one-line remedy in `docs/design/dx-0.1.0-ux.md` §1.
```

`slipforkd` runs **unprivileged as `User=<developer-user>`** (resolved via `crates/slipfork/src/user.rs` into non-template `slipforkd.service`, with `M3.28.6` adding `Requires=slipforkd.socket`, `After=slipforkd.socket`, and `WantedBy=multi-user.target` alongside its empty `CapabilityBoundingSet=`, `ADR-0011` §1) and acts as the single local authority (`H-010` #1): it binds `<state_dir>/run/daemon.sock` (`0600`; `Layout::for_account` gives clients and `slipforkd.service` the same `<state_dir>/run`, `ADR-0011` §2), holds the persistent `OSPP v1` WebSocket session, resolves `snow.yml` secrets/resources, supervises Btrfs subvolume worlds, and routes `.localhost` traffic via `snow.yml`'s `ingresses` longest-prefix table (`0.1.0` has one origin type, `struct WorldOrigin(LocalOrigin)` — tombstone in `ADR-0011`). **OSPP is never required to boot local worlds:** offline laptops work out of the box from `snow.yml` + `~/.local/share/slipfork/vaults/<project>.json` (`0600`; `slipfork.toml` is optional).

### 1.2 `slipfork up` vs `slipfork restart`, Compact `lineage` Line, Verbose `-v` & Seed Dump Flags (`--no-dump | --refresh-dump`)

**One boot pipeline, two verbs:** `up` passes the caller's explicit `cwd` over `<state_dir>/run/daemon.sock` to run the world boot pipeline for an unbooted, `stopped`, or `failed` world selected by `--world` or `cwd` (§1.6) — waiting for `BtrfsEngine::spawn_service`'s injected `ReadinessProbe` (`ADR-0002` §2), printing the actionable world block plus **one** compact `lineage` line (`WorldLineage`), and returning idempotently (`changed: false`, touching nothing) when already `running`; **`slipfork restart`** (`up --watch` and `restart --watch` do not exist in `0.1.0`) stops components and `crun` backing services in place while keeping the worktree and CoW state subvolume, then runs that **exact same** boot pipeline (`changed: true`):

```console
$ cd ~/demo
$ slipfork up
world     demo (running, changed)
app       http://demo.localhost
worktree  /home/slipfork/demo
next      slipfork open | slipfork db | slipfork logs | slipfork restart | slipfork down
lineage   rootfs a1b2c3d4e5f6 cached · postgres:18 cached · seed 9f8e7d6c5b4a rebuilt 2.1s · base 4570132bffb8 sealed 10ms

$ slipfork restart
world     demo (running, changed)
app       http://demo.localhost
worktree  /home/slipfork/demo
restarted pg_clusters.psql, api
next      slipfork open | slipfork db | slipfork logs | slipfork restart | slipfork down
lineage   rootfs a1b2c3d4e5f6 cached · postgres:18 cached · seed 9f8e7d6c5b4a cached · base 4570132bffb8 reused

$ slipfork up --no-dump                 # Skip seed dump and run clean initdb -> pre-deploy migrations
$ slipfork up --refresh-dump            # Require live OSPP dump check (mutually exclusive with --no-dump; fails closed if offline)
$ slipfork up --resources local         # Force hermetic world-local S3 + clean initdb (safe for agent runs)
$ slipfork -v up                        # Adds per-node [lineage] + [resources] rows to human output (dx §8.2)
```

A second `slipfork up` on a world that is already `running` prints the `unchanged` block — `docs/design/dx-0.1.0-ux.md` §4 owns that transcript; it is not restated here.

- **Verbose-only `[resources]` lines (`-v` / `--verbose`, `M3.28.11`):** healthy default boots print zero internal `[resources]` jargon; with `-v` / `--verbose`, each row prints `[resources] <id>: <strategy> -> <resolved> (<reason>)` with the `snow.yml` resource id verbatim (`media`, `main`) and the `snake_case` labels of `ResourceStrategy` / resolved target / reason (`local|prefer_remote|remote`, `local|remote`, `configured|mode_default|not_offered|no_dump|refreshed_dump`). `-v` changes human rendering only; `--json` is the maximal machine document and never mixes these rows into stdout.
- **Offline / Unpaired Transparency:** When unpaired or offline under default `prefer_remote`, `ResourcePlan::resolve` never degrades silently: it returns the typed `{strategy, resolved, reason}` triple (`prefer_remote`, `local`, `not_offered`), carried in `--json` `resources` and rendered under `-v` / `--verbose`.
- **Loud One-Line Notice When Ingress Is Not `:80` (`ADR-0011` §3):** See `docs/design/dx-0.1.0-ux.md` §2 rule 4 (the single normative owner across `status`, `up`, `fork`, `restart`, `list`, and `open`).
- **Port-Aware Single Origin & In-Netns Loopback Bridge (`H-010` #2, `H-011` #4):** `WorldOrigin::local(&route, ingress_port)` renders `http://demo.localhost` on `:80` and `http://demo.localhost:<port>` under auto-spawned `--listen`, while the world's PID 1 `slipfork-world-init` (spawned by `BtrfsEngine`, `M3.13`, `ADR-0010` §6) binds an **in-netns loopback TCP bridge** on `127.0.0.1:<ingress_port>` inside each world and splices accepted streams to `slipforkd`'s `0600` `<state_dir>/http-ingress.sock`, so `$BASE_URL` resolves identically from host browsers and inside `slipfork exec` (and, in `M3.2` Phase 2 target state, `slipfork test`) (`snow.yml` `ingresses` prefixes route `/api`, `/`, etc. to their target component ports). **Never connect to host `127.0.0.1:5432` or `:8000`**; use the printed `origin`, `slipfork exec`, or `slipfork forward --json`.
- **World Rootfs & `--host-caches` (design of record: `ADR-0009` §1–§3; operational detail in `docs/design/world-base-image.md`):** every world's `L0` rootfs is built once from the project's own `snow.yml` `images:` `Dockerfile` stage, selected by `[world]` in `slipfork.toml` (`ADR-0009` §1: `world.image` / `world.stage`), seeded into the transient `<state_dir>/worlds/seed-<l0_sha12>` subvolume and sealed as the read-only base subvolume `<state_dir>/snapshots/<project>-base-<l0_sha12>`, whose `<subvol>/rootfs` tree is mounted read-only at `/`; every fork and test slot is then one `BTRFS_IOC_SNAP_CREATE_V2` clone of that base (`ADR-0010` §2–§3). `postgres:<v>`, `redis:<v>`, and SeaweedFS run as daemonless `crun` containers sharing the world's `127.0.0.1`. `--host-caches` adds the host toolchain caches **read-only** only when the ELF/libc ABI probe matches, failing closed with the measured mismatch otherwise — it is never a `slipfork.toml` key.

### 1.3 First-Class Hook Failure Rescue UX & Direct CLI TTY Entry (`hook run`, `logs`, `shell`, `exec`)

Hooks (`<component>:<phase>`, e.g. `api:pre-deploy`) stream stdout/stderr live from `slipforkd`. On non-zero exit, Slipfork prints the **full un-truncated output**, **preserves the failed world** (listed in `slipfork list`), and prints three copy-pasteable rescue commands using the `--world` selector (§1.6):

```console
$ slipfork fork billing
[hook]      api:pre-deploy: sh -eu -c uv run python manage.py migrate
... (full untruncated traceback) ...
error: hook 'api:pre-deploy' exited with code 1 in world 'demo:billing'
  argv: ["sh", "-eu", "-c", "uv run python manage.py migrate"]
  log:  /home/slipfork/.cache/slipfork/demo/worlds/demo:billing/logs/api.pre-deploy.log

Rescue commands (world 'demo:billing' kept alive for inspection):
  slipfork hook run api:pre-deploy --world billing          # add --no-tty in non-interactive shells
  slipfork restart --world billing                          # re-run full boot after editing
  slipfork logs --world billing api:pre-deploy
  slipfork shell --world billing                            # or: slipfork exec --world billing -- <cmd>
```

- **Retained log paths join the world id (not the bare fork):** `slipforkd` writes `<state_dir>/<project>/worlds/<world_id>/logs/<component>.<phase>.log` — the shipped `Layout::world_logs_dir` join, a daemon-owned plain directory outside the world's CoW subvolume (`ADR-0010` §2), so a failed world's logs outlive the world they came from. Nothing reaps them automatically in Phase 1 (`prune` releases world records only), so delete the directory to reclaim space. The `log:` line above is the real path on disk and stays unambiguous when a project has both the `<project>` and `<project>:<fork>` worlds.
- **Direct CLI World Entry (`H-010` #1, `M3.28.11`):** `slipforkd` is the **sole owner** of `snow.yml` (`modes.local`) environment resolution (`@slipfork/opencode` only routes `shell` calls to `slipfork exec --cwd <cwd>`, `ADR-0004`). For `shell`, interactive `hook run` (`pdb`), and `exec`, the daemon returns the world's entry command — `nsenter --target <init_pid> --user --preserve-credentials --mount --net --ipc --pid --wd=...` into the world's `slipfork-world-init` PID 1 (`ADR-0010` §6; `WorldEntryCommand.cwd = "/"` so `sh: 0: getcwd() failed` never appears) — plus the resolved environment; the thin `slipfork` CLI then enters the world inheriting its native terminal `stdin`/`stdout`/`stderr`, so that CLI ↔ `slipforkd` boundary carries **zero file descriptors** (the in-daemon `netns.sock` bridge is a separate concern, `ADR-0010` §2). Interactive `slipfork shell` prints one orientation line (`world rootfs has no host tools (no ps, no curl); exit and run slipfork ps | slipfork logs | slipfork open`), and `slipfork logs` prefixes multi-source output with `<source>: `. `db` / `redis` enter their service container instead (§1.4). `shell` and default `hook run` require a TTY, failing closed naming `hook run --no-tty` or `slipfork exec`.

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

### 1.5 Lifecycle (`status`, `list`, `down`, `prune`) & `--json` Contract Pointer

`docs/design/dx-0.1.0-ux.md` is the **single normative home** for all `0.1.0` interactive transcripts (§§1–9), the CLI `--json` projection and per-command schemas (§3 & §10), and the `0.1.0` CLI command table (§10).

```console
$ slipfork list
WORLD         STATE     ORIGIN                          NEXT
demo          running   http://demo.localhost           slipfork open --world demo
demo:billing  failed    http://billing.demo.localhost   slipfork logs --world demo:billing

$ slipfork down --world billing            # Stop world services while keeping .worktrees/billing
$ slipfork prune                           # Preview orphaned worlds (--yes to delete)
```

- **One world spelling & single `Local` origin:** every `--json` `"world"` field uses the same `world_id` as the `WORLD` column (§1.6); table columns are padded, never truncated, and `--json` carries the full values. `0.1.0` ships untagged `struct WorldOrigin(LocalOrigin)` exclusively (tombstone in `ADR-0011`).

### 1.6 World Selection Rules & Command Surface Pointer

The complete `0.1.0` CLI command table lives solely in **`docs/design/dx-0.1.0-ux.md` §10** (`M3.2` Phase 2 `0.2.0` adds `slipfork test [-j N] [--dry-run]` per §2 below).

- **World selection — one rule (`M3.15`, `ADR-0005` §3):** every world-scoped command resolves its world against the request's `cwd` repository (`RepoRoot.project`) through the shipped private `daemon::resolve_world_selector` parser (the typed `WorldId` plus a shared `project::` location is `0.2.0` target state), so **every same-project id printed by `slipfork list` or any `--json` payload is accepted verbatim**:
  - `<project>:<fork>` (e.g. `demo:billing`) — must match the `cwd` repository's project; a cross-project id fails closed with a remedy (`cd` into that project's checkout — `slipfork status --json` carries each world's `worktree` — or `slipfork exec --cwd <worktree> --world demo:billing`). Matching is by case, and `<project>:<project>` is accepted as the user fork `<project>` (the colon disambiguates it); a bare `<fork>` equal to the project name is rejected only when creating one via `slipfork fork`.
  - `<project>` (e.g. `demo`) — selects the project's own world even from inside `.worktrees/<fork>`.
  - `<fork>` (e.g. `billing`) — repo-local shorthand for `<project>:<fork>`, validated by `validate_fork_name` against the project's real `snow.yml` `domains[].id` slice (invalid DNS labels, `domains[].id` collisions, and reserved `test-*` slots fail closed). A bare label that is not this repository's project is always a fork name, never another project's world.
  - Omitting `--world` resolves `cwd`'s nearest ancestor: `<vcs_root>/.worktrees/<fork>[/...]` -> `<fork>`, repo root -> `<project>`, outside any repository -> fail closed naming `cd <repo>` or `slipfork exec --cwd <repo>`.
- **Positional forms:** `slipfork fork [fork]` takes an optional bare `<fork>` (omitting `[fork]` derives it from the current `git` branch or `jj` bookmark in `slipforkd`, failing closed on detached `HEAD` or inside `.worktrees/<fork>`; internal dirty snapshots pin `-c user.name=slipfork -c user.email=slipfork@localhost`, `M3.28.5`/`M3.28.10`), while `slipfork drop <fork>` requires `<fork>` and also accepts `<project>:<fork>`.

### 1.7 Host Preflight — `slipfork doctor` Btrfs Probe & Fail-Closed Mount Hint (`M3.15`)

Slipfork runs on Btrfs only: `<state_dir>` must sit on a Btrfs mount created with `mount -o user_subvol_rm_allowed,noatime`, because every world, snapshot, and artifact is an unprivileged `$UID:$GID` subvolume created, snapshotted, and destroyed through Btrfs ioctls. `slipfork doctor` runs **locally, before the daemon**: the filesystem and subvolume checks come from the same Btrfs probe owner `slipforkd` boots against (`SubvolumeDriver`, `ADR-0010` §4 is normative; `M3.15`), and the namespace checks come from the injected `HostProbe` (`probe_userns` + `probe_netns`), so `doctor` still explains a broken host when `slipforkd` refuses to boot:

```console
$ slipfork doctor
state_dir: /home/slipfork/.cache/slipfork
filesystem: btrfs
btrfs_super_magic: ok
subvolume_ioctl: ok
noatime: ok
unshare_userns: ok
netns: ok
ingress: :80 active (port-less origins)
```

- **Field provenance — no invented values:** `state_dir` is the requested state root and `filesystem` is `doctor`'s own label for the report's raw `fstype` code (`0x9123683e` -> `btrfs`, `0x58465342` -> `xfs`, `0xef53` -> `ext4`, `0x01021994` -> `tmpfs`, `0x794c7630` -> `overlayfs`, else `unknown(0x<hex>)`). Line order, and probe values unrelated to a failure, are illustrative; the `--json` document and its always-present keys are `docs/design/dx-0.1.0-ux.md` §10.
- `filesystem` / `btrfs_super_magic` come from one `statfs` of `<state_dir>` (or its nearest existing ancestor, creating nothing on a non-Btrfs host); anything but `BTRFS_SUPER_MAGIC` (`0x9123683e`) is a hard failure.
- `subvolume_ioctl` is a live scratch round-trip under `<state_dir>/.doctor-probe-<pid>` (create -> read-only snapshot -> clear read-only -> destroy), proving unprivileged `user_subvol_rm_allowed` instead of advertising a cached capability; it renders `ok`, `failed (errno <n>)` with the mount hint, or `skipped` when the filesystem is not Btrfs.
- `noatime` reports the `ST_NOATIME` mount-option evidence behind the same mount hint; it is advisory only — the required checks are the Btrfs magic, the unprivileged subvolume round-trip, `unshare_userns`, and `netns` (`ADR-0010` §4).
- `unshare_userns` reports the identity-mapping probe (`unshare --user --map-user=<uid> --map-group=<gid>`); `failed` under `kernel.apparmor_restrict_unprivileged_userns=1` carries the hint naming the missing `/etc/apparmor.d/slipfork` `userns,` profile (`sudo slipfork daemon install`).
- `netns` proves the world network namespace can bring up loopback (`--net --ipc --pid`), and is `skipped` whenever `unshare_userns` is not `ok`.
- `ingress` (`IngressProbe`, `M3.28.6`, `ADR-0011` §3) reports `:80 active (port-less origins)`, ``installed but inactive (run `sudo slipfork daemon install`)``, or ``not installed (ephemeral port; run `sudo slipfork daemon install`)``; like `noatime`, it is advisory so `doctor` exits `0` on unprivileged hosts without systemd units.
- **No `crun` line, because `crun` is not a host dependency:** static `crun` is a world-rootfs asset (`ADR-0009` §1) — `slipforkd` provisions the compile-time SHA-256-pinned release into `<state_dir>/cache/runtime/` and `BaseImageBuilder` stages it at `/.slipfork/bin/crun`, failing closed on a digest mismatch when the rootfs is materialized. A host `PATH` probe would test the wrong machine, so `doctor` performs none (`ADR-0010` §4).

A non-Btrfs state dir fails closed (`exit 1`) with the one fix that matters and zero fallback (wording illustrative; the pinned string is `mount -o user_subvol_rm_allowed,noatime`):

```console
$ slipfork doctor
state_dir: /home/slipfork/.cache/slipfork
filesystem: xfs
btrfs_super_magic: mismatch
subvolume_ioctl: skipped
noatime: ok
unshare_userns: ok
netns: ok
ingress: not installed (ephemeral port; run `sudo slipfork daemon install`)
slipfork: /home/slipfork/.cache/slipfork is not a Btrfs filesystem; Slipfork has no non-Btrfs mode
  fix: point --state-dir at a Btrfs mount made with mount -o user_subvol_rm_allowed,noatime
```

`--json` emits `doctor`'s document, which is `docs/design/dx-0.1.0-ux.md` §10's row for `doctor`; `ADR-0010` §4 owns the exit rule. The one nuance worth repeating here: a `skipped` probe (`subvolume_ioctl` on a non-Btrfs host, `netns` when `unshare_userns` is not `ok`) is never `ok`, so it exits `1` and lists the remediation in `fixes`.

**Three-stage enforcement — no false boot parity (`ADR-0010` §4):**

1. **`slipforkd` boot** — `ensure_supported`, magic-only: it proves the filesystem type and nothing else, and fails closed with `EngineError::BtrfsRequired` plus the mount hint before any world directory exists.
2. **`slipfork doctor`, pre-daemon** — the scratch round-trip is the only place unprivileged `user_subvol_rm_allowed` is proven, and it is the reason to run `doctor` before the first `up`.
3. **First subvolume teardown** — a Btrfs state dir mounted **without** `user_subvol_rm_allowed` passes boot, so a half-created world tree is still possible: the first `BTRFS_IOC_SNAP_DESTROY_V2` (`destroy_world`, `prune`, or the transient base-seed cleanup) fails closed on `EPERM` with the same `mount -o user_subvol_rm_allowed,noatime` hint.

---

## 2. World-Scoped `[resources]` `ResourceStrategy` & `M3.2` Phase 2 Testing (`slipfork.toml`, `STP v1`)

Every `M3.2` promise below is Phase 2 target state (`ADR-0006` §3 is normative for `STP v1`; `ADR-0006` §5 for `SLIPFORK_OUTPUT_DIR`).

Zero `[resources]` lines are needed for the standard workflow (`local` worlds default to `"prefer_remote"`, hermetic `TestSlot` worlds default to `"local"` via `ResourcePlan::resolve`). Declare `[resources]` **only** when overriding a default:

```toml title="slipfork.toml"
project = "demo"

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
