# Slipfork `0.1.0` UX Specification: Port-Less `:80` Ingress, Bare `status`, `restart`, `ps`, `open`, Branch-Named `fork`, and `lineage`

- **Status:** Accepted `0.1.0` Target DX Specification (`M3.28.1`, `ADR-0011`; **single normative home** for the `0.1.0` CLI command table and `--json` documents)
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
next      slipfork open | slipfork db | slipfork logs | slipfork restart | slipfork down
lineage   rootfs a1b2c3d4e5f6 cached · postgres:18 cached · seed 9f8e7d6c5b4a rebuilt 2.1s · base 4570132bffb8 sealed 10ms

$ curl -fsS http://demo.localhost/
{"hello": "world", "world": "demo"}

$ slipfork
slipforkd 1420, ingress 127.0.0.1:80, [::1]:80
  demo            running   http://demo.localhost              /home/slipfork/demo
                  lineage   rootfs a1b2c3d4e5f6 cached · postgres:18 cached · seed 9f8e7d6c5b4a cached · base 4570132bffb8 reused
                  next      slipfork open | slipfork db | slipfork logs | slipfork restart | slipfork down
```

- **Block order:** `world`, `app`, `worktree`, `restarted` (restart) / `cd` (fork), `next`, `lineage`; `status` renders the daemon line plus one `world` / `lineage` / `next` triple per world, and `ps` renders `world`, `lineage`, the process table, then `next`. No command invents a sixth row.
- **One `lineage` line, defined once:** the renderer, its key order, and its outcome vocabulary live in `§8.2` only; no other section restates them.
- **Zero happy-path noise:** `[resources]` resolution rows (`prefer_remote -> local (not_offered)`), `[lineage]` per-node rows, and namespace vocabulary never print on a normal boot. Global `-v` / `--verbose` adds those rows to **human** output only; `--json` is always the maximal machine document (`§10`).
- **`next` is one table, stated here and nowhere else:**

  | World state | `next` membership, in this order |
  |---|---|
  | `running` | `open`, `db`, `logs`, `restart`, `down` |
  | `failed` | `logs`, `restart`, `down` |
  | `stopped` | `up`, `logs`, `down` |

  A world that is not the `cwd`-selected one renders each command with `--world <project>:<fork>`. `status`'s `list`-style `NEXT` column (`§10`) shows that table's first command for the row's state.
- **Only `up` and `fork` auto-spawn `slipforkd`.** Every other verb — `status`, `ps`, `list`, `logs`, `open`, `restart`, `down`, `drop`, `db`, `redis`, `exec`, `shell`, `secret`, `snow resolve`, `hook` — fails closed when nothing answers on `<state_dir>/run/daemon.sock`, with exactly one line and exit `1`:
  ```console
  $ slipfork
  slipfork: no slipforkd on /home/slipfork/.cache/slipfork/run/daemon.sock — run `slipfork up`
  ```
  This is the fix for the owner's first finding: a `:0` daemon no longer appears because something *read* the state.

---

## 2. Port-80 System Socket Activation: `slipfork daemon install` & `slipfork doctor`

On Linux (`net.ipv4.ip_unprivileged_port_start = 1024`), neither an unprivileged process nor `systemd --user` can bind port `80`. `slipfork daemon install` (`M3.28.6`) is the **single** command that installs `/etc/systemd/system/slipforkd.socket` (`ListenStream=127.0.0.1:80`, `[::1]:80`, `127.0.0.1:<forward_port>`, default `7080`), `/etc/systemd/system/slipforkd.service` (`User=<account>`, empty `CapabilityBoundingSet=`, `Requires=slipforkd.socket`, `After=slipforkd.socket`, `WantedBy=multi-user.target`), and `/etc/apparmor.d/slipfork` (`userns,`), then activates both units immediately and across reboots:

```console
$ sudo slipfork daemon install
wrote     /etc/systemd/system/slipforkd.socket
wrote     /etc/systemd/system/slipforkd.service
wrote     /etc/apparmor.d/slipfork
enabled   slipforkd.socket + slipforkd.service as slipfork, ingress 127.0.0.1:80 + [::1]:80
next      slipfork up

