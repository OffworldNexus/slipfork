# Slipfork `0.1.0` Clean-Sheet CLI UX Specification

Slipfork runs your cloud environment locally in a box; CI is the clutch. One `slipfork up` boots every service and database in its own daemonless `crun` container behind `.localhost`, and `slipfork fork` clones the live stack in ~20 ms.

```console
$ slipfork up
✓ Base snapshot ready (postgres:18, redis:7, api, front)  1.4s
✓ api setup & pre-deploy [2/2]                            0.6s
● demo  http://demo.localhost

$ slipfork
● demo  http://demo.localhost  ~/demo
├─ pg     running  :5432  12s
├─ cache  running  :6379  12s
├─ api    running  :8000  11s
├─ front  running  :5173  11s
└─ feat-auth  ● http://feat-auth.demo.localhost  ~/demo/.worktrees/feat-auth
```

!!! info "Zero-Noise Terminal Hierarchy"
    Dynamic column widths measure visible text after stripping ANSI escapes, primary URLs render bold cyan, `running` renders green, `failed` renders red, and `NO_COLOR`, non-TTY pipes, and `--json` emit **zero** `\x1b[` sequences. Only `up` and `fork` auto-spawn `slipforkd`; read-only bare `slipfork` prints `○ No worlds running` (exit `0`) when idle without starting a daemon.

## 1. Pod Lifecycle & Fleet Tree (`slipfork`, `ps`, `up`, `restart`, `down`, `fork`, `drop`)

```mermaid
flowchart LR
  UP["slipfork up [-F ...]"] -->|boots pod + base snapshot| RUN["● &lt;world&gt; http://&lt;world&gt;.localhost"]
  RUN -->|slipfork fork [name]| FORK["Btrfs CoW clone (~20ms)<br/>.worktrees/&lt;fork&gt; + volumes"]
  RUN -->|slipfork restart| RUN
  FORK -->|slipfork drop [fork]| CLEAN["Reaps pod + worktree<br/>(dirty guard unless -f)"]
```

- **Bare `slipfork` (`= slipfork status`) & `slipfork ps [-w <world>]`:** Bare `slipfork` renders the fleet tree across all running worlds (`● <world>  <url>  ~/path`, child `├─ <service>  <state>  :<port>  <age>` rows, and `└─ <fork>  ● <url>  ~/path` forks) with **zero** `slipforkd <pid>` header, **zero** lineage hashes (gated behind `-v`), and **zero** `next` spam. A single yellow stderr notice (`! Port 80 not installed — run sudo slipfork daemon install for port-less URLs`) prints *only* when ingress is not `:80`; `ps` scopes the exact same tree to `cwd`'s world (`@slipfork/opencode` reads `status --json`).
- **`slipfork up [-w] [-F <f1,f2>] [--no-default-features] [--no-dump|--refresh-dump] [-v]`:** Streams live spinners per phase (`[step/total]` + elapsed time) collapsing to `✓` lines and `● <world>  <url>`. Re-running `up` on a running world prints a crisp 2-line confirmation (`● <world>  <url> (already running)` + `  next: slipfork restart to apply changes`); if `setup` or `pre-deploy` fails, `up` prints `Forkfile:<line>`, the last 15 output lines inline, preserves the world, and prints `next: slipfork logs <svc> | slipfork shell <svc>`.
- **`slipfork restart [-w] [-F] [--no-default-features] [-v]` & `slipfork down [-w] [--all]`:** `restart` stops pod containers while preserving the Btrfs subvolume and re-runs the boot pipeline (`not_running` fails closed naming `slipfork up`). `down` stops `cwd`'s world (`--all` stops every running world) and **fails closed** (`exit 1`) on an unknown `-w/--world`.
- **`slipfork fork [name] [-F] [--no-default-features]` & `slipfork drop [fork] [-f|--force]`:** `fork` derives `[name]` from the active Git/Jujutsu branch when omitted, creates `.worktrees/<fork>`, and CoW-clones the world subvolume (databases + `volume` dirs). `drop` defaults `[fork]` to `cwd` when inside a fork worktree, refuses dirty/untracked worktrees unless `-f/--force`, and **fails closed** on an unknown fork name.

## 2. Multiplexed Logs, Container Entry, Tasks & Remote Open

