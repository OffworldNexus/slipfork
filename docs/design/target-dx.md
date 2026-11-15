# Slipfork M2–M4 Target Developer Experience Specification (`docs/design/target-dx.md`)

- **Status:** Proposed Target DX Specification (`ADR-0001`..`ADR-0007` locked; `ADR-0008` pending `H-010`)
- **Visibility:** Internal design spec (`exclude_docs` in `zensical.toml`). Per **Core Rule #4 (Zero Vaporware in Published Docs)**, sections here promote into published `docs/` and `README.md` **only** as each binary capability ships and passes conformance tests (`M2.19`).

---

## 1. Milestone 2 Target DX: Single-Origin Worlds, Unprivileged `slipforkd`, `OSPP v1` & Rescue Tooling

### 1.1 Install & Two Startup Modes (Systemd `:80` Socket Activation vs Unprivileged `--listen`)

Installing the workspace places `slipfork`, `slipforkd`, and `git-remote-slipfork` onto `PATH` (source install today; prebuilt binaries in M4):

```console
$ cargo install --git https://github.com/OffworldNexus/slipfork slipfork

# Workstation one-time port-80 setup (PID 1 systemd binds :80 + :7080; slipforkd runs as $USER):
$ slipfork daemon unit --only socket  | sudo tee /etc/systemd/system/slipforkd.socket > /dev/null
$ slipfork daemon unit --only service | sudo tee /etc/systemd/system/slipforkd.service > /dev/null
$ sudo systemctl daemon-reload && sudo systemctl enable --now slipforkd.socket

# Container / CI / no-systemd environments (zero root needed; port handling pending H-010 #2):
$ slipforkd --listen 127.0.0.1:7080 --forward-port 7081 &
```

`slipforkd.service` runs **unprivileged as `$USER` with an empty `CapabilityBoundingSet=`** and explicit `ReadWritePaths=` for state/runtime sockets (`--forward-port <u16>` overrides the default `7080` forward port on collision; `ADR-0008` §5). If `slipforkd` is not running, `slipfork up` fails closed with one line naming both startup commands above. **OSPP is never required to boot local worlds:** offline laptops work out of the box from `snow.yml` + `~/.local/share/slipfork/vaults/<project>.json` (`0600`; `slipfork.toml` is optional).

### 1.2 `slipfork up`, Diagnostic `[resources]` Lines & Seed Dump Flags (`--no-dump | --refresh-dump`)

`up` resolves the world via `--world <fork>` or `cwd`'s nearest ancestor (`<vcs_root>/.worktrees/<fork>[/...]` -> fork `<fork>`, repo root -> `canonical`), waits for declared component `internal_ports` readiness before printing `[origin]`, and is idempotent when re-run (`down` stops services immediately without waiting for readiness):

```console
$ slipfork up                           # Paired laptop: uses OSPP dev S3 + cached zstd seed dump
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
- **Single Canonical Origin & Host-Port Rule:** Every world has **at most one public origin** (`origin.url()`, `ADR-0005` §1; e.g. `http://feat-billing.acme.localhost` for `.worktrees/feat-billing`, with `<domain>.(<fork>.)?<project>.localhost` routed as trusted local ingresses). Services run inside isolated network namespaces — **never connect to host `127.0.0.1:5432` or `:8000`**; use the printed `origin`, `slipfork exec`, or `slipfork forward --json`.

### 1.3 First-Class Hook Failure Rescue UX (`hook run`, `logs`, `shell`, `exec`)

Hooks (`<component>:<phase>`, e.g. `api:pre-deploy`) stream stdout/stderr live. On non-zero exit, Slipfork prints the **full un-truncated output**, **preserves the failed world** (listed in `slipfork list`), and prints three copy-pasteable rescue commands using the canonical `--world <fork>` selector:

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

- **Single Env Owner (`slipfork exec`) & Human PTY vs Agent Non-TTY:** `slipfork exec` is the **sole owner** of in-world `snow.yml` (`modes.local`) environment resolution and injection (`$DATABASE_URL`, `$REDIS_URL`, `secrets::*` are always injected by `slipfork exec`; `@slipfork/opencode` only routes `shell` calls to `slipfork exec --cwd <cwd>`, `ADR-0004`). `hook run`, `shell`, `db`, and `redis` attach a real PTY (`pdb`/REPLs); without a TTY, they fail closed naming `hook run --no-tty` or `slipfork exec --world <fork> -- <argv>...`.

### 1.4 Service Clients (`db`, `redis`, `forward`), Secrets, VCS (`git` + `jj`) & `OSPP v1` Pairing

