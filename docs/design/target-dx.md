# Slipfork M2–M4 Target Developer Experience Specification (`docs/design/target-dx.md`)

- **Status:** Accepted Target DX Specification (`ADR-0001`..`ADR-0008`, `H-001`..`H-010`)
- **Visibility:** Internal design spec (`exclude_docs` in `zensical.toml`). Per **Core Rule #4 (Zero Vaporware in Published Docs)**, sections here promote into published `docs/` and `README.md` **only** as each binary capability ships and passes conformance tests (`M2.19`).

---

## 1. Milestone 2 Target DX: Unified `slipforkd`, Zero-Setup Auto-Spawn, `OSPP v1` & Rescue Tooling

### 1.1 Zero-Setup Auto-Spawn vs Optional Port-80 Systemd Socket Activation

Installing the workspace places `slipfork`, `slipforkd`, and `git-remote-slipfork` onto `PATH` (source install today; prebuilt binaries in M4):

```console
$ cargo install --git https://github.com/OffworldNexus/slipfork slipfork

# Zero-setup default (containers, CI, laptops without root):
# Running `slipfork up` or `slipfork fork` automatically starts
# `slipforkd --listen 127.0.0.1:0 --state-dir <state_dir> --runtime-dir <runtime_dir>`
# on <runtime_dir>/daemon.sock (0600) and prints http://acme.localhost:<port>!

# Optional workstation port-80 setup (PID 1 systemd binds :80 + :7080; slipforkd runs as User=<developer-user>):
$ slipfork daemon unit --only socket  | sudo tee /etc/systemd/system/slipforkd.socket > /dev/null
$ slipfork daemon unit --only service | sudo tee /etc/systemd/system/slipforkd.service > /dev/null
$ sudo systemctl daemon-reload && sudo systemctl enable --now slipforkd.socket
```

