# Slipfork `0.1.0` UX Specification: Port-Less `:80` Ingress, Bare `status`, `restart`, `ps`, `open`, Branch-Named `fork`, and `lineage`

- **Status:** Accepted `0.1.0` Target DX Specification (`M3.28.1`, `ADR-0011`; **single normative home** for the `0.1.0` CLI command table and `--json` schemas)
- **Visibility:** Internal design blueprint (`exclude_docs` in `zensical.toml`). Promoted to published `docs/` and `README.md` only in `M3.28.14` after live verification (`M3.28.13`).

---

## 1. The Four-Command Fresh Experience (`:80` Ingress Active)

Once `slipfork` is installed and `sudo slipfork daemon install` has run once on the host (`§2`), every project origin is port-less (`http://<project>.localhost`), bare `slipfork` renders the live status dashboard, and editing code takes one command (`slipfork restart`):

```console
$ cd ~/demo
$ slipfork up
world     demo (running, changed)
app       http://demo.localhost
worktree  /home/slipfork/demo
db        slipfork db
logs      slipfork logs
next      slipfork open  |  slipfork restart
lineage   rootfs a1b2c3d4e5f6 cached · postgres:18 hit · seed 9f8e7d6c5b4a rebuilt 2.1s · base 4570132bffb8 sealed 10ms

$ curl -fsS http://demo.localhost/
{"hello": "world", "world": "demo"}

$ slipfork
slipforkd 1420, ingress 127.0.0.1:80, [::1]:80
  demo            running   http://demo.localhost              /home/slipfork/demo
                  lineage   rootfs a1b2c3d4e5f6 · base 4570132bffb8 · seed 9f8e7d6c5b4a
                  -> slipfork open | slipfork restart | slipfork logs | slipfork down
```

- **Zero happy-path noise:** `[resources]` resolution rows (`prefer_remote -> local (not_offered)`) and internal namespace tags never print on a normal boot; pass global `-v` / `--verbose` (or `--json`) when inspecting per-node lineage and resource selection.
- **One canonical runtime directory (`<state_dir>/run`):** `Layout::for_account(&ResolvedAccount, ...)` derives `state_dir` (`--state-dir` -> `$XDG_CACHE_HOME/slipfork` -> `<home>/.cache/slipfork`) and defaults `runtime_dir` unconditionally to `<state_dir>/run` (`--runtime-dir <DIR>` is the sole override); `Layout::resolve` resolves the invoking account and calls `Layout::for_account` so there is never a second layout derivation. Deleting the `$XDG_RUNTIME_DIR/slipfork` branch guarantees login shells, `git-remote-slipfork`, and `slipforkd.service` always meet on `<state_dir>/run/daemon.sock` (`0600`).

---

## 2. Port-80 System Socket Activation: `slipfork daemon install` & `slipfork doctor`

