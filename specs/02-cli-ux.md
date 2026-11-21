# 02 — The CLI UX

Slipfork's CLI is designed around four rules: **high signal, clear visual hierarchy, real ANSI colors on interactive terminals, and zero noise.**

**Color convention across every command:**
- **Green:** `●` active world dot, `✓` completed steps, `running` state
- **Bold cyan:** clickable `http://*.localhost` URLs
- **Red:** `✗` failed steps, `failed` / `crashed` state
- **Yellow:** `!` warnings and dirty git checkout notices
- **Dim:** filesystem paths (`~/projects/demo`), ages (`12m`, `4s`), `(up to date)` badges, and dividers

---

## 1. `slipfork` (Status Tree) & `slipfork ps`

Running bare `slipfork` (alias `slipfork status`) displays every active world on your machine as one clean tree with human service names, real ports and routes, human ages, and nested forks:

```text
● demo  http://demo.localhost  ~/projects/demo
  ├─ api      running      :8000 -> /api          12m
  ├─ worker   running      -                      12m
  ├─ front    running      :5173 -> /             12m
  ├─ mailpit  running      :8025 -> mail.*/       12m
  ├─ pg       postgres 18  :5432                  12m
  ├─ cache    redis 7      :6379                  12m
  ├─ uploads  s3 (private) :8333                  12m
  ├─ assets   s3 + cdn     -> static_cdn.*        12m
  └─ forks
     └─ ● feat-auth  http://feat-auth--demo.localhost  ~/projects/demo/.forks/feat-auth
        ├─ api      running      :8000 -> /api          4m
        ├─ worker   running      -                      4m
        ├─ front    running      :5173 -> /             4m
        ├─ mailpit  running      :8025 -> mail.*/       4m
        ├─ pg       postgres 18  :5432                  4m
        ├─ cache    redis 7      :6379                  4m
        ├─ uploads  s3 (private) :8333                  4m
        └─ assets   s3 + cdn     -> static_cdn.*        4m
```

- Column widths size dynamically to fit long service and fork names without shearing.
- When nothing is running, `slipfork` prints a dim `○ No worlds running` and exits `0`.
- `slipfork ps` renders the exact same tree scoped strictly to the current directory's world or fork.
- `status` and `ps` accept `--json` (zero ANSI) and `-v` adds the supervisor pid lineage.

---

## 2. `slipfork up`, `restart` & `down`

`slipfork up` drives animated spinners across the boot DAG — images, backing resources, `@<tool>` / `before-start` setup nodes, `pre-deploy` hooks, and services — replacing each line cleanly with a green `✓` and timing as it finishes:

```text
✓ Image backend (cached 0.1s)
✓ Image frontend (built 1.4s)
✓ Resources pg, cache, uploads, assets ready (0.4s)
✓ Setup backend: uv sync (1.1s)
✓ Setup frontend: pnpm install (up to date)
✓ Hook api:pre-deploy (0.6s)
✓ Services api, worker, front, mailpit running (0.2s)
✓ Ready -> http://demo.localhost
```

On subsequent `slipfork up`, `slipfork restart`, or `slipfork fork` runs where watched lockfiles and `when changed(...)` inputs are unchanged, setup nodes skip in `< 1ms` (`(up to date)`).

If a setup step, hook, or service fails, Slipfork prints the failure in red with inline logs and **leaves the world running** so you can inspect it right away:

```text
✗ Hook api:pre-deploy failed (exit 1) — Forkfile:32
  │ sqlalchemy.exc.ProgrammingError: column "org_id" does not exist
  next: slipfork logs api  or  slipfork shell api
```

- **`slipfork restart [service]`** re-checks `@<tool>` and `before-start when changed(...)` inputs, re-runs affected hooks, and restarts one or all services in place.
- **`slipfork down`** stops the world's containers cleanly while keeping its Btrfs data subvolumes intact.
- **Live child progress:** Any child step that can exceed `200 ms` — image `pull`, `RUN`, and `down`/`restart` teardown — is `spawn()`ed and streams `[step/total]` phase headers with elapsed time; nothing blocks on `.output()` or `pull -q`.

---

## 3. `slipfork fork [name]` & `slipfork drop <name>`

`slipfork fork` takes an atomic Btrfs snapshot of **both** your project subvolume (including `.venv`, `node_modules`, and `.slipfork/` input-hash stamps) and your world's database/cache/S3 subvolumes in `< 50ms`, then boots the fork behind its own `.localhost` URL:

```text
✓ Forked demo -> feat-auth in 38ms
  path  ~/projects/demo/.forks/feat-auth
  url   http://feat-auth--demo.localhost
```