`slipforkd` runs **unprivileged as `User=<developer-user>`** (resolved once from the invoking account via `getpwuid_r(geteuid())` into non-template `slipforkd.service`) with an empty `CapabilityBoundingSet=` and acts as the single local authority (`H-010` #1): it holds the persistent `OSPP v1` WebSocket session, resolves `snow.yml` secrets/resources, supervises CoW worlds, and routes `.localhost` (via `snow.yml`'s `ingresses` longest-prefix table) + shared tunnel traffic. **OSPP is never required to boot local worlds:** offline laptops work out of the box from `snow.yml` + `~/.local/share/slipfork/vaults/<project>.json` (`0600`; `slipfork.toml` is optional).

### 1.2 `slipfork up`, Diagnostic `[resources]` Lines & Seed Dump Flags (`--no-dump | --refresh-dump`)

`up` passes the caller's explicit `cwd` over `<runtime_dir>/daemon.sock` to boot the world selected by `--world <fork>` or `cwd`'s nearest ancestor (`<vcs_root>/.worktrees/<fork>[/...]` -> fork `<fork>`, repo root -> `canonical`), waits for declared component `internal_ports` readiness before printing `[origin]`, and is idempotent when re-run (`down` stops services immediately without waiting for readiness):

```console
$ slipfork up                           # Paired laptop (:80 socket activation; :<port> when auto-spawned)
[secrets]   ensured secrets::SECRET_KEY -> ~/.local/share/slipfork/vaults/acme.json (0600)
[resources] s3_buckets.media: prefer_remote -> remote (https://s3.dev.example.com/acme-media)
[resources] pg_databases.main: prefer_remote -> cached dump sha256:9f4c12..
[hook]      api:pre-deploy: uv run python manage.py migrate
[ready]     api (internal :8000 in world netns)
[origin]    http://acme.localhost

$ slipfork up --no-dump                 # Skip seed dump and run clean initdb -> pre-deploy migrations
$ slipfork up --refresh-dump            # Require live OSPP dump check (mutually exclusive with --no-dump; fails closed if offline)
$ slipfork up --resources local         # Force hermetic world-local S3 + clean initdb (safe for agent runs)
```

- **Offline / Unpaired Transparency:** When unpaired or offline under default `prefer_remote`, `ResourcePlan::resolve` never degrades silently — it prints `[resources] pg_databases.main: prefer_remote -> local initdb (OSPP unpaired)` and exposes `{strategy, resolved, reason}` under `--json`.
- **Port-Aware Single Origin & In-Netns Loopback Bridge (`H-010` #2):** `WorldOrigin::local(&route, ingress_port)` renders `http://acme.localhost` on `:80` and `http://acme.localhost:<port>` under auto-spawned `--listen`, while `ReflinkNetnsEngine` binds an **in-netns loopback TCP bridge** on `127.0.0.1:<ingress_port>` inside each world back to `slipforkd` so `$BASE_URL` resolves identically from host browsers and inside `slipfork exec` / `slipfork test` (`snow.yml` `ingresses` prefixes route `/api`, `/`, etc. to their target component ports). **Never connect to host `127.0.0.1:5432` or `:8000`**; use the printed `origin`, `slipfork exec`, or `slipfork forward --json`.
- **World Rootfs & `--host-caches` (design of record: `docs/design/world-base-image.md` §1–§3, `ADR-0009`):** every world's `L0` rootfs is built once from the project's own `snow.yml` `images:` `Dockerfile` stage, selected by `[world]` in `slipfork.toml` (`world.image` when `images:` declares more than one `BuildImage`, `world.stage` for a compiled build stage) and reused read-only by every fork and test slot; `postgres:<v>`, `redis:<v>`, and SeaweedFS run as daemonless `crun` containers on the world's `127.0.0.1`. `--host-caches` adds the host toolchain caches **read-only** only when the ELF/libc ABI probe matches, failing closed with the measured mismatch otherwise — it is never a `slipfork.toml` key.

### 1.3 First-Class Hook Failure Rescue UX & Direct CLI TTY Entry (`hook run`, `logs`, `shell`, `exec`)

Hooks (`<component>:<phase>`, e.g. `api:pre-deploy`) stream stdout/stderr live from `slipforkd`. On non-zero exit, Slipfork prints the **full un-truncated output**, **preserves the failed world** (listed in `slipfork list`), and prints three copy-pasteable rescue commands using the canonical `--world <fork>` selector:

```console
$ slipfork fork feat-billing
[hook] api:pre-deploy: uv run python manage.py migrate
... (full untruncated traceback) ...
error: hook 'api:pre-deploy' exited with code 1 in world 'feat-billing'
  argv: ["sh", "-eu", "-c", "uv run python manage.py migrate"]
  log:  ~/.cache/slipfork/acme/worlds/feat-billing/logs/api.pre-deploy.log

Rescue commands (world 'feat-billing' kept alive for inspection):
  slipfork hook run api:pre-deploy --world feat-billing          # add --no-tty in non-interactive shells
  slipfork logs --world feat-billing api:pre-deploy
  slipfork shell --world feat-billing                            # or: slipfork exec --world feat-billing -- <cmd>
```

- **Direct CLI World Entry — Zero `SCM_RIGHTS` (`H-010` #1):** `slipforkd` is the **sole owner** of `snow.yml` (`modes.local`) environment resolution (`@slipfork/opencode` only routes `shell` calls to `slipfork exec --cwd <cwd>`, `ADR-0004`). For `shell`, interactive `hook run` (`pdb`), and `exec`, `slipforkd` returns `WorldExecTarget` (`nsenter` / `incus exec` world argv + resolved `ExecEnv`) over `<runtime_dir>/daemon.sock` (`0600`) and the thin `slipfork` CLI enters the world directly inheriting its native terminal `stdin`/`stdout`/`stderr`; for `db` and `redis` the same message carries the service-container entry (`WorldEngine::service_entry_command` -> `crun exec` into the running `postgres:<v>` / `redis:<v>` container, `ADR-0009` §3), so `psql`/`redis-cli` run from the container's own rootfs rather than the app image (`shell` and default `hook run` require a TTY, failing closed naming `hook run --no-tty` or `slipfork exec`; a stopped backing service fails closed `ServiceNotRunning`).

### 1.4 Service Clients (`db`, `redis`, `forward`), Secrets, VCS (`git` + `jj`) & `OSPP v1` Pairing

```console
$ slipfork exec --world feat-billing -- uv run pytest -k billing   # Non-interactive agent/CI execution (daemon-resolved env)
$ slipfork db --world feat-billing                                 # Interactive psql INSIDE the postgres service container (crun exec, native TTY)
$ slipfork redis --world feat-billing                              # Interactive redis-cli INSIDE the redis service container (crun exec, native TTY)
$ slipfork forward --world feat-billing --json                     # Daemon-owned 127.0.0.1:0 bridge (lives until Ctrl-C/disconnect):
{"world":"feat-billing","targets":[{"id":"pg_databases.main","host_addr":"127.0.0.1:43182","uri":"postgres://127.0.0.1:43182/main"}]}

$ git pull slipfork feat-billing                                                           # Committed branch refs/heads/feat-billing (@- in Jujutsu)
$ git fetch slipfork '+refs/slipfork/feat-billing/wip:refs/slipfork/feat-billing/wip'     # Dirty + untracked snapshot (@ in Jujutsu; equals base when clean)

$ printf '%s' "$STRIPE_KEY" | slipfork secret set STRIPE_SECRET_KEY          # --force to rotate existing key
$ printf '%s\n' "$PAIR_URL_WITH_FRAGMENT" | slipfork auth login -            # Stdin keeps #handle= out of argv
[auth] paired p256:02a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90 (backend=tpmd:sealed)
```

- **`db` / `redis` Target the Service Container, Never the App Rootfs (`ADR-0009` §2–§3):** `slipforkd` answers `exec_target` with `WorldEngine::service_entry_command`, i.e. a `crun exec` entry into the world's running `postgres:<v>` / `redis:<v>` container, so `psql` and `redis-cli` come from the container's own rootfs and the project `Dockerfile` never needs DB client packages (and dump/restore versions can never skew). A stopped or absent backing service fails closed naming the service id (`ServiceNotRunning`) instead of silently entering the world. `slipfork forward` is unchanged: `slipforkd` binds a host `127.0.0.1:0` listener and bridges it with `dial_world_tcp` to the in-world port.

### 1.5 Canonical `--json` Contracts, Lifecycle (`list`, `down`, `prune`) & `share`

```console
$ slipfork list
WORLD          STATE     ORIGIN                                 WORKTREE
canonical      running   http://acme.localhost                  /repo
feat-billing   failed    http://feat-billing.acme.localhost     /repo/.worktrees/feat-billing

$ slipfork down --world feat-billing       # Stop world services while keeping .worktrees/feat-billing
$ slipfork prune --dry-run                 # Preview orphaned worlds/snapshots (--yes to delete)
```

- **Canonical `--json` Schemas (normative home; `--json` writes only JSON to `stdout` and suppresses spinners/ANSI):**
  - `slipfork list --json` -> `[{"world": "feat-billing", "state": "running|failed|stopped", "origin": "http://feat-billing.acme.localhost", "worktree": "/repo/.worktrees/feat-billing"}]` (**exactly one** `origin` URL per world, port-qualified when `ingress_port != 80`).
  - `slipfork up --json` / `fork --json` -> `{"world": "...", "origin": "http://...", "resources": [{"id": "...", "strategy": "...", "resolved": "remote|local", "reason": "..."}]}`.
  - `slipfork support --json` -> `[{"id": "...", "category": "...", "tier": "...", "evidence": {...}, "required_evidence": [...]}]`.
- **Sharing (`slipfork share [--stop] [--world <fork>]`, `ADR-0005` §2):** Toggling sharing inside `slipforkd` tears down the live world and re-materializes from its `base`/fixture snapshot with the single new `WorldOrigin` (discarding uncommitted post-snapshot DB mutations while preserving worktree files; `share --stop` on an unshared world is an idempotent no-op). Until `tunnel-weaver` publishes `weaved`'s IPC specification (`M2.22`), `slipfork share` fails closed telling the caller to use the world's `.localhost` origin (or `EdgeError::NoForwardListener` when `slipforkd` runs without a forward listener).

### 1.6 Complete M2–M3 CLI Command Table

Every world-scoped command selects a fork exclusively via `--world <fork>` (zero positional fork aliases; omitting `--world` resolves `cwd`'s nearest ancestor). M1's PoC `slipfork snap` command is superseded by `up`/`fork` and deleted in `M2.16` (with published docs updated in `M2.19`).

| Command | Milestone | Behavior |
|---|---|---|
| `slipfork up [--world <fork>] [--no-dump \| --refresh-dump] [--resources local] [--host-caches] [--json]` | M2 | Boot world in `slipforkd` (auto-spawning if needed), materialize/validate the `Dockerfile` `L0` rootfs, start `crun` backing services, await readiness, print single `origin` |
| `slipfork down [--world <fork>]` | M2 | Stop world services without deleting its worktree (idempotent) |
| `slipfork fork <fork> [--no-dump \| --refresh-dump] [--resources local] [--host-caches] [--json]` | M2 | Create `.worktrees/<fork>` (`git` or `jj`) + CoW world in `slipforkd` (same `L0` rootfs digest, `0` rootfs copy) |
| `slipfork drop <fork>` | M2 | Destroy fork world and remove `.worktrees/<fork>` |
| `slipfork exec [--world <fork>] [--cwd <dir>] -- <argv>...` | M1 + M2 | Fetch `WorldExecTarget` from `slipforkd` and run non-interactively inside world |
| `slipfork list [--json]` | M2 | List worlds with state, worktree path, and **exactly one** `origin` URL |
| `slipfork prune [--dry-run \| --yes] [--json]` | M2 | Preview or garbage-collect orphaned worlds and unreferenced snapshots |
| `slipfork shell [--world <fork>]` | M2 | Enter world directly inheriting native TTY `$SHELL` (requires TTY) |
| `slipfork logs [--world <fork>] [<target>] [--follow] [--stderr] [--lines <n>]` | M2 | Read or follow retained component/hook logs from `slipforkd` |
| `slipfork hook run <hook_id> [--world <fork>] [--no-tty]` | M2 | Re-run `<component>:<phase>` hook (direct native TTY `pdb` by default; `--no-tty` via daemon) |
| `slipfork db [<db_id>] [--world <fork>]` | M2 | Interactive `psql` **inside the running `postgres:<v>` service container** (`crun exec` entry command from `service_entry_command`; requires TTY; fails closed `ServiceNotRunning`) |
| `slipfork redis [<cluster_id>] [--world <fork>]` | M2 | Interactive `redis-cli` **inside the running `redis:<v>` service container** (`crun exec` entry command; requires TTY; fails closed `ServiceNotRunning`) |
| `slipfork forward [--world <fork>] [--json] [<target>...]` | M2 | Ask `slipforkd` to bind `127.0.0.1:0` bridge(s) for host GUIs (held open until disconnect) |
| `slipfork share [--stop] [--world <fork>]` | M2 (`M2.22`) | Re-materialize with/without `weaved` origin (`share` fails closed until `M2.22`; `--stop` no-ops when local) |
| `slipfork secret set [--force] <KEY>` / `unset <KEY>` / `list` | M2 | Manage `0600` vault secrets via `slipforkd` (`stdin` input; `--force` rotates existing keys) |
| `slipfork auth login [- \| '<url>#handle=<64-hex>']` | M2 | Pair `tpmd`/P-256 device key over `ospp-v1` (`-` reads URL from `stdin`) |
| `slipfork daemon unit [--only socket\|service] [--forward-port <u16>]` | M2 | Render non-template `slipforkd.socket` and/or `slipforkd.service` (`User=<developer-user>`) |
| `slipfork doctor` / `snow resolve` / `dag plan` | M1 (shipped) | Verify host capabilities, inspect resolved `snow.yml` envs, and plan DAG waves |
| `slipfork support [--json]` | M2 | Report verified integration tiers & `required_evidence` (`support/integrations.toml`) |
| `slipfork test [-j N] [--host-caches] [-- <runner...>]` | M3 | Materialize hermetic `TestSlot` DAG over the daemon-materialized `L0` rootfs, run parallel tests, collect typed coverage |

---

## 2. World-Scoped `[resources]` `ResourceStrategy` & Milestone 3 Testing (`slipfork.toml`)

Zero `[resources]` lines are needed for the standard workflow (`local` worlds default to `"prefer_remote"`, `slipfork test` `TestSlot` worlds default hermetically to `"local"` via `ResourcePlan::resolve`). Declare `[resources]` **only** when overriding a default:

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
run = "uv run python manage.py loaddata catalog" # Origin-independent -> reusable CoW snapshot

[fixtures.seed_sites]
parent = "seed_catalog"
per_world = true                                  # Writes BASE_URL -> runs per world after WorldOrigin assignment
inputs = ["api/fixtures/configure_site.py"]
run = "uv run python api/fixtures/configure_site.py"

[test]
coverage = true
coverage_report = ["html", "xml"]
coverage_fail_under = 85
```

- **Hermetic-by-Construction `TestSlot` Worlds:** `ResourceMode::Test` derives from `WorldTarget::TestSlot` (`ADR-0008` §3), so pre-warmed M3 test worlds never touch shared dev S3 or dev DB dumps unless `test = "remote"` / `"prefer_remote"` is set on that resource.
- **Blastit, Playwright Traces & Allure (`SLIPFORK_OUTPUT_DIR`):** Each test world mounts surviving `.slipfork/runs/<run_id>/<slot>/output` as `SLIPFORK_OUTPUT_DIR` (`ADR-0006` §5), keeping `slipfork.toml` 100% free of shell pipelines.