- **`slipfork logs [target...] [-f|--follow] [-n|--lines N] [-w]`:** Merges `stdout` and `stderr` chronologically by default and concurrently multiplexes all matching targets with a dynamically aligned, per-service color-coded `<target> │ ` gutter (1 named target prints raw unprefixed lines unless `-v`). Under `-f/--follow`, `logs` stays alive across `restart` and `down` + `up` cycles, printing a dimmed `── restarted <time> ──` divider when containers respawn.
- **Per-Service Pod Entry (`shell`, `exec`, `run`, `env`):**
  - **Service Auto-Selection:** `shell [service]`, `exec [-s/--service <id>]`, and `env [-s/--service <id>]` auto-select the service when the active `Forkfile` has 1 service **or** caller `cwd` is inside `<worktree>/<service_dir>`, failing closed at repo root when ambiguous (`error: multiple services (api, front) — next: slipfork shell api`).
  - **`slipfork shell [service] [-w]`:** Enters the service's running/materialized `crun` container at `<worktree>/<service_dir>` with clean prompt `(slipfork:<world>/<service>) $ ` and **zero** stderr lecture.
  - **`slipfork exec [-s <service>] [-w] <cmd...>`:** Direct non-interactive container execution inheriting stdio and exit code (`--` is optional via trailing varargs).
  - **`slipfork run <task> [args...] [-w]`:** Executes a `Forkfile` `task <name> in <service>` inside that service's container.
  - **`slipfork env [-s <service>] [--reveal] [-F] [--no-default-features] [-w]`:** Prints resolved environment keys for `<service>` with secret-tainted values masked as `[REDACTED]` unless `--reveal` (`--json` emits `{<KEY>: str | null}`).
- **Data Clients & Remote-Ready `open`:**
  - **`slipfork db [db_id] [-w]` & `slipfork redis [id] [-w]`:** Piped stdin prints the world's published connection `url` fact byte-clean (exactly what the app reads as `DATABASE_URL`/`REDIS_URL`, with no ANSI or spinner); a TTY opens `psql`/`redis-cli` interactively inside the backing container.
  - **`slipfork open [-r|--remote] [-p|--port 8080] [-w]`:** Opens the world URL locally, or — when `-r/--remote` is passed **or** `$SSH_CONNECTION` is set on a headless host — prints the ready-to-paste `ssh -N -L <port>:127.0.0.1:<ingress_port> <user>@<host>` tunnel and `http://<world>.localhost:<port>` URL (`slipforkd` ingress strips `:<any_port>` on `.localhost` `Host` headers).
- **Secrets, Host Health & Admin:**
  - **`slipfork secret set <KEY>` | `unset <KEY>` | `list`:** Stores local `0600` vault overrides. `set` takes no value argument: a TTY gets a hidden prompt (echo off), a pipe is read verbatim; remote OSPP secrets never touch disk.
  - **`slipfork doctor [-v]`:** Leads with `✓ Ready` or `✗ N issue(s)` followed by a crisp 3-row checklist (`✓ Btrfs storage`, `✓ Linux namespaces`, `✓ Port 80 ingress` or advisory `!` hint; `-v` expands raw mount/fstype/uid details).
  - **`slipfork prune [-y|--yes]`**, **`slipfork forward [target...] [-w]`**, **`slipfork auth <url>`**, and **`slipfork daemon install|unit`**.

## 3. Complete `0.1.0` Command & `--json` Contract

Global flags (`global = true`): `--json`, `-v/--verbose`, `--state-dir <DIR>`. Failures format as `error: <1 line>\n  next: <remedy>` (or `{"ok": false, "code": str, "error": str, "next": str|null, ...}` under `--json`, exit `1`).