On Linux (`net.ipv4.ip_unprivileged_port_start = 1024`), neither an unprivileged process nor `systemd --user` can bind port `80`. `slipfork daemon install` (`M3.28.6`) is the **single** command that installs `/etc/systemd/system/slipforkd.socket` (`ListenStream=127.0.0.1:80`, `[::1]:80`, `127.0.0.1:7080`), `/etc/systemd/system/slipforkd.service` (`User=<account>`, empty `CapabilityBoundingSet=`, plus `M3.28.6`'s new `Requires=slipforkd.socket`, `After=slipforkd.socket`, and `WantedBy=multi-user.target` in `render_service_unit` and `crates/slipfork-edge/tests/golden/slipforkd.service`), and `/etc/apparmor.d/slipfork` (`userns,`), then activates both units immediately and across reboots:

```console
$ sudo slipfork daemon install --user slipfork
[wrote]   /etc/systemd/system/slipforkd.socket
[wrote]   /etc/systemd/system/slipforkd.service
[wrote]   /etc/apparmor.d/slipfork
[enabled] slipforkd.socket, slipforkd.service (User=slipfork, ingress 127.0.0.1:80 + [::1]:80)

$ sudo slipfork daemon install --user slipfork
[ok]      /etc/systemd/system/slipforkd.socket unchanged
[ok]      /etc/systemd/system/slipforkd.service unchanged
[ok]      /etc/apparmor.d/slipfork unchanged
[enabled] slipforkd.socket, slipforkd.service (User=slipfork, ingress 127.0.0.1:80 + [::1]:80)
```

### Locked `daemon install` & `daemon unit` Rules

1. **Identity, root policy & `Layout::for_account` (`crates/slipfork/src/user.rs`):** `slipfork daemon install [--user <ACCOUNT>] [--bin-dir <DIR>]... [--forward-port <PORT>]` (no `--force`).
   - Install orchestration is a pure function taking `is_root: bool` and `explicit_user: Option<&str>`.
   - When `!is_root`, `UserIdentity::username()` (`id -un`, `env_clear`) resolves `<account>` and `daemon install` fails closed before touching `UnitInstaller`:
     ```console
     $ slipfork daemon install
     slipfork: daemon install writes /etc and enables system units; run it as root (sudo slipfork daemon install --user slipfork)
     ```
   - When `is_root`, `explicit_user` falls back to `UserIdentity::invoking_account()` (`crates/slipfork/src/user.rs` is the **only** module permitted to read `SUDO_USER`, under its fixed `PATH`, validating the name through `UserIdentity::lookup_account`). Direct root with neither `--user` nor a valid non-root `invoking_account()`, or `--user root` (`uid == 0`), fails closed (`slipfork: slipforkd runs as an unprivileged developer account, never root; run sudo slipfork daemon install --user <account>`).
   - `UserIdentity::lookup_account` resolves the target account's `ResolvedAccount { name, uid, gid, home }` (rejecting unknown accounts, `%`-specifiers, and `uid == 0`), and `Layout::for_account` computes the unit's `state_dir` and `runtime_dir` (`<state_dir>/run`).
2. **Exact product renderers & flat repeatable `--bin-dir`:** `daemon install` writes the **exact bytes** produced by `slipfork daemon unit`. `daemon unit` gains repeatable `--bin-dir <DIR>` (default: `current_exe()` parent directory), emitting one flat `<rel_dir>/slipfork{,d,-world-init}` alternative per `--bin-dir` inside the single 4-line AppArmor brace expression (never nested braces):
   ```text
   abi <abi/4.0>,
   profile slipfork /{usr/local/bin/slipfork{,d,-world-init},tmp/ux/bin/slipfork{,d,-world-init},usr/bin/unshare,.slipfork/bin/crun} flags=(unconfined) {
     userns,
   }
   ```
3. **Narrow `UnitInstaller` port & clean `:0` handover (`crates/slipfork/src/daemon_unit_install.rs`):** All OS writes and unit transitions go through narrow `UnitInstaller` verbs (`read_file`, `write_file_atomic`, `daemon_reload`, `apparmor_replace`, `enable_now`, `restart_service` — zero stringly `run(program, args)` bag). Each file is written via a same-directory temp file + `rename` **only** when its bytes differ from disk. Before `enable_now(&[ManagedUnit::Socket, ManagedUnit::Service])`, if `<state_dir>/run/daemon.sock` is currently answered by a non-`:80` auto-spawned `slipforkd`, `daemon install` sends `DaemonControl::shutdown` and awaits socket release so `slipforkd.service` never hits `EdgeError::SocketInUse` (`restart_service` runs when unit bytes changed while already active).
4. **Loud one-line notice when ingress is not `:80` (single normative owner):** Rootless auto-spawn (`slipforkd --listen 127.0.0.1:0`) remains available for unprivileged CI and containers, where `WorldOrigin::local` formats `http://<host>:<port>`. Whenever `DaemonStatus.ingress.port != 80`, the six world/daemon inspection and entry commands — **`status`, `up`, `fork`, `restart`, `list`, and `open` (including `open --remote`)** — emit **exactly one** line to **stderr** (never stdout, never under `--json`):
   ```text
   slipfork: ingress is :40645, not :80 — `slipfork daemon install` binds 127.0.0.1:80 + [::1]:80 via systemd, so demo.localhost works without a port
   ```
5. **Pre-daemon `slipfork doctor` ingress line (`IngressProbe`):** `doctor` reports `:80` socket activation status alongside the four required Btrfs/namespace checks (advisory — `DoctorReport.ok` stays `true` on unprivileged hosts without systemd units):
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
   The other two states render ``ingress: not installed (ephemeral port; run `slipfork daemon install`)`` and ``ingress: installed but inactive (run `sudo systemctl start slipforkd.socket`)``.

---

## 3. Bare `slipfork`, `slipfork status`, and the Daemon-vs-CLI Wire Boundary

`Cli.command` becomes `Option<Commands>` in `clap`: invoking bare `slipfork` runs the exact same handler as `slipfork status`. `slipfork --help` and `slipfork help` retain the full `Commands:` list (24 subcommands + `help` = 25 entries, with `dag` and the unimplemented outbound-sharing verb deleted per `§§8–9`) parsed by `tools/check_docs_honesty.py`.

```console
$ slipfork status
slipforkd 1420, ingress 127.0.0.1:80, [::1]:80
  demo            running   http://demo.localhost                   /home/slipfork/demo
                  lineage   rootfs a1b2c3d4e5f6 · base 4570132bffb8 · seed 9f8e7d6c5b4a
                  -> slipfork open | slipfork restart | slipfork logs | slipfork down
    :feat-ux      running   http://feat-ux.demo.localhost           /home/slipfork/demo/.worktrees/feat-ux
                  lineage   rootfs a1b2c3d4e5f6 · base 4570132bffb8 · seed 9f8e7d6c5b4a
                  -> slipfork open --world feat-ux | slipfork restart --world feat-ux | slipfork drop feat-ux
```

When zero worlds exist:

```console
$ slipfork
slipforkd 1420, ingress 127.0.0.1:80, [::1]:80
  no worlds running — cd into a checkout and run `slipfork up`
```

- **Lineage digests on `status`:** Each world renders short 12-hex prefixes (`rootfs <digest12> · base <key12> · seed <digest12>`) from its own `WorldSummary.lineage`, printing `-` for any `None` field (never `0` or `""`); there is no daemon-wide lineage cell, so one world never displays another world's cache facts.
- **Daemon wire model vs CLI `--json` projection (stated once, shared everywhere):**
  - Over `<state_dir>/run/daemon.sock`, `DaemonControl` exchanges typed Rust values (`ARCHITECTURE.md` §3.4): `DaemonStatus { pid, ingress: IngressInfo { addrs, port }, forward_port, worlds: Vec<WorldSummary> }`, `WorldSummary { world: WorldId, parent: Option<WorldId>, state: WorldState, origin: WorldOrigin, worktree: PathBuf, lineage: WorldLineage }`, `WorldProcessReport { lineage: WorldLineage, services: Vec<WorldServiceSummary> }`, and `WorldServiceSummary { world: WorldId, id: String, kind: ServiceKind, state: ServiceState, pid: Option<u32>, port: Option<u16>, uptime_secs: Option<u64> }`. `IngressInfo.port` is validated present in `IngressInfo.addrs` (failing closed otherwise).
  - `WorldOrigin` is a single untagged newtype `struct WorldOrigin(LocalOrigin)` (`Serialize` emits `origin.url()`, fail-closed `Deserialize` validates `http://<host>[:<port>]`); `slipfork open` passes the typed `&WorldOrigin` directly to `BrowserOpener::open_origin(&WorldOrigin)`.
  - At the CLI `--json` stdout boundary, **one** shared CLI projection helper emits `{"ok": true, ...}` envelopes (and `{"ok": false, "error": "..."}` on error), projects `WorldOrigin` as its URL string `"origin": origin.url()`, and preserves full 64-character lowercase hex digests in JSON (`<digest12>` truncation is human-only). For `status --json`, the CLI fetches `DaemonControl::status()` and attaches each world's `services` via `DaemonControl::world_processes`:
  ```json
  {
    "ok": true,
    "pid": 1420,
    "ingress": { "addrs": ["127.0.0.1:80", "[::1]:80"], "port": 80 },
    "worlds": [
      {
        "world": "demo",
        "parent": null,
        "state": "running",
        "origin": "http://demo.localhost",
        "worktree": "/home/slipfork/demo",
        "lineage": {
          "rootfs": { "digest": "a1b2c3d4e5f60123456789abcdef0123456789abcdef0123456789abcdef0123", "cache": "hit", "millis": null },
          "base": { "key": "4570132bffb8d480614da23adf1ad1bbe56ab5bf5512b93386dacfcb71497107", "millis": 10 },
          "seed": { "digest": "9f8e7d6c5b4a0123456789abcdef0123456789abcdef0123456789abcdef0123", "rebuilt": false, "millis": null },
          "services": [{ "id": "postgres:18", "cache": "hit", "millis": null }]
        },
        "services": [
          { "world": "demo", "id": "pg_clusters.psql", "kind": "backing_service", "state": "running", "pid": 1488, "port": 5432, "uptime_secs": 42 },
          { "world": "demo", "id": "api", "kind": "component", "state": "running", "pid": 1512, "port": 8000, "uptime_secs": 41 }
        ]
      }
    ]
  }
  ```

---

## 4. One Boot Pipeline, Two Verbs: `slipfork up` vs `slipfork restart`

`up --watch` and `restart --watch` **do not exist in `0.1.0`** (zero file watchers, zero polling loops, zero new workspace dependencies). **One pipeline, two verbs:** `up` runs the world boot pipeline when the selected world is unbooted, `stopped`, or `failed` and is idempotent when already `running` (`changed: false`); `restart` stops the world's supervised components and `crun` backing services in place while keeping the worktree and CoW state subvolume, then runs that **exact same** boot pipeline (`changed: true`):

- **`slipfork up` on an already-running world (`changed: false`):**
  ```console
  $ slipfork up
  world     demo (running, unchanged — run `slipfork restart` to apply edits)
  app       http://demo.localhost
  worktree  /home/slipfork/demo
  db        slipfork db
  logs      slipfork logs
  next      slipfork restart  |  slipfork open
  lineage   rootfs a1b2c3d4e5f6 cached · postgres:18 hit · seed 9f8e7d6c5b4a cached · base 4570132bffb8 -
  ```
  *(Why `base 4570132bffb8 -` ends in `-`: when `<state_dir>/snapshots/<project>-base-<key12>` already existed on Btrfs, `ensure_base` did not re-seal the snapshot in this run, so `base.millis` is `None` and renders `-` instead of `0ms`, whereas a cold seal renders `base <key12> sealed <dur>`.)*
- **`slipfork restart [--world <WORLD>] [--json]` (`changed: true`):**
  ```console
  $ slipfork restart
  [restarted] pg_clusters.psql, api
  world     demo (running, changed)
  app       http://demo.localhost
  worktree  /home/slipfork/demo
  db        slipfork db
  logs      slipfork logs
  next      slipfork open  |  slipfork logs
  lineage   rootfs a1b2c3d4e5f6 cached · postgres:18 hit · seed 9f8e7d6c5b4a cached · base 4570132bffb8 -
  ```
- **Rescue & fail-closed edges:** `restart` on a `failed` world re-runs the shared boot pipeline in place (rescue path). `restart` on a never-booted or `stopped` world fails closed in one line: `slipfork: world 'demo' is not running; boot it first with slipfork up`. `up`, `restart`, and `fork` share one summary renderer (`world`, `worktree`, `app`, `db`, `logs`, `cd` on forks, `next`, `lineage`).

---

## 5. World Process Tree & Lineage Header: `slipfork ps`

`slipfork ps [--world <WORLD>] [--json]` calls `DaemonControl::world_processes(&WorldSelector) -> WorldProcessReport { lineage, services }`, backed by `WorldEngine::world_lineage` and `WorldEngine::world_processes` (`uptime_secs` stamped by the engine from its injected monotonic clock). The CLI never reads `/proc`, `/sys`, or a log directory; `None` values render as `-`, never `0`.

```console
$ slipfork ps
lineage  rootfs a1b2c3d4e5f6 · base 4570132bffb8 · seed 9f8e7d6c5b4a
KIND              ID                STATE     PID    PORT   UPTIME
backing_service   pg_clusters.psql  running   1488   5432   42s
component         api               running   1512   8000   41s
```

- **`slipfork ps --json` (stdout only):** Serializes `WorldProcessReport` directly (`{"lineage": {"rootfs", "base", "seed", "services"}, "services": [{"world", "id", "kind", "state", "pid", "port", "uptime_secs"}]}`), preserving full 64-hex digests and `null` for `None`.
- Unknown or unbooted world fails closed: `slipfork: world 'demo' is not running in this slipforkd; run slipfork up first`.

---

## 6. Opening a World Locally or From a Laptop: `slipfork open` & `slipfork open --remote`

- **Local workstation (`slipfork open [--world <WORLD>] [--json]`):** Resolves the world's typed `WorldOrigin` via `DaemonControl` and hands it to `BrowserOpener::open_origin(&WorldOrigin)`. `trait BrowserOpener` (`crates/slipfork-ospp/src/pairing.rs`, keeping `crates/slipfork-ospp` 100% env-free and preserving zeroizing `LivePairingUrl` on `open_pairing_url`) has **one** production implementation `SystemBrowserOpener` in `crates/slipfork/src/browser.rs`, constructed from resolved environment inputs in **both** `crates/slipfork/src/main.rs` and `crates/slipfork/src/bin/slipforkd.rs`. On a headless host (neither `BROWSER` nor `DISPLAY`/`WAYLAND_DISPLAY`, or opener failure), `open` fails closed ending with:
  ```console
  $ slipfork open
  slipfork: no desktop browser available on this host; run `slipfork open --remote` for the SSH port-forward recipe
  ```
- **Remote VM from a laptop (`slipfork open --remote [--world <WORLD>] [--json]`):** Formats `ssh -N -L 8080:127.0.0.1:<ingress.port> <account>@<host>` from `DaemonStatus.ingress.port` (never a hardcoded literal `80` when running on an ephemeral port; local forward port is the fixed constant `8080` with no extra flag) and the Composition Root's injected `UserIdentity` + `HostIdentity` probes (`crates/slipfork/src/user.rs`; never ambient `$USER`/`$HOSTNAME`):
  ```console
  $ slipfork open --remote                # when ingress.port == 80
  world   demo
  ssh     ssh -N -L 8080:127.0.0.1:80 slipfork@slipfork-dev-vm
  open    http://demo.localhost:8080

  $ slipfork open --remote                # when ingress.port == 40645 (also prints §2's stderr :80 line)
  world   demo
  ssh     ssh -N -L 8080:127.0.0.1:40645 slipfork@slipfork-dev-vm
  open    http://demo.localhost:8080
  ```
- **Why `http://<route>.localhost:8080` routes without proxy config (verified):** Modern browsers resolve `*.localhost` to `127.0.0.1`/`::1` (RFC 6761) and send `Host: demo.localhost:8080` over the SSH tunnel to `slipforkd`'s `127.0.0.1:<ingress.port>` listener. `WorldRegistry::resolve` delegates to `LocalHostRoute::parse`, whose `normalize_host` strips any numeric `:port` (`crates/slipfork-world/src/lib.rs:582-594`), routing `Host: demo.localhost:8080` and `Host: feat-ux.demo.localhost:8080` to the exact same worlds.

---

## 7. Zero-Arg `slipfork fork`, Pinned Snapshot Identity & One-Line VCS Errors

`slipfork fork [FORK]` makes `<FORK>` optional (`slipfork drop <FORK>` keeps its required name so destructive teardown is always explicit):

```console
$ git switch -c feat/ux-pass
$ slipfork fork
world     demo:feat-ux-pass (running, changed)
worktree  /home/slipfork/demo/.worktrees/feat-ux-pass
app       http://feat-ux-pass.demo.localhost
cd        cd /home/slipfork/demo/.worktrees/feat-ux-pass
next      slipfork open --world feat-ux-pass  |  slipfork drop feat-ux-pass
lineage   rootfs a1b2c3d4e5f6 cached · postgres:18 hit · seed 9f8e7d6c5b4a cached · base 4570132bffb8 -
```

1. **Daemon-side branch derivation & real `snow_domains` validation (`VcsWorkspaceFactory::current_branch`):** The CLI sends `ForkRequest { fork: Option<String>, .. }` and never runs `git` or reads `.git`. When `fork` is `None`, `slipforkd` queries `VcsWorkspaceFactory::current_branch(&worktree_root)` and sanitises the branch name via `crates/slipfork-vcs::sanitize_fork_name` (lowercase ASCII, `/` -> `-`, drop bytes outside `[a-z0-9-]`, collapse repeated `-`, strip leading/trailing `-`, reject empty). `slipforkd` then parses the checkout's `snow.yml`, checks `sanitised != repo.project.as_str()` as a separate check, and validates `validate_fork_name(&sanitised, &snow_domains)` passing the project's real `snow.yml` `domains[].id` slice (never `&[]`).
2. **Fail-closed no-arg cases (never a generated random name):**
   - Detached `HEAD` or bookmark-less `jj`: `slipfork: checkout has no active branch; pass an explicit name with slipfork fork <name>`
   - Invoked inside `.worktrees/<fork>`: `slipfork: already inside fork worktree '.worktrees/<fork>'; run slipfork fork from the repository root /home/slipfork/demo`
   - Fork already exists: `slipfork: fork 'feat-ux-pass' already exists; boot it with slipfork up --world feat-ux-pass`
   - Sanitised branch equals `<project>` or collides with `domains[].id` / `test-*` / empty: `slipfork: branch 'demo' collides with project 'demo'; pass a distinct name with slipfork fork <name>`
3. **Pinned internal snapshot identity (`user.name=slipfork`):** Synthesizing `refs/slipfork/<fork>/wip` passes `-c user.name=slipfork -c user.email=slipfork@localhost` as global `git` flags before `commit-tree` (and equivalent `--config` overrides in `jj`), never reading, writing, or requiring the developer's `git config`.
4. **One-line VCS errors:** Non-zero `git`/`jj` exits format as `slipfork: <subcommand> failed (exit <n>): <last stderr line>` — zero raw multi-line `git` advice dumps.

---

## 8. `dag` Deleted From `0.1.0`, Per-World `WorldLineage` (`lineage`), `slipfork-dag` Sweep, Quiet `shell` & Attributed `logs`

### 8.1 Total `dag` Deletion & `0.2.0` Parking Rule (Owner Ruling #1)

- **`dag` is deleted from `0.1.0` entirely (`M3.28.11`):** Remove `Commands::Dag` and `DagAction` from `crates/slipfork/src/cli.rs`, its `main.rs` renderer, its `critical_cli` tests, `docs/testing-dag.md` + its `zensical.toml` nav entry (`"Orthogonal DAG planner"`), and all `dag` references in `docs/reference/cli.md`, `docs/index.md`, `docs/getting-started.md`, and `README.md`. **No rename to `slipfork plan`, no alias, no `#[deprecated]`:** `slipfork dag` exits `2` as an unrecognized subcommand.
- **Docs truth obligations (`M3.28.11` / `M3.28.14`):** `AGENTS.md` line 3's claim that `0.1.0` ships `"the dag plan planner"` is false under this ruling and must be fixed in `M3.28.11` / verified in `M3.28.14`. In `M3.28.14`, `CHANGELOG.md`'s `### Removed` block under `0.1.0` must name both `dag plan` and the unimplemented `share` subcommand (`§9`).
- **`0.2.0` parking note (never a `0.1.0` promise):** Multi-world DAG planning belongs to `0.2.0` (`slipfork test --dry-run` / plan summary such as `"31 cached · 9 rebuild across 8 worlds"`), which is the first moment planning is an execution decision.

### 8.2 Per-World `WorldLineage` Wire Contract (`M3.28.3`–`M3.28.4`) & `up -v` Detail

Rootfs and OCI service-image cache outcomes + elapsed `millis: Option<u64>` are returned on `WorldRootfs` and `OciRootfs` (which already observe `RootfsBuildEvent::{CacheHit, CacheMiss}` via the injected monotonic clock) and recorded on each world's handle alongside `ensure_base` / `clone_world` measurements (`WorldEngine::world_lineage(&WorldHandle) -> Result<WorldLineage, EngineError>`). `WorldLineage` travels on `WorldUpSummary.lineage`, `WorldSummary.lineage`, and `WorldProcessReport.lineage` (never on `DaemonStatus`):

- `rootfs: Option<RootfsLineage>` — `{ digest: String /* full 64-hex */, cache: LineageCache /* hit|miss */, millis: Option<u64> }` (`Hit` -> `cached`, `Miss` -> `rebuilt <dur>`)
- `services: Vec<ServiceImageLineage>` — `{ id: String, cache: ServiceCache /* hit|miss|miss_offline */, millis: Option<u64> }`:
  - `ServiceCache::Hit` renders `<id> hit`
  - `ServiceCache::Miss` renders `<id> pulled <dur>` (`-` when `millis` is `None`)
  - `ServiceCache::MissOffline` renders `<id> miss-offline` (literal token)
- `seed: Option<SeedLineage>` — `{ digest: Option<String /* full 64-hex */>, rebuilt: Option<bool>, millis: Option<u64> }` (`rebuilt == Some(true)` -> `rebuilt <dur>`, `Some(false)` -> `cached`)
- `base: Option<BaseLineage>` — `{ key: String /* full 64-hex */, millis: Option<u64> }` (`Some(ms)` -> `sealed <dur>`, `None` on warm reuse -> `-`)

Every unknown or unmeasured field is `None` (`null` in `--json`) and renders `-` in human output — **never `0`, never `""`, and never an estimated duration**:

- **Default `slipfork up` / `fork` / `restart` (`M3.28.8`):** Prints **one** compact line:
  `lineage  rootfs <digest12> cached · postgres:18 hit · seed <digest12> rebuilt 2.1s · base <key12> sealed 10ms`
- **Verbose `slipfork -v up` (`M3.28.8` / `M3.28.11`):** Adds per-node lineage and `[resources]` detail:
  ```console
  $ slipfork -v up
  [resources] demo: prefer_remote -> local (not_offered)
  [lineage]   rootfs       a1b2c3d4e5f6  hit           -
  [lineage]   postgres:18  -             hit           -
  [lineage]   seed         9f8e7d6c5b4a  rebuilt       2104ms
  [lineage]   base         4570132bffb8  sealed        10ms
  ```

### 8.3 `crates/slipfork-dag` Consumer & Deletion Audit (`M3.28.11`)

| `slipfork-dag` surface | `0.1.0` disposition | Verified consumer / reason |
|---|---|---|
| `SlipforkConfig`, `WorldImageConfig`, `WorldConfigError`, `ArtifactSpec`, `FixtureSpec`, `TestSpec`, `CoverageConfig`, `CoverageReportFormat`, `CoverageThreshold`, `DagError` (minus `Io`) | **Keep** | Live `0.1.0` config consumers: `crates/slipfork/src/project.rs`, `crates/slipfork/src/daemon.rs`, `critical_world_config.rs`, `critical_dag.rs` |
| `ResourcesSpec`, `ResourcePlan::resolve`, `ResourceMode`, `ResourceStrategy`, `DumpPolicy`, `RemoteResourceOffer`, `RemoteS3Bucket`, `RemotePgDump`, `Planned*`, `S3Resolution`, `PgSeed`, `Resource*` | **Keep** | Live `0.1.0` resource consumers: `crates/slipfork/src/daemon.rs`, `crates/slipfork-ospp/src/{resources.rs,client.rs}`, `critical_ospp.rs`, `critical_dag.rs` |
| `src/fs.rs` (`FsMerkleHasher`, `FsDigestCache`), `DagError::Io`, `slipfork-dag` `fs` Cargo feature, `slipfork` `features = ["fs"]` | **Delete in `M3.28.11`** | Unreachable once `Commands::Dag` is deleted |
| Pure `DagPlanner`, `DagPlan`, `PlannedArtifact`, `PlannedFixture`, `CacheAction`, `InputHasher`, `DigestCache`, `MapInputHasher`, `SetDigestCache`, `matches_glob`, and `SupportRegistry` / `IntegrationTier` / `IntegrationCategory` (minus `Tunnel`) / `Evidence` / `EvidenceField` / `RegistryError` | **Keep (not wired into `0.1.0` Composition Root)** | Retained because protected critical tests (`critical_dag.rs`, `critical_support.rs`) and `tools/check_support.py` pin them and `0.2.0`'s `slipfork test` consumes the planner |

### 8.4 Quiet `shell` & Attributed `logs` (`M3.28.11`)

- **Quiet world entry (`slipfork shell`, `exec`, `db`, `hook run`):** `BtrfsEngine::entry_command` passes `--wd` inside `nsenter` with host `WorldEntryCommand.cwd = "/"` so the entry shell starts on a valid in-world dentry and `sh: 0: getcwd() failed` never appears. Interactive `slipfork shell` prints one orientation line to stderr before handing over the TTY: `world rootfs is the app stage (no host tools; use slipfork ps / logs / open)`.
- **Attributed `slipfork logs`:** When streaming more than one source (`slipfork logs` with no target), every line is prefixed with `<source>: ` (`api: `, `api:pre-deploy: `); a single named target (`slipfork logs api`) prints unprefixed raw lines; `--json` stays unchanged.

---

## 9. Locked Decision 9: Single `Local` Origin (`struct WorldOrigin`) & Deletion of Outbound Sharing (`M3.28.15`)

`0.1.0` has **one** origin kind: untagged `struct WorldOrigin(LocalOrigin)`, minted solely via `WorldOrigin::local(&route, ingress_port)`. Because `UnavailableTunnelRegistrar::register()` unconditionally returns `Err(TunnelError::DaemonUnavailable)` and a CLI command that can never succeed must not exist, `M3.28.15` deletes `Commands::Share` + `--stop`, `DaemonControl::share`/`unshare`, `UnavailableTunnelRegistrar` + `crates/slipfork-edge/src/tunnel.rs`, `TunnelRegistrar`/`TunnelLease`/`TunnelError`/`TunnelRequest` + `InMemoryTunnelRegistrar`/`assert_tunnel_registrar_conformance`, `IntegrationCategory::Tunnel`, the `Shared` origin variant, and the edge's `308`/`421` redirect branches and public-host map (`tools/check_stub_adapters.py` in `M3.28.16` fails `make check` if any composition root wires `Unavailable*`/`Stub*`/`Fake*`/`todo!()`). `slipforkd` origin rendering and `WorldRegistry::resolve` have zero `Shared` branch and zero `_ =>` catch-all arm.

---

## 10. Single Normative `0.1.0` CLI Command Table & `--json` Schemas

Global flags: `--json` (stdout JSON only; errors emit `{"ok": false, "error": "..."}`), `-v` / `--verbose`, `--state-dir <DIR>` (`$XDG_CACHE_HOME/slipfork` or `<home>/.cache/slipfork`), `--runtime-dir <DIR>` (`<state_dir>/run`). Every `"origin"` string in `--json` is the single CLI projection `origin.url()` of typed `WorldOrigin`, and every digest/key in `--json` is the full 64-hex string.

| Command | Summary | `--json` stdout schema |
|---|---|---|
| `slipfork` / `slipfork status` | Daemon ingress + per-project canonical/`:fork` world tree + lineage digests + next steps | `{"ok": true, "pid": u32, "ingress": {"addrs": [str], "port": u16}, "worlds": [{"world": str, "parent": str\|null, "state": "running\|failed\|stopped", "origin": str, "worktree": str, "lineage": WorldLineage, "services": [WorldServiceSummary]}]}` |
| `slipfork up [--world <W>] [--no-dump \| --refresh-dump] [--resources local] [--host-caches]` | Boot unbooted/`stopped`/`failed` world or confirm `running` (`changed: false`) + `lineage` | `{"world": str, "state": "running", "changed": bool, "restarted": [str], "worktree": str, "origin": str, "lineage": WorldLineage, "resources": [{"id": str, "strategy": str, "resolved": str, "reason": str}]}` |
| `slipfork restart [--world <W>]` | Stop components + `crun` services in place, then run the shared boot pipeline (`changed: true`) | Same schema as `slipfork up --json` (`"changed": true`) |
| `slipfork ps [--world <W>]` | World `lineage` header + supervised component & `crun` backing-service table | `{"lineage": WorldLineage, "services": [{"world": str, "id": str, "kind": "component\|backing_service", "state": "running\|exited\|stopped", "pid": u32\|null, "port": u16\|null, "uptime_secs": u64\|null}]}` |
| `slipfork open [--world <W>] [--remote]` | Open typed `WorldOrigin` via `BrowserOpener::open_origin` or print SSH `:8080 -> 127.0.0.1:<ingress.port>` recipe | `{"ok": true, "world": str, "origin": str, "recipe"?: {"ssh": str, "url": str}}` |
| `slipfork fork [FORK] [--base <REV>] [--no-dump \| --refresh-dump] [--resources local] [--host-caches]` | Branch-derived (or explicit) `.worktrees/<fork>` + Btrfs CoW world + `cd` + `lineage` | Same schema as `slipfork up --json` (`"changed": true`) |
| `slipfork drop <FORK>` | Destroy fork subvolume, `.worktrees/<fork>`, branch, and `refs/slipfork/<fork>/wip` | `{"ok": true, "dropped": bool}` |
| `slipfork down [--world <W>]` | Stop world services while keeping its worktree (idempotent) | `{"ok": true, "released": bool}` |
| `slipfork list` | Flat `WORLD / STATE / ORIGIN / WORKTREE` table across projects | `[{"world": str, "state": "running\|failed\|stopped", "origin": str, "worktree": str}]` |
| `slipfork prune [--dry-run \| --yes]` | Preview (default/`--dry-run`) or reap worlds whose checkout directory is gone | `{"dry_run": bool, "worlds": [str], "snapshots": [], "artifacts": [], "rootfs_digests": [], "oci_digests": [], "reclaim_bytes": 0}` |
| `slipfork daemon install [--user <U>] [--bin-dir <D>]... [--forward-port <P>]` | Install `/etc/systemd/system/slipforkd.{socket,service}` + `/etc/apparmor.d/slipfork` and `enable --now` | `{"ok": true, "user": str, "socket": "wrote\|unchanged", "service": "wrote\|unchanged", "apparmor": "wrote\|unchanged"}` |
| `slipfork daemon unit [--only socket\|service\|apparmor] [--bin-dir <D>]... [--forward-port <P>]` | Render systemd socket/service or flat 4-line AppArmor profile to stdout | `{"socket": str\|null, "service": str\|null, "apparmor": str\|null}` |
| `slipfork doctor` | Pre-daemon Btrfs + userns/netns + advisory `:80` `ingress` check | `{"ok": bool, "uid": u32, "gid": u32, "state_dir": str, "probed_path": str, "fstype": str, "filesystem": str, "btrfs_super_magic": bool, "subvolume_ioctl": {...}, "noatime": bool, "unshare_userns": {...}, "netns": {...}, "ingress": {"status": "active\|inactive\|not_installed", "port"?: 80, "listen"?: [str], "hint"?: str}, "fixes": [str]}` |
| `slipfork exec [--world <W>] [--cwd <D>] -- <ARGV>...` | Quiet non-interactive execution inside the world's `slipfork-world-init` PID 1 | Child stdout passthrough, followed by `{"ok": bool, "world": str, "exit_code": i32\|null}` |
| `slipfork shell [--world <W>]` | Quiet interactive TTY shell inside the world (requires TTY; prints 1 stderr stage hint) | Interactive TTY; after exit `{"ok": bool, "world": str, "exit_code": i32\|null}` |
| `slipfork logs [--world <W>] [TARGET] [--follow] [--stderr] [--lines <N>]` | Retained component/hook logs (`<source>: ` prefix when multi-source) | JSON lines `{"stream": "stdout\|stderr", "source": str, "text": str}` |
| `slipfork hook run <HOOK> [--world <W>] [--no-tty]` | Re-run `<component>:<phase>` hook (interactive TTY `pdb` or `--no-tty` via daemon) | `{"ok": bool, "world"?: str, "exit_code": i32\|null}` |
| `slipfork db [DB_ID] [--world <W>]` | Interactive `psql` inside the running `postgres:<v>` `crun` container (TTY) | Interactive TTY; after exit `{"ok": bool, "world": str, "service": str, "exit_code": i32\|null}` |
| `slipfork redis [CLUSTER_ID] [--world <W>]` | Interactive `redis-cli` inside the running `redis:<v>` `crun` container (TTY) | Interactive TTY; after exit `{"ok": bool, "world": str, "service": str, "exit_code": i32\|null}` |
| `slipfork forward [--world <W>] [TARGET...]` | Bind daemon-owned `127.0.0.1:0` bridges into the world until Ctrl-C / disconnect | `{"world": str, "targets": [{"id": str, "host_addr": str, "uri": str}]}` |
| `slipfork snow resolve [--file <F>] [--mode <M>] [--component <C>] [--expose-secrets]` | Validate `snow.yml`, persist missing `!!` secrets, print resolved variables (`[REDACTED]`) | `{<key>: <redacted_or_exposed_str>}` |
| `slipfork secret set [--force] <KEY>` / `unset <KEY>` / `list` | Manage `0600` project vault secrets through `slipforkd` (`set` reads stdin) | `{"ok": true, "key": str}` / `{"ok": true, "key": str, "removed": bool}` / `[str]` |
| `slipfork auth login [- \| '<url>#handle=<64-hex>']` | Pair `tpmd` / P-256 device key over `ospp-v1` (`-` reads stdin) | `{"ok": true, "key_id": str, "backend": str, "account": str\|null}` |
| `slipfork support` | Report embedded integration tiers and `required_evidence` (`support/integrations.toml`) | `[{"id": str, "category": str, "tier": str, "evidence": {...}, "required_evidence": [str]}]` |