$ sudo slipfork daemon install          # idempotent re-run
unchanged slipforkd.socket, slipforkd.service, /etc/apparmor.d/slipfork; enabled
```

Three lines of `wrote` are the whole first-run story, the `enabled` line names the account the daemon runs as (so nobody has to run `getent passwd`), and a re-run is one line.

### Locked `daemon install` & `daemon unit` Rules

1. **Identity, root policy & `Layout::for_account` (`crates/slipfork/src/user.rs`):** `slipfork daemon install [--user <ACCOUNT>] [--bin-dir <DIR>]... [--forward-port <PORT>]` (no `--force`).
   - Install orchestration is a pure function taking `is_root: bool` and `explicit_user: Option<&str>`.
   - When `!is_root`, `UserIdentity::username()` (`id -un`, `env_clear`) resolves `<account>` and `daemon install` fails closed before touching `UnitInstaller`, ending on the exact command that works next:
     ```console
     $ slipfork daemon install
     slipfork: daemon install writes /etc and enables system units; run `sudo slipfork daemon install`
     ```
   - When `is_root`, `explicit_user` falls back to `UserIdentity::invoking_account()` (`crates/slipfork/src/user.rs` is the **only** module permitted to read `SUDO_USER`, under its fixed `PATH`, validating the name through `UserIdentity::lookup_account`), so the bare `sudo slipfork daemon install` in `§1` is correct on a developer host. Direct root with neither `--user` nor a valid non-root `invoking_account()`, or `--user root` (`uid == 0`), fails closed in one line:
     ```console
     $ sudo slipfork daemon install
     slipfork: slipforkd must run as an unprivileged developer account, never root; log in as that account and run `sudo slipfork daemon install`
     ```
   - `--user <ACCOUNT>` is the override for a host whose daemon account is not the operator's login account (it is not needed on a normal install). `UserIdentity::lookup_account` resolves the target's `ResolvedAccount { name, uid, gid, home }` (rejecting unknown accounts, `%`-specifiers, and `uid == 0`), and `Layout::for_account` computes the unit's `state_dir` and `runtime_dir` (`<state_dir>/run`).
2. **Exact product renderers & flat repeatable `--bin-dir`:** `daemon install` writes the **exact bytes** produced by `slipfork daemon unit`; `daemon unit` renders, `daemon install` installs, and nothing else writes a unit file. `daemon unit` gains repeatable `--bin-dir <DIR>` (default: `current_exe()` parent directory), emitting one flat `<rel_dir>/slipfork{,d,-world-init}` alternative per `--bin-dir` inside the single 4-line AppArmor brace expression (never nested braces):
   ```text
   abi <abi/4.0>,
   profile slipfork /{usr/local/bin/slipfork{,d,-world-init},tmp/ux/bin/slipfork{,d,-world-init},usr/bin/unshare,.slipfork/bin/crun} flags=(unconfined) {
     userns,
   }
   ```
3. **Narrow `UnitInstaller` port & clean `:0` handover (`crates/slipfork/src/daemon_unit_install.rs`):** All OS writes and unit transitions go through narrow `UnitInstaller` verbs (`read_file`, `write_file_atomic`, `daemon_reload`, `apparmor_replace`, `enable_now`, `restart_service` — zero stringly `run(program, args)` bag). Each file is written via a same-directory temp file + `rename` **only** when its bytes differ from disk. Before `enable_now(&[ManagedUnit::Socket, ManagedUnit::Service])`, if `<state_dir>/run/daemon.sock` is currently answered by a non-`:80` auto-spawned `slipforkd`, `daemon install` sends `DaemonControl::shutdown` and awaits socket release so `slipforkd.service` never hits `EdgeError::SocketInUse` (`restart_service` runs when unit bytes changed while already active).
4. **Loud one-line notice when ingress is not `:80` (single normative owner):** Rootless auto-spawn (`slipforkd --listen 127.0.0.1:0`) remains available for unprivileged CI and containers, where `WorldOrigin::local` formats `http://<host>:<port>`. Whenever a daemon answers and `DaemonStatus.ingress.port != 80`, the six world/daemon inspection and entry commands — **`status`, `up`, `fork`, `restart`, `list`, and `open` (including `open --remote`)** — emit **exactly one** line to **stderr** (never stdout, never under `--json`), naming the command that fixes it and printing no placeholder:
   ```text
   slipfork: ingress is :40645, not :80 — run `sudo slipfork daemon install` once for port-less origins
   ```