| Command | Purpose | `--json` Document |
|---|---|---|
| `slipfork` / `slipfork status` | Fleet tree of running worlds, services, and forks (`○ No worlds running`, exit `0` when idle) | `{"ok": true, "ingress_port": u16\|null, "worlds": [{"world": str, "parent": str\|null, "state": str, "origin": str, "worktree": str, "lineage": WorldLineage, "services": [WorldServiceSummary]}]}` |
| `slipfork ps [-w <W>]` | World-scoped pod tree (`cwd` default) | `{"ok": true, "world": str, "state": str, "origin": str, "worktree": str, "lineage": WorldLineage, "services": [WorldServiceSummary]}` |
| `slipfork up [-w <W>] [-F <F>] [--no-default-features] [--no-dump\|--refresh-dump]` | Boot or confirm running pod (`auto-spawns slipforkd`) | `{"ok": true, "world": str, "state": "running", "changed": bool, "restarted": [str], "worktree": str, "origin": str, "lineage": WorldLineage, "services": [WorldServiceSummary]}` |
| `slipfork restart [-w <W>] [-F <F>] [--no-default-features]` | Restart pod services in place keeping CoW state | Same as `up --json` (`"changed": true`) |
| `slipfork down [-w <W>] [--all]` | Stop world pod (`--all` stops all; unknown `-w` fails closed) | `{"ok": true, "stopped": [str]}` |
| `slipfork fork [NAME] [-F <F>] [--no-default-features]` | Branch-derived `.worktrees/<fork>` + ~20 ms Btrfs CoW clone | Same as `up --json` (`"changed": true`) |
| `slipfork drop [FORK] [-f\|--force]` | Destroy fork pod + worktree (`cwd` default; dirty guard unless `-f`; unknown fails closed) | `{"ok": true, "dropped": str}` |
| `slipfork logs [TARGET...] [-f] [-n <N>] [-w <W>]` | Chronological merged `stdout`+`stderr` multiplexer (survives restarts under `-f`) | NDJSON `{"ts_ms": u64, "target": str, "stream": "stdout\|stderr\|event", "text": str}` |
| `slipfork run <TASK> [ARGS...] [-w <W>]` | Run `Forkfile` `task` inside its service container | Rejects `--json` (streams child stdio; mirrors exit code) |
| `slipfork env [-s <SVC>] [--reveal] [-F <F>] [--no-default-features] [-w <W>]` | Print resolved service `env` (`[REDACTED]` unless `--reveal`) | `{<KEY>: str \| null}` (`null` when redacted) |
| `slipfork shell [SVC] [-w <W>]` | Interactive `(slipfork:<world>/<svc>)` shell in service container | Rejects `--json` (TTY required) |
| `slipfork exec [-s <SVC>] [-w <W>] <CMD...>` | Execute `<CMD...>` in service container (`--` optional) | Rejects `--json` (mirrors child exit code) |
| `slipfork db [DB_ID] [-w <W>]` / `redis [ID] [-w <W>]` | `psql` / `redis-cli` in the backing container on a TTY; piped stdin prints the world's published URI | Rejects `--json` (piped URI, or streams the client on a TTY) |
| `slipfork open [-r] [-p <PORT>] [-w <W>]` | Open `.localhost` URL or print SSH `-L` tunnel (`$SSH_CONNECTION` auto-remote) | `{"ok": true, "world": str, "origin": str, "remote": {"ssh": str, "url": str}\|null}` |
| `slipfork secret set <K>` / `unset <K>` / `list` | Manage local `0600` vault overrides (`set` takes no value: hidden TTY prompt, or piped stdin) | `{"ok": true, "key": str}` / `{"ok": true, "key": str, "removed": bool}` / `[str]` |
| `slipfork doctor [-v]` | Verdict-first 3-row host checklist (Btrfs, namespaces, `:80` ingress) | `{"healthy": bool, "btrfs": {...}, "namespaces": {...}, "ingress": {...}, "fixes": [str]}` |
| `slipfork migrate [FILE] [-o\|--output <PATH>] [--stdout] [-f\|--force]` | One-way `Forkfile` → `Forkfile` conversion (`./Forkfile` → `./Forkfile`; refuses an existing `Forkfile` unless `-f`; `--stdout` previews without writing) | `{"ok": true, "input": str, "output": str, "services": u32, "pg_clusters": u32, "env_groups": u32}` (a JSON string of the converted text under `--stdout`) |
| `slipfork prune [-y\|--yes]` | Preview or reap orphaned worlds, snapshots, and unreferenced caches | `{"ok": true, "dry_run": bool, "worlds": [str], "snapshots": [str], "reclaim_bytes": u64}` |
| `slipfork forward [TARGET...] [-w <W>]` | Hold `127.0.0.1:0` host TCP bridges into pod until Ctrl-C | `{"ok": true, "world": str, "targets": [{"id": str, "host_addr": str, "uri": str}]}` |
| `slipfork auth <URL>` | Pair machine P-256 key over `ospp-v1` (`<url>#handle=<handle>`) | `{"ok": true, "public_key": str, "account": str, "machine": str}` |
| `slipfork daemon install` / `daemon unit` | Install or render `:80` `slipforkd.{socket,service}` + AppArmor profile | `{"ok": true, "user": str, "socket": str, "service": str, "apparmor": str}` |