```console
$ slipfork exec --world feat-billing -- uv run pytest -k billing   # Non-interactive agent/CI execution (injects modes.local env)
$ slipfork db --world feat-billing                                 # Interactive psql inside world
$ slipfork redis --world feat-billing                              # Interactive redis-cli inside world
$ slipfork forward --world feat-billing --json                     # Foreground host-GUI TCP bridge (127.0.0.1:0):
{"world":"feat-billing","targets":[{"id":"pg_databases.main","host_addr":"127.0.0.1:43182","uri":"postgres://127.0.0.1:43182/main"}]}

$ git pull slipfork feat-billing                                   # Committed branch (@- in Jujutsu)
$ git fetch slipfork feat-billing/wip                              # Dirty + untracked snapshot (@ in Jujutsu)

$ printf '%s' "$STRIPE_KEY" | slipfork secret set STRIPE_SECRET_KEY          # --force to rotate existing key
$ printf '%s\n' "$PAIR_URL_WITH_FRAGMENT" | slipfork auth login -            # Stdin keeps #handle= out of argv
[auth] paired p256:02a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90 (backend=tpmd:sealed)
```

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
  - `slipfork list --json` -> `[{"world": "feat-billing", "state": "running|failed|stopped", "origin": "http://feat-billing.acme.localhost", "worktree": "/repo/.worktrees/feat-billing"}]` (**exactly one** `origin` URL per world).
  - `slipfork up --json` / `fork --json` -> `{"world": "...", "origin": "http://...", "resources": [{"id": "...", "strategy": "...", "resolved": "remote|local", "reason": "..."}]}`.
  - `slipfork support --json` -> `[{"id": "...", "category": "...", "tier": "...", "evidence": {...}, "required_evidence": [...]}]`.
- **Sharing (`slipfork share [--stop] [--world <fork>]`):** Toggling sharing tears down the live world and re-materializes from its `base`/fixture snapshot with the single new `WorldOrigin` (discarding uncommitted post-snapshot DB mutations while preserving worktree files; `share --stop` on an unshared world is an idempotent no-op). Until `tunnel-weaver` publishes `weaved`'s IPC specification (`M2.22`), `slipfork share` fails closed telling the caller to use the world's `.localhost` origin.

### 1.6 Complete M2–M3 CLI Command Table

Every world-scoped command selects a fork exclusively via `--world <fork>` (zero positional fork aliases; omitting `--world` resolves `cwd`'s nearest ancestor). M1's PoC `slipfork snap` command is superseded by `up`/`fork` and deleted in `M2.16` (with published docs updated in `M2.19`).

| Command | Milestone | Behavior |
|---|---|---|
| `slipfork up [--world <fork>] [--no-dump \| --refresh-dump] [--resources local] [--json]` | M2 | Boot world, await port readiness, print single `origin` |
| `slipfork down [--world <fork>]` | M2 | Stop world services without deleting its worktree (idempotent) |
| `slipfork fork <fork> [--no-dump \| --refresh-dump] [--resources local] [--json]` | M2 | Create `.worktrees/<fork>` (`git` or `jj`) + CoW world |
| `slipfork drop <fork>` | M2 | Destroy fork world and remove `.worktrees/<fork>` |
| `slipfork exec [--world <fork>] [--cwd <dir>] -- <argv>...` | M1 + M2 | Run command non-interactively inside world with `modes.local` env injected |
| `slipfork list [--json]` | M2 | List worlds with state, worktree path, and **exactly one** `origin` URL |
| `slipfork prune [--dry-run \| --yes] [--json]` | M2 | Preview or garbage-collect orphaned worlds and unreferenced snapshots |
| `slipfork shell [--world <fork>]` | M2 | Interactive PTY `$SHELL` inside world (requires TTY) |
| `slipfork logs [--world <fork>] [<target>] [--follow] [--stderr] [--lines <n>]` | M2 | Read or follow retained component/hook logs (lists candidates if unknown) |
| `slipfork hook run <hook_id> [--world <fork>] [--no-tty]` | M2 | Re-run `<component>:<phase>` hook (PTY `pdb` by default; `--no-tty` for agents) |
| `slipfork db [<db_id>] [--world <fork>]` | M2 | Interactive `psql` inside world (requires TTY; use `exec` for non-TTY SQL) |
| `slipfork redis [<cluster_id>] [--world <fork>]` | M2 | Interactive `redis-cli` inside world (requires TTY) |
| `slipfork forward [--world <fork>] [--json] [<target>...]` | M2 | Bind `127.0.0.1:0` foreground TCP bridge(s) via `dial_world_tcp` for host GUIs |
| `slipfork share [--stop] [--world <fork>]` | M2 (`M2.22`) | Re-materialize with/without `weaved` origin (`share` fails closed until `M2.22`; `--stop` no-ops when local) |
| `slipfork secret set [--force] <KEY>` / `unset <KEY>` / `list` | M2 | Manage `0600` vault secrets (`stdin` input; `--force` rotates existing keys) |
| `slipfork auth login [- \| '<url>#handle=<64-hex>']` | M2 | Pair `tpmd`/P-256 device key over `ospp-v1` (`-` reads URL from `stdin`) |
| `slipfork daemon unit [--only socket\|service] [--forward-port <u16>]` | M2 | Render unprivileged `slipforkd.socket` and/or `slipforkd.service` to `stdout` |
| `slipfork doctor` / `snow resolve` / `dag plan` | M1 (shipped) | Verify host capabilities, inspect resolved `snow.yml` envs, and plan DAG waves |
| `slipfork support [--json]` | M2 | Report verified integration tiers & `required_evidence` (`support/integrations.toml`) |
| `slipfork test [-j N] [-- <runner...>]` | M3 | Materialize hermetic `TestSlot` DAG, run parallel tests, collect typed coverage |

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