5. **Pre-daemon `slipfork doctor` ingress line (`IngressProbe`):** `doctor` reports `:80` socket activation status alongside the four required Btrfs/namespace checks (advisory — `DoctorReport.ok` stays `true` on unprivileged hosts without systemd units; the CLI's document key is `healthy`, `§10`):
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
   The other two states are ``ingress: not installed (ephemeral port; run `sudo slipfork daemon install`)`` and ``ingress: installed but inactive (run `sudo slipfork daemon install`)`` — the installer is always the hint, and the userns failure hint ends on it too. No other command is offered as a competing remedy.
6. **One canonical runtime directory (`<state_dir>/run`), owned by `slipforkd`:** the client's socket path is unconditionally `<state_dir>/run/daemon.sock`, and `--runtime-dir <DIR>` is a **`slipforkd`-only** flag (also consumed by `daemon unit`/`daemon install` when rendering the unit). A client-side override could point at a different `daemon.sock` than the installed unit and silently win, so the thin client has no such flag; `--state-dir` stays global because both sides derive from it. `Layout::for_account(&ResolvedAccount, ...)` derives `state_dir` (`--state-dir` -> `$XDG_CACHE_HOME/slipfork` -> `<home>/.cache/slipfork`) and `runtime_dir = <state_dir>/run`; `Layout::resolve` resolves the invoking account and calls it, so there is never a second layout derivation or a second socket path. `M3.28.6` (daemon/unit side) and `M3.28.7` (client side) implement this; `M3.28.14` publishes it.

---

## 3. Bare `slipfork`, `slipfork status`, and the Daemon-vs-CLI Boundary

`Cli.command` becomes `Option<Commands>` in `clap`: invoking bare `slipfork` runs the exact same handler as `slipfork status`. `slipfork --help` and `slipfork help` list exactly `§10`'s verbs and nothing else — no `dag`, no outbound-sharing verb.

```console
$ slipfork status
slipforkd 1420, ingress 127.0.0.1:80, [::1]:80
  demo            running   http://demo.localhost                /home/slipfork/demo
                  lineage   rootfs a1b2c3d4e5f6 cached · postgres:18 cached · seed 9f8e7d6c5b4a cached · base 4570132bffb8 reused
                  next      slipfork open | slipfork db | slipfork logs | slipfork restart | slipfork down
    demo:feat-ux  running   http://feat-ux.demo.localhost        /home/slipfork/demo/.worktrees/feat-ux
                  lineage   rootfs a1b2c3d4e5f6 cached · postgres:18 cached · seed 9f8e7d6c5b4a cached · base 4570132bffb8 reused
                  next      slipfork open --world demo:feat-ux | slipfork db --world demo:feat-ux | slipfork logs --world demo:feat-ux | slipfork restart --world demo:feat-ux | slipfork down --world demo:feat-ux
```

When the daemon answers but zero worlds exist:

```console
$ slipfork
slipforkd 1420, ingress 127.0.0.1:80, [::1]:80
  no worlds — run `slipfork up` inside a project checkout
```

When no daemon answers, bare `slipfork` prints `§1`'s one `no_daemon` line and exits `1` — a read never starts a daemon.

- **Every printed `world` value is the canonical `WorldId`** (`demo`, `demo:feat-ux`), indented under its project, so it pastes straight back into `--world`.
- **Daemon wire model vs each verb's document (stated once, shared everywhere):**
  - Over `<state_dir>/run/daemon.sock`, `DaemonControl` exchanges typed Rust values (`ARCHITECTURE.md` §3.4): `DaemonStatus { pid, ingress: IngressInfo { addrs, port }, forward_port, worlds: Vec<WorldSummary> }`, `WorldSummary { world: WorldId, parent: Option<WorldId>, state: WorldState, origin: WorldOrigin, worktree: PathBuf, lineage: WorldLineage }`, `WorldUpSummary { world, state, changed, restarted: Vec<String>, worktree, origin, lineage, services: Vec<WorldServiceSummary>, resources, next }`, `WorldProcessReport { state: WorldState, lineage: WorldLineage, services: Vec<WorldServiceSummary> }`, and `WorldServiceSummary { world: WorldId, id: String, kind: ServiceKind, state: ServiceState, pid: Option<u32>, port: Option<u16>, uptime_secs: Option<u64> }`. `IngressInfo.addrs` lists the bound HTTP listeners and **its order is non-semantic**; `IngressInfo.port` is validated present in `addrs` (failing closed otherwise). `WorldServiceSummary.id` is unique within one world's `services[]`, which is what makes `restarted` a sound 1:1 join (`§10`).
  - `WorldOrigin` is a single untagged newtype `struct WorldOrigin(LocalOrigin)` (`Serialize` emits `origin.url()`, fail-closed `Deserialize` validates `http://<host>[:<port>]`); `slipfork open` passes the typed `&WorldOrigin` directly to `BrowserOpener::open_origin(&WorldOrigin)`.
  - There is **no shared envelope or projection abstraction**: each verb owns and renders its own top-level document, defined per row in `§10`. Three contract facts hold across those documents — `"origin"` is always the `origin.url()` string of the typed `WorldOrigin`, the lineage service-image key is `"image"` (wire: `ServiceImageLineage.id`), and `doctor`'s report verdict is `"healthy"` (wire: `DoctorReport.ok`). Failures use `§10`'s failure document.

---

## 4. One Boot Pipeline, Two Verbs: `slipfork up` vs `slipfork restart`

`up --watch` and `restart --watch` **do not exist in `0.1.0`** (zero file watchers, zero polling loops, zero new workspace dependencies). **One pipeline, two verbs:** `up` runs the world boot pipeline when the selected world is unbooted, `stopped`, or `failed` and is idempotent when already `running` (`changed: false`); `restart` stops the world's supervised components and `crun` backing services in place while keeping the worktree and CoW state subvolume, then runs that **exact same** boot pipeline (`changed: true`):

- **`slipfork up` on an already-running world (`changed: false`)** — the answer to "did my edit get picked up?":
  ```console
  $ slipfork up
  world     demo (running, unchanged — run `slipfork restart` to apply edits)
  app       http://demo.localhost
  worktree  /home/slipfork/demo
  next      slipfork open | slipfork db | slipfork logs | slipfork restart | slipfork down
  lineage   rootfs a1b2c3d4e5f6 cached · postgres:18 cached · seed 9f8e7d6c5b4a cached · base 4570132bffb8 reused
  ```
- **`slipfork restart [--world <WORLD>]` (`changed: true`):**
  ```console
  $ slipfork restart
  world     demo (running, changed)
  app       http://demo.localhost
  worktree  /home/slipfork/demo
  restarted pg_clusters.psql, api
  next      slipfork open | slipfork db | slipfork logs | slipfork restart | slipfork down
  lineage   rootfs a1b2c3d4e5f6 cached · postgres:18 cached · seed 9f8e7d6c5b4a cached · base 4570132bffb8 reused
  ```
- **`restarted` is a bare id list** (`[str]`): each entry is a `services[].id` of this world, unbracketed in human output, and `[]` when nothing restarted. Ids are unique within a world's `services[]` (`§3`), so the join is sound.
- **Rescue & fail-closed edges:** `restart` on a `failed` world re-runs the shared boot pipeline in place (rescue path). `restart` on a never-booted or `stopped` world stops nothing, so it refuses rather than silently booting what the caller believed was running, in exactly one line: ``slipfork: world 'demo' is not running; boot it with `slipfork up` `` (`code: not_running`, `§10`). A boot that fails under `up`/`fork`/`restart` leaves the world `failed`, prints one line naming the log command, and under `--json` emits the failure document with `code: boot_failed` and `world` + `state` extras (`§10`).
- **Self-sufficient documents:** `up`, `restart`, and `fork` each return `world`, `state`, `changed`, `restarted`, `worktree`, `origin`, `lineage`, `services[]`, `resources[]`, and `next` (`§10`), so an agent reads ids, ports, and the next command from one call. Their human blocks share one key order (`§1`).

---

## 5. World Process Tree & Lineage Header: `slipfork ps`

`slipfork ps [--world <WORLD>]` calls `DaemonControl::world_processes(&WorldSelector) -> WorldProcessReport { state, lineage, services }` — the world's `state`, its `lineage`, and its process rows all come from that **one** read (`M3.28.3` wires `state` into the report; no second `status` call). The CLI never reads `/proc`, `/sys`, or a log directory; `None` values render as `-`, never `0`. `ps` is the canonical **per-world process view**; `status` is the fleet view.

```console
$ slipfork ps
world     demo (running)
lineage   rootfs a1b2c3d4e5f6 cached · postgres:18 cached · seed 9f8e7d6c5b4a cached · base 4570132bffb8 reused
ID                STATE     PID    PORT   UPTIME
pg_clusters.psql  running   1488   5432   42s
api               running   1512   8000   41s
next      slipfork open | slipfork db | slipfork logs | slipfork restart | slipfork down
```

- **Ports are in-world only.** `PORT 5432` is the container's readiness port inside the world's `netns`; nothing on the host answers it. The `next` line names `slipfork db` (the supported way in) so nobody tries host `psql -p 5432`.
- **States, defined once:** a world is `running | failed | stopped`; a world service is `running | exited | stopped` (`exited` = the process ended on its own; `stopped` = `down`/`restart` stopped it).
- **`ps` is read-only and never fails for a world that exists:** a `stopped` or `failed` world prints its `world` line, lineage, and rows, and exits `0`. Only an unknown or never-booted world fails closed, in one line: ``slipfork: no world 'demo' here; boot it with `slipfork up` or list worlds with `slipfork list` `` (`code: unknown_world`). With no daemon answering at all, `§1`'s `no_daemon` line applies.
- **`slipfork ps --json`:** that verb's document in `§10` — `{"ok": true, "world", "state", "lineage", "services"}` — preserving full 64-hex digests and `null` for every unknown.

---

## 6. Opening a World Locally or From a Laptop: `slipfork open` & `slipfork open --remote`

- **Local workstation (`slipfork open [--world <WORLD>]`):** Resolves the world's typed `WorldOrigin` via `DaemonControl` and hands it to `BrowserOpener::open_origin(&WorldOrigin)`, then confirms in one line:
  ```console
  $ slipfork open
  opened    http://demo.localhost
  ```
  `trait BrowserOpener` (`crates/slipfork-ospp/src/pairing.rs`, keeping `crates/slipfork-ospp` 100% env-free and preserving zeroizing `LivePairingUrl` on `open_pairing_url`) has **one** production implementation `SystemBrowserOpener` in `crates/slipfork/src/browser.rs`, constructed from resolved environment inputs in **both** `crates/slipfork/src/main.rs` and `crates/slipfork/src/bin/slipforkd.rs`. On a headless host (neither `BROWSER` nor `DISPLAY`/`WAYLAND_DISPLAY`, or opener failure), `open` fails closed on one actionable line:
  ```console
  $ slipfork open
  slipfork: no desktop browser available on this host; run `slipfork open --remote` for the SSH port-forward recipe
  ```
  Under `--json` that is the failure document with `"code": "no_browser"` and `"next": "slipfork open --remote"` (`§10`).
- **Remote VM from a laptop (`slipfork open --remote [--world <WORLD>]`):** Run it on the VM inside your SSH session; it prints the line to paste on the laptop and the URL to browse there. It formats `ssh -N -L 8080:127.0.0.1:<ingress.port> <account>@<host>` from `DaemonStatus.ingress.port` (never a hardcoded literal `80` when running on an ephemeral port; the local forward port is the fixed constant `8080`, with no extra flag) and from the Composition Root's injected `UserIdentity` + `HostIdentity` probes (`crates/slipfork/src/user.rs`; never ambient `$USER`/`$HOSTNAME`). The browsable URL is the origin host of `origin.url()` plus `:8080`:
  ```console
  $ slipfork open --remote                # ingress.port == 80
  world   demo
  ssh     ssh -N -L 8080:127.0.0.1:80 slipfork@slipfork-dev-vm
  app     http://demo.localhost:8080

  $ slipfork open --remote                # ingress.port == 40645 (also prints §2's stderr :80 line)
  world   demo
  ssh     ssh -N -L 8080:127.0.0.1:40645 slipfork@slipfork-dev-vm
  app     http://demo.localhost:8080
  ```
  `--json` returns the same two strings (`"recipe": {"ssh", "url"}`), so no agent has to assemble them.
- **Why `http://<route>.localhost:8080` routes without proxy config (verified):** Modern browsers resolve `*.localhost` to `127.0.0.1`/`::1` (RFC 6761) and send `Host: demo.localhost:8080` over the SSH tunnel to `slipforkd`'s `127.0.0.1:<ingress.port>` listener. `WorldRegistry::resolve` delegates to `LocalHostRoute::parse`, whose `normalize_host` strips any numeric `:port` (`crates/slipfork-world/src/lib.rs:582-594`), routing `Host: demo.localhost:8080` and `Host: feat-ux.demo.localhost:8080` to the exact same worlds.

---

## 7. Zero-Arg `slipfork fork`, Pinned Snapshot Identity & One-Line VCS Errors

`slipfork fork [FORK]` makes `<FORK>` optional (`slipfork drop <FORK>` keeps its required name so destructive teardown is always explicit):

```console
$ git switch -c feat/ux-pass
$ slipfork fork
world     demo:feat-ux-pass (running, changed)
app       http://feat-ux-pass.demo.localhost
worktree  /home/slipfork/demo/.worktrees/feat-ux-pass
cd        cd /home/slipfork/demo/.worktrees/feat-ux-pass
next      slipfork open --world demo:feat-ux-pass | slipfork db --world demo:feat-ux-pass | slipfork logs --world demo:feat-ux-pass | slipfork restart --world demo:feat-ux-pass | slipfork down --world demo:feat-ux-pass
lineage   rootfs a1b2c3d4e5f6 cached · postgres:18 cached · seed 9f8e7d6c5b4a cached · base 4570132bffb8 reused
```

1. **Daemon-side branch derivation & real `snow_domains` validation (`VcsWorkspaceFactory::current_branch`):** The CLI sends `ForkRequest { fork: Option<String>, .. }` and never runs `git` or reads `.git`. When `fork` is `None`, `slipforkd` queries `VcsWorkspaceFactory::current_branch(&worktree_root)` and sanitises the branch name via `crates/slipfork-vcs::sanitize_fork_name` (lowercase ASCII, `/` -> `-`, drop bytes outside `[a-z0-9-]`, collapse repeated `-`, strip leading/trailing `-`, reject empty). `slipforkd` then parses the checkout's `snow.yml`, checks `sanitised != repo.project.as_str()` as a separate check, and validates `validate_fork_name(&sanitised, &snow_domains)` passing the project's real `snow.yml` `domains[].id` slice (never `&[]`).
2. **Fail-closed no-arg cases (never a generated random name, never a placeholder — every message prints the resolved values, and each names its `§10` code):**
   - Detached `HEAD` or bookmark-less `jj`: `slipfork: checkout has no active branch; pass an explicit name with slipfork fork <name>` (`code: no_branch`)
   - Invoked inside a fork worktree: `slipfork: already inside fork worktree '.worktrees/feat-ux-pass'; run slipfork fork from the repository root /home/slipfork/demo` (`code: in_fork_worktree`, extra `worktree`)
   - Fork already exists: `slipfork: fork 'feat-ux-pass' already exists; boot it with slipfork up --world demo:feat-ux-pass` (`code: fork_exists`, extra `world`)
   - Sanitised branch equals `<project>` or collides with `domains[].id` / `test-*` / empty: `slipfork: branch 'demo' collides with project 'demo'; pass a distinct name with slipfork fork <name>` (`code: name_collides_with_project`, extra `name`)
3. **Pinned internal snapshot identity (`user.name=slipfork`):** Synthesizing `refs/slipfork/<fork>/wip` passes `-c user.name=slipfork -c user.email=slipfork@localhost` as global `git` flags before `commit-tree` (and equivalent `--config` overrides in `jj`), never reading, writing, or requiring the developer's `git config`.
4. **One-line VCS errors:** Non-zero `git`/`jj` exits format as `slipfork: <subcommand> failed (exit <n>): <last stderr line>` — zero raw multi-line `git` advice dumps.

---

## 8. Per-World `WorldLineage` (`lineage`), `dag` Deletion, Quiet `shell` & Attributed `logs`

### 8.1 `dag` Deleted From `0.1.0` (Owner Ruling)

`dag` is deleted from `0.1.0` (owner ruling; tombstone in `ADR-0011`, deletion worklist and doc-truth obligations are `M3.28.11`'s, `plan/nodes/M3/M3.28/NODE.md`): `slipfork dag` exits `2` as an unrecognized subcommand, with no rename, no alias, and no `#[deprecated]`.

### 8.2 `WorldLineage` Wire Contract (`M3.28.3`–`M3.28.4`), Renderer & `up -v` Detail

Rootfs and OCI service-image cache outcomes + elapsed `millis: Option<u64>` are returned on `WorldRootfs` and `OciRootfs` (which already observe `RootfsBuildEvent::{CacheHit, CacheMiss}` via the injected monotonic clock) and recorded on each world's handle alongside `ensure_base` / `clone_world` measurements (`WorldEngine::world_lineage(&WorldHandle) -> Result<WorldLineage, EngineError>`). `WorldLineage` travels on `WorldUpSummary.lineage`, `WorldSummary.lineage`, and `WorldProcessReport.lineage` (never on `DaemonStatus`):

- `rootfs: Option<RootfsLineage>` — `{ digest: String /* full 64-hex */, cache: LineageCache /* wire: hit|miss */, millis: Option<u64> }`
- `services: Vec<ServiceImageLineage>` — `{ id: String /* image ref, document key "image" */, cache: ServiceCache /* wire: hit|miss|miss_offline */, millis: Option<u64> }`
- `seed: Option<SeedLineage>` — `{ digest: Option<String /* full 64-hex */>, rebuilt: Option<bool>, millis: Option<u64> }`
- `base: Option<BaseLineage>` — `{ key: String /* full 64-hex */, millis: Option<u64> }`

**Human outcome vocabulary (four words, never a wire enum):** `cached`, `rebuilt <dur>`, `reused`, `sealed <dur>`. The JSON enums `hit` / `miss` / `miss_offline` never appear in human output.

| Node | Human text | When |
|---|---|---|
| `rootfs` | `cached` / `rebuilt <dur>` | `hit` / `miss` |
| `services[]` | `cached` / `rebuilt <dur>` | `hit` / `miss` |
| `services[]` (offline) | `-` | `miss_offline`: neither cached nor rebuilt; the document carries the enum |
| `seed` | `cached` / `rebuilt <dur>` | `rebuilt == Some(false)` / `Some(true)` |
| `base` | `reused` / `sealed <dur>` | `millis == None` (the snapshot already existed) / `Some(ms)` |

**The one renderer** (this is its only home; every other section points here):

```text
lineage   rootfs <digest12> <outcome> · <image> <outcome> · seed <digest12> <outcome> · base <key12> <outcome>
```

A node whose `Option` is `None` (the daemon recorded no such fact) renders `-`; a measured outcome always uses one of the four words above. **Typing rule for every `--json` document:** every key is always present, `null` when unknown — never omitted, never `0`, never `""`.

- **Default `slipfork up` / `fork` / `restart` (`M3.28.8`):** prints that one compact line, e.g. `lineage   rootfs <d12> cached · postgres:18 cached · seed <d12> rebuilt 2.1s · base <k12> sealed 10ms`.
- **Verbose `slipfork -v up` (`M3.28.8` / `M3.28.11`):** adds per-node lineage and `[resources]` detail to human output only:
  ```console
  $ slipfork -v up
  [resources] demo: prefer_remote -> local (not_offered)
  [lineage]   rootfs       a1b2c3d4e5f6   cached        -
  [lineage]   postgres:18  -              cached        -
  [lineage]   seed         9f8e7d6c5b4a   rebuilt       2104ms
  [lineage]   base         4570132bffb8   sealed        10ms
  ```

### 8.3 Quiet `shell` & Attributed `logs` (`M3.28.11`)

- **Quiet world entry (`slipfork shell`, `exec`, `db`, `hook run`):** `BtrfsEngine::entry_command` passes `--wd` inside `nsenter` with host `WorldEntryCommand.cwd = "/"` so the entry shell starts on a valid in-world dentry and `sh: 0: getcwd() failed` never appears. Interactive `slipfork shell` prints one orientation line to stderr before handing over the TTY, phrased so the user does not try host tools inside the world: `world rootfs has no host tools (no ps, no curl); exit and run slipfork ps | slipfork logs | slipfork open`.
- **Attributed `slipfork logs`:** When streaming more than one source (`slipfork logs` with no target), every line is prefixed with `<source>: ` (`api: `, `api:pre-deploy: `); a single named target (`slipfork logs api`) prints unprefixed raw lines.

---

## 9. One `Local` Origin (`struct WorldOrigin`)

`0.1.0` has **one** origin kind: untagged `struct WorldOrigin(LocalOrigin)`, minted solely via `WorldOrigin::local(&route, ingress_port)`. The outbound-sharing surface is deleted in `M3.28.15` (tombstone: `ADR-0011`; worklist: `plan/nodes/M3/M3.28/NODE.md`); `slipforkd` origin rendering and `WorldRegistry::resolve` therefore have zero shared-origin branch and zero `_ =>` catch-all arm.

---

## 10. Single Normative `0.1.0` CLI Command Table & `--json` Documents

**Global flags** (`clap` `global = true`, accepted before or after the subcommand): `--json` (machine output), `-v` / `--verbose` (extra **human** detail, ignored under `--json`), and `--state-dir <DIR>` (`$XDG_CACHE_HOME/slipfork` or `<home>/.cache/slipfork`). There is no client `--runtime-dir`: the client's socket is always `<state_dir>/run/daemon.sock`, and the flag belongs to `slipforkd` (`§2` rule 6).

**Every verb owns its document.** The table below is the complete, authoritative list of top-level `--json` documents: there is no shared envelope type, wrapper, or projection layer. Three shapes exist, and each row says which it is: an **object** document carrying `"ok": true`; a **collection** document that is the bare array or map; or **no document** at all. Every key in every document is always present (`null` when unknown), per `§8.2`.

**The failure document (defined once here; every verb uses it).** A `--json` failure writes exactly one document to **stdout** and exits `1` (`2` for `clap` usage errors; the streaming verbs in the table mirror the child's exit code instead):

```text
{"ok": false, "code": "<stable snake_case>", "error": "<one actionable line>", "next": "<exact command>" | null, …extras}
```

The base is open, but the extra keys are a **closed list per code** — no code carries any other key:

| `code` | extras | emitted by |
|---|---|---|
| `no_daemon` | `path` | every verb except `up` / `fork` when nothing answers on `<state_dir>/run/daemon.sock` (`§1`) |
| `unknown_world` | `world` | any world-scoped verb whose selector is unknown or never booted |
| `not_running` | `world` | `restart` on a `stopped` or never-booted world (`§4`) |
| `boot_failed` | `world`, `state` | `up` / `fork` / `restart` (`§4`) |
| `no_browser` | — | `open` on a headless host (`§6`) |
| `fork_exists` | `world` | `fork` when the derived name already exists (`§7`) |
| `no_branch` | — | `fork` with no derivable branch (`§7`) |
| `in_fork_worktree` | `worktree` | `fork` invoked inside `.worktrees/<fork>` (`§7`) |
| `name_collides_with_project` | `name` | `fork` whose derived name equals the project or collides with `domains[].id` / `test-*` / empty (`§7`) |
| `not_root` | — | `daemon install` (`§2`) |
| `ingress_missing` | `addrs` | any verb that needs the origin when the daemon reported no HTTP ingress listener (an invariant violation, so the CLI fails closed instead of guessing a port) |
| `variable_collision` | `key` | `snow resolve` when a `snow.yml` variable name collides with `ok` / `code` / `error` / `next` |

| Command | Summary | `--json` stdout document |
|---|---|---|
| `slipfork` / `slipfork status` | Daemon ingress + per-project canonical/`:fork` world tree + lineage digests + next steps (process rows are `slipfork ps --json`) | object: `{"ok": true, "pid": u32, "ingress": {"addrs": [str], "port": u16}, "forward_port": u16\|null, "worlds": [{"world": str, "parent": str\|null, "state": "running\|failed\|stopped", "origin": str, "worktree": str, "lineage": WorldLineage}]}` |
| `slipfork up [--world <W>] [--no-dump \| --refresh-dump] [--resources local] [--host-caches]` | Boot unbooted/`stopped`/`failed` world, or confirm `running` (`changed: false`); the only verb besides `fork` that may auto-spawn `slipforkd` | object: `{"ok": true, "world": str, "state": "running", "changed": bool, "restarted": [str], "worktree": str, "origin": str, "lineage": WorldLineage, "services": [WorldServiceSummary], "resources": [{"id": str, "strategy": str, "resolved": str, "reason": str}], "next": [str]}` |
| `slipfork restart [--world <W>]` | Stop components + `crun` services in place, then run the shared boot pipeline (`changed: true`) | same document as `slipfork up --json` (`"changed": true`) |
| `slipfork ps [--world <W>]` | `world` state + `lineage` header + supervised component & `crun` backing-service table (canonical per-world process view) | object: `{"ok": true, "world": str, "state": "running\|failed\|stopped", "lineage": WorldLineage, "services": [{"world": str, "id": str, "kind": "component\|backing_service", "state": "running\|exited\|stopped", "pid": u32\|null, "port": u16\|null, "uptime_secs": u64\|null}]}` |
| `slipfork open [--world <W>] [--remote]` | Open typed `WorldOrigin` via `BrowserOpener::open_origin`, or print the SSH `:8080 -> 127.0.0.1:<ingress.port>` recipe and laptop URL | object: `{"ok": true, "world": str, "origin": str, "recipe": {"ssh": str, "url": str}\|null}` |
| `slipfork fork [FORK] [--base <REV>] [--no-dump \| --refresh-dump] [--resources local] [--host-caches]` | Branch-derived (or explicit) `.worktrees/<fork>` + Btrfs CoW world + `cd` + `lineage`; may auto-spawn `slipforkd` | same document as `slipfork up --json` (`"changed": true`) |
| `slipfork drop <FORK>` | Destroy fork subvolume, `.worktrees/<fork>`, branch, and `refs/slipfork/<fork>/wip` | object: `{"ok": true, "dropped": bool}` |
| `slipfork down [--world <W>]` | Stop world services while keeping its worktree (idempotent) | object: `{"ok": true, "released": bool}` |
| `slipfork list` | Flat `WORLD / STATE / ORIGIN / NEXT` table across projects; `NEXT` is that world's first `next` command (`§1`), paste-ready | collection: `[{"world": str, "state": "running\|failed\|stopped", "origin": str, "worktree": str}]` |
| `slipfork prune [--dry-run \| --yes]` | Preview (default/`--dry-run`) or reap worlds whose checkout directory is gone | object: `{"ok": true, "dry_run": bool, "worlds": [str], "snapshots": [str], "artifacts": [str], "rootfs_digests": [str], "oci_digests": [str], "reclaim_bytes": u64}` |
| `slipfork daemon install [--user <U>] [--bin-dir <D>]... [--forward-port <P>]` | Install `/etc/systemd/system/slipforkd.{socket,service}` + `/etc/apparmor.d/slipfork` and `enable --now` | object: `{"ok": true, "user": str, "socket": "wrote\|unchanged", "service": "wrote\|unchanged", "apparmor": "wrote\|unchanged"}` |
| `slipfork daemon unit [--only socket\|service\|apparmor] [--bin-dir <D>]... [--forward-port <P>]` | Render systemd socket/service or flat 4-line AppArmor profile to stdout (render seam; `daemon install` is the only install path) | object: `{"ok": true, "socket": str\|null, "service": str\|null, "apparmor": str\|null}` |
| `slipfork doctor` | Pre-daemon Btrfs + userns/netns + advisory `:80` `ingress` check | object: `{"ok": true, "healthy": bool, "uid": u32, "gid": u32, "state_dir": str, "probed_path": str, "fstype": str, "filesystem": str, "btrfs_super_magic": bool, "subvolume_ioctl": {...}, "noatime": bool, "unshare_userns": {...}, "netns": {...}, "ingress": {"status": str, "port": u16\|null, "listen": [str]\|null, "hint": str\|null}, "fixes": [str]}` |
| `slipfork exec [--world <W>] [--cwd <D>] -- <ARGV>...` | Quiet non-interactive execution inside the world's `slipfork-world-init` PID 1 | **no document**: rejects `--json` |
| `slipfork shell [--world <W>]` | Quiet interactive TTY shell inside the world (requires TTY; prints 1 stderr stage hint) | **no document**: rejects `--json` |
| `slipfork logs [--world <W>] [TARGET] [--follow] [--stderr] [--lines <N>]` | Retained component/hook logs (`<source>: ` prefix when multi-source) | **NDJSON** lines `{"stream": "stdout\|stderr", "source": str, "text": str}` — the only stream document |
| `slipfork hook run <HOOK> [--world <W>] [--no-tty]` | Re-run `<component>:<phase>` hook (interactive TTY `pdb` or `--no-tty` via daemon) | object: `{"ok": true, "world": str\|null, "exit_code": i32\|null}`; the CLI mirrors `exit_code` |
| `slipfork db [DB_ID] [--world <W>]` | Interactive `psql` inside the running `postgres:<v>` `crun` container (TTY) | **no document**: rejects `--json` |
| `slipfork redis [CLUSTER_ID] [--world <W>]` | Interactive `redis-cli` inside the running `redis:<v>` `crun` container (TTY) | **no document**: rejects `--json` |
| `slipfork forward [--world <W>] [TARGET...]` | Bind daemon-owned `127.0.0.1:0` bridges into the world until Ctrl-C / disconnect | object: `{"ok": true, "world": str, "targets": [{"id": str, "host_addr": str, "uri": str}]}` |
| `slipfork snow resolve [--file <F>] [--mode <M>] [--component <C>] [--expose-secrets]` | Validate `snow.yml`, persist missing `!!` secrets, print resolved variables (secrets redacted) | collection: `{<KEY>: str \| null}` (`null` = redacted; `--expose-secrets` fills it). The keys `ok` / `code` / `error` / `next` are reserved and a `snow.yml` variable using one fails closed: `slipfork: snow.yml variable 'ok' collides with the reserved ok/code/error/next keys` |
| `slipfork secret set [--force] <KEY>` / `unset <KEY>` / `list` | Manage `0600` project vault secrets through `slipforkd` (`set` reads stdin) | object `{"ok": true, "key": str}` / object `{"ok": true, "key": str, "removed": bool}` / collection `[str]` |
| `slipfork auth login [- \| '<url>#handle=<64-hex>']` | Pair `tpmd` / P-256 device key over `ospp-v1` (`-` reads stdin) | object: `{"ok": true, "key_id": str, "backend": str, "account": str\|null}` |
| `slipfork support` | Report embedded integration tiers and `required_evidence` (`support/integrations.toml`) | collection: `[{"id": str, "category": str, "tier": str, "evidence": {...}, "required_evidence": [str]}]` |

**The four verbs with no document** (`exec`, `shell`, `db`, `redis`) reject `--json` because the child owns stdout and no single document is possible: ``slipfork: <verb> passes the child's stdout through and cannot emit one JSON document; drop --json and use the exit code``.

**`drop` and `down` are idempotent:** an already-absent fork or an already-released world exits `0` with `dropped: false` / `released: false`. **`--world <W>`** accepts the canonical `WorldId` selector grammar (`<project>`, `<project>:<fork>`, or bare `<fork>` inside that project's checkout); values printed in any `world` field are `<project>:<fork>` and paste back verbatim.