`slipfork drop feat-auth` stops the fork and deletes its Btrfs snapshots instantly, refusing with a yellow warning if `.forks/feat-auth` has uncommitted or untracked git changes unless `-f` / `--force` is passed.

---

## 4. Daily Developer Commands

`slipfork logs -f` streams merged `stdout` + `stderr` with distinct colored service gutters and stays attached across restarts:

```text
api    │ INFO:     Uvicorn running on http://0.0.0.0:8000
worker │ [2026-11-21 14:21:04] celery@demo ready.
       ── restarted api (14:22:09) ──
api    │ INFO:     Application startup complete.
```

`slipfork doctor` leads with a clear verdict followed by the **only 3 host requirements** (strictly Btrfs, unprivileged namespaces, and `crun` + `:80` ingress — zero Docker required anywhere):

```text
✓ Ready
  ✓ Btrfs       /home is btrfs (subvolume snapshots enabled)
  ✓ Namespaces  unprivileged user + netns available
  ✓ Runtime     crun 1.20, :80 .localhost ingress active
```

When a requirement fails, the verdict counts rows and every failing row carries its own remedy; AppArmor is named inside the namespace remedy rather than adding a fourth row:

```text
✗ 2 issue(s)
  ✓ Btrfs       /home is btrfs (subvolume snapshots enabled)
  ✗ Namespaces  unprivileged user + netns unavailable
     next: sudo sysctl -w kernel.unprivileged_userns_clone=1
  ✗ Runtime     crun is not on PATH
     next: install crun (for example `sudo apt-get install crun`) and retry
```

The three rows are always `Btrfs`, `Namespaces`, and `Runtime`, and `--json` emits `{ready, checks:[{name, ok, detail, next?}]}` with zero ANSI.

| Command | Behavior |
| :--- | :--- |
| `slipfork logs [-f] [service]` | Colored per-service gutter, chronological merge by capture timestamp, stays attached across `restart`; `-f` exits `0` on Ctrl-C. `--json` emits one `{"pod","stream","line","kind"}` object per line and a final `{"exit":0}`. |
| `slipfork shell [service\|image]` | Interactive shell inside the service or image container with its exact user, `workdir`, and runtime `env` (defaults automatically when unambiguous). `shell` allocates **no TTY**: it pipes stdio to `sh -i`, so prompts and job control are limited. |
| `slipfork exec [service\|image] -- <cmd>` | Runs any ad-hoc command (`pytest`, `pnpm test`, scripts) inside the target container, forwards stdin, and forwards the command's exit code. |
| `slipfork db [database]` | Opens interactive `psql` (or executes piped SQL when `stdin` is redirected). |
| `slipfork redis [id]` | Opens interactive `redis-cli` against the world's Redis instance. |
| `slipfork env [service]` | Prints resolved runtime variables; secrets show `[REDACTED]` unless `--reveal` is passed on a TTY. |
| `slipfork secret list\|set\|unset` | Lists or overrides local offline secrets for the current world. |
| `slipfork open [service]` | Opens the world or route URL in your browser (headless/SSH-aware, zero `sudo` env-stripping bugs). |
| `slipfork auth [<endpoint-url>]` | Pairs your machine with a remote `OSPP v1` endpoint; precedence is `--ospp-endpoint` > positional URL > `<state_dir>/ospp.json`. |
| `slipfork daemon install` | Writes the per-user daemon unit; no `sudo`, and worlds stay in the developer's session. |
| `slipfork daemon install --ingress` | Adds the system `:80` socket and AppArmor profile; the only daemon verb that needs `sudo`. |
| `slipfork migrate` | Converts a legacy `snow.yml` manifest into a formatted `# forkfile: 0.1` `Forkfile`. |

After pairing, `slipfork auth` prints the settled pair and one `next: slipfork up` line; with no endpoint and no saved record it fails with `this daemon has no OSPP endpoint` and names the flag to pass.

`shell`, `exec`, `db`, and `redis` auto-select their target when exactly one candidate is running. When several match, they fail with one `error: N candidates: <ids>` line and a `next:` naming the invoked verb and an id to pass. `logs` without a service merges every pod in capture-timestamp order, with a 250 ms reorder window while following.

---

## 5. Strict Terminal Hygiene

- **TTY-aware styling:** ANSI colors and spinners run **only** when the output stream is a real TTY, `NO_COLOR` is unset, and `TERM != dumb`.
- **Clean scrollback:** Active spinner frames clear completely before every status or log line — zero raw carriage-return or `ESC[2K` line-clear garbage in scrollback or redirected files.
- **Machine parity (`--json`):** Every inspection and lifecycle command supports `--json`, emitting the exact same fields as structured JSON with zero ANSI escapes.
- **Actionable errors:** Every error prints **one clear sentence** followed by `next: <exact command to fix it>`.
