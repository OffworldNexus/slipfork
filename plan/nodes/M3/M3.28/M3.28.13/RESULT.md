# M3.28.13 — live re-verification of the `0.1.0` UX pass on the Btrfs DO VM

**STATUS: BLOCKED.** Step 6 (`slipfork restart` applies an edited app response) fails on this
build: `restart` stops the `sh -eu -c` wrapper but not the component process under it, the new
component dies with `EADDRINUSE`, and the old response keeps being served. Transcript and root
cause below (§6). Every other check in the task's step list passed; nothing is claimed that was
not run, and the inputs this environment cannot supply (an OSSP server, the owner's laptop) are
named as unexercised.

## Environment

- Droplet `slipfork-dev-vm`, id `620073485`, region `ams3`, ip `165.232.86.174`, 100 GiB Btrfs
  volume mounted at `/var/lib/slipfork` (found already up; reused, never recreated).
- Repository base `9a79afc` on `release/0.1.0`.
- Date of the run: 2026-11-18 (UTC). Identity: `~/.cache/slipfork/do-vm/id_ed25519`.
- Every command ran from the repository root with
  `export PATH="$HOME/.local/bin:$HOME/.cargo/bin:$HOME/.bun/bin:$PATH"`.
- Remote verbs run as the demo account through
  `tools/do_vm.py ssh -- /usr/sbin/runuser -u slipfork -- bash -lc '…'`, so `HOME=/home/slipfork`
  and `~/.cache` resolves to the Btrfs volume (`/var/lib/slipfork/home/.cache`) — the same state
  root the installed `slipforkd` serves.
- The two TTY-only verbs (`slipfork db`, `slipfork shell`) were driven with this exact envelope
  (the harness's own identity and `known_hosts` file), because `tools/do_vm.py ssh -- <cmd>`
  forwards without a pseudo-terminal (it only allocates one for its interactive form):

  ```console
  ssh -F /dev/null -o IdentitiesOnly=yes -o IdentityAgent=none -o ControlMaster=no \
      -o ControlPath=none -o StrictHostKeyChecking=accept-new \
      -o UserKnownHostsFile=.slipfork/do-vm/known_hosts-620073485 \
      -i ~/.cache/slipfork/do-vm/id_ed25519 -tt root@165.232.86.174 "<remote command>"
  ```

  §9 and §11 abbreviate that as `ssh -tt … root@165.232.86.174`; the full form is the one above.
- Raw transcripts live next to this file in `evidence/`; each block below is the file's content.

## 1. Install the current build (`sync`, OSPP v1 cut-over included)

```console
$ uv run --no-project tools/do_vm.py sync
demo     unchanged /home/slipfork/demo (seeded once; owner edits are kept)
unchanged slipforkd.socket, slipforkd.service, /etc/apparmor.d/slipfork; enabled
synced 607 file(s) to /home/slipfork/slipfork on 165.232.86.174
EXIT=0
```

The OSPP v1 cut-over is visible in the installed artefacts: no `tpmd` anywhere in the units or the
AppArmor profile, no `dag`/`share` mention in the units, and no `slipfork-tpmd` binary on `PATH`:

```console
$ uv run --no-project tools/do_vm.py ssh -- bash -lc 'bash /tmp/opencode-m32813-units.sh; echo "SCRIPT_EXIT=$?"'
$ systemctl cat slipforkd.service slipforkd.socket | grep -i tpmd
UNIT_TPMD_GREP_EXIT=1
$ systemctl cat slipforkd.service slipforkd.socket | grep -iE "dag|share"
UNIT_DAG_SHARE_GREP_EXIT=1
$ command -v slipfork-tpmd
no slipfork-tpmd on PATH
$ grep -c tpmd /etc/apparmor.d/slipfork
0
APPARMOR_TPMD_GREP_EXIT=1
EXIT=0
SCRIPT_EXIT=0
```

`sync` also re-runs `slipfork daemon install`, which restarted the units; per its contract every
running world's processes stop until the next `slipfork up`. That is why the first port-less curl
below returns `404` and the world has to be booted first with `slipfork up` (§6) — recorded rather
than hidden.

## 2. Socket activation

```console
$ uv run --no-project tools/do_vm.py ssh -- systemctl is-active slipforkd.socket
active
EXIT=0
```

## 3. Version

```console
$ uv run --no-project tools/do_vm.py ssh -- slipfork --version
slipfork 0.1.0
EXIT=0
```

## 4. The primary proof — port-less origin, `:80`, no port anywhere in the URL

Before any `up` (the daemon has no world yet):

```console
$ uv run --no-project tools/do_vm.py ssh -- curl -fsS -o /dev/null -w '%{http_code}\n' -H 'Host: demo.localhost' http://127.0.0.1/
curl: (22) The requested URL returned error: 404
404
EXIT=22
```

After `slipfork up` (§6 baseline):

```console
$ uv run --no-project tools/do_vm.py ssh -- curl -fsS -o /dev/null -w '%{http_code}\n' -H 'Host: demo.localhost' http://127.0.0.1/
200
EXIT=0
```

The URL is `http://127.0.0.1/` — default `:80`, no port token. The body through the same port-less
origin:

```console
$ uv run --no-project tools/do_vm.py ssh -- curl -fsS -H 'Host: demo.localhost' http://127.0.0.1/
{"debug": true, "hello": "world", "host": "slipfork-dev-vm", "secret_len": 32, "world": "demo"}
EXIT=0
```

`secret_len: 32` is the `modes.local` auto-generated `DEMO_SECRET !!` from the local vault, so the
`:80` path carries the resolved world environment, not a stub.

## 5. Bare `slipfork`, help without `dag`/`share`, and both deleted verbs

Status dashboard (bare `slipfork`):

```console
$ uv run --no-project tools/do_vm.py ssh -- /usr/sbin/runuser -u slipfork -- bash -lc 'cd ~/demo && slipfork; echo "EXIT=$?"'
slipforkd 62455, ingress 127.0.0.1:80, [::1]:80
  demo             running   http://demo.localhost                    /home/slipfork/demo
                  lineage   rootfs 01a4b324b7cb rebuilt 7.4s · postgres:18 cached · seed - - · base 4570132bffb8 reused
                  next      slipfork open | slipfork db | slipfork logs | slipfork restart | slipfork down
EXIT=0
```

`--help` lists every command and neither `dag` nor `share`:

```console
$ uv run --no-project tools/do_vm.py ssh -- /usr/sbin/runuser -u slipfork -- bash -lc 'cd ~/demo && slipfork --help; echo "EXIT=$?"'
Instant CoW dev worlds & parallel test DAG orchestrator

Usage: slipfork [OPTIONS] [COMMAND]

Commands:
  status   Show the daemon ingress plus every world's tree, lineage, and next steps
  doctor   Probe the Btrfs substrate, the live subvolume ioctl round-trip, and unprivileged namespaces
  snow     Validate and resolve `snow.yml` modes & `!!` auto-generated secrets
  up       Boot the world selected by `--world` (or `cwd`) through `slipforkd`
  down     Stop a world's services while keeping its worktree (idempotent)
  restart  Re-run a held world in place: stop its components + services, then run the same boot pipeline as `up` (`0.1.0` has no `--watch`; re-run this command after an edit)
  ps       Show one world's state, lineage, and every supervised component / `crun` service row
  open     Open the selected world's origin in the desktop browser, or print the SSH recipe
  fork     Create `.worktrees/<fork>` plus its CoW world through `slipforkd`
  drop     Destroy a fork world and remove its `.worktrees/<fork>` checkout
  list     List worlds with state, worktree path, and exactly one origin URL
  prune    Preview (default) or reap worlds whose checkout directory is gone
  secret   Manage the project's local `0600` vault secrets through `slipforkd`
  daemon   Daemon administration
  exec     Run a command non-interactively inside a world (daemon-resolved environment)
  shell    Enter a world interactively with native terminal stdio (requires a TTY)
  logs     Read or follow retained component and hook logs from `slipforkd`
  hook     Hook administration
  db       Open `psql` inside the world's running PostgreSQL service container (requires a TTY)
  redis    Open `redis-cli` inside the world's running Redis service container (requires a TTY)
  forward  Bind daemon-owned `127.0.0.1:0` listeners bridged into a world (held until Ctrl-C)
  auth     Pair this machine with the daemon's OSPP endpoint
  support  Report verified integration tiers and their required evidence (`ADR-0007`)
  help     Print this message or the help of the given subcommand(s)

Options:
      --json
          Emit machine-readable JSON on stdout

  -v, --verbose
          Add per-node lineage and `[resources]` detail to human output (ignored under `--json`)

      --state-dir <STATE_DIR>
          Explicit CoW state directory (defaults to `$XDG_CACHE_HOME/slipfork` or `~/.cache/slipfork`).
          
          The client's control socket is always `<state_dir>/run/daemon.sock`; there is no `--runtime-dir` override outside `slipforkd` itself (`ADR-0011` §2).

  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
EXIT=0
```

```console
$ grep -cE '^  (dag|share) ' evidence/05b-help.txt
0
GREP_EXIT=1
```

The deleted verbs each exit `2`:

```console
$ uv run --no-project tools/do_vm.py ssh -- /usr/sbin/runuser -u slipfork -- bash -lc 'cd ~/demo && slipfork dag plan; echo "EXIT=$?"'
error: unrecognized subcommand 'dag'

Usage: slipfork [OPTIONS] [COMMAND]

For more information, try '--help'.
EXIT=2
```

```console
$ uv run --no-project tools/do_vm.py ssh -- /usr/sbin/runuser -u slipfork -- bash -lc 'cd ~/demo && slipfork share; echo "EXIT=$?"'
EXIT=2
error: unrecognized subcommand 'share'

  tip: a similar subcommand exists: 'secret'

Usage: slipfork [OPTIONS] [COMMAND]

For more information, try '--help'.
```

(The `share` transcript's two streams interleave in capture order; the exit status is `2`.)

## 6. FAIL — `restart` does not apply an edited app response

First boot of this run (the `up` block: one `lineage` line, real digests/cache facts, no
`[resources]` jargon):

```console
$ uv run --no-project tools/do_vm.py ssh -- /usr/sbin/runuser -u slipfork -- bash -lc 'cd ~/demo && slipfork up; echo "EXIT=$?"'
[ready]     api (internal :8000 in world netns)
world     demo (running, changed)
app       http://demo.localhost
worktree  /home/slipfork/demo
next      slipfork open | slipfork db | slipfork logs | slipfork restart | slipfork down
lineage   rootfs 01a4b324b7cb rebuilt 7.4s · postgres:18 cached · seed - - · base 4570132bffb8 reused
EXIT=0
```

Served body before the edit:

```console
$ uv run --no-project tools/do_vm.py ssh -- curl -fsS -H 'Host: demo.localhost' http://127.0.0.1/
{"debug": true, "hello": "world", "host": "slipfork-dev-vm", "secret_len": 32, "world": "demo"}
EXIT=0
```

First attempt: edit `"hello": "world"` to `"hello": "world-M3.28.13"`, then `restart`:

```console
$ uv run --no-project tools/do_vm.py ssh -- /usr/sbin/runuser -u slipfork -- bash -lc 'cd ~/demo && sed -i "s/\"hello\": \"world\"/\"hello\": \"world-M3.28.13\"/" app/server.py && grep -n "hello" app/server.py && slipfork restart; echo "EXIT=$?"'
7:  GET /    -> hello + host + world + debug + secret_len (never the secret itself)
76:                    "hello": "world-M3.28.13",
[ready]     api (internal :8000 in world netns)
[restarted] api, pg_clusters.psql
world     demo (running, changed)
app       http://demo.localhost
worktree  /home/slipfork/demo
next      slipfork open | slipfork db | slipfork logs | slipfork restart | slipfork down
lineage   rootfs 01a4b324b7cb rebuilt 7.4s · postgres:18 cached · seed - - · base 4570132bffb8 reused
EXIT=0
```

The body after `restart` is **unchanged** — the old response is still served:

```console
$ uv run --no-project tools/do_vm.py ssh -- curl -fsS -H 'Host: demo.localhost' http://127.0.0.1/
{"debug": true, "hello": "world", "host": "slipfork-dev-vm", "secret_len": 32, "world": "demo"}
EXIT=0
```

That first world had been through the `sync` handover, so the whole sequence was repeated from a
clean `down` + `up`. The fresh boot is healthy — the component row is `running` with a pid, and the
process tree shows the real service one level below its supervisor:

```console
$ uv run --no-project tools/do_vm.py ssh -- /usr/sbin/runuser -u slipfork -- bash -lc 'cd ~/demo && slipfork down >/dev/null && slipfork up >/dev/null && sleep 1 && slipfork ps && ps -eo pid,ppid,stat,cmd | grep -E "nsenter|server.py" | grep -v grep; echo "EXIT=$?"'
state     running
lineage   rootfs 71e00bc47fe4 cached · postgres:18 cached · seed - - · base 4570132bffb8 reused
KIND             ID                STATE    PID    PORT  UPTIME
component        api               running  64538  8000  1s
backing_service  pg_clusters.psql  running  64508  5432  1s
  64508   62455 S    nsenter --target=64474 --user --preserve-credentials --mount --net --ipc --pid --wdns=/ -- /.slipfork/bin/crun --cgroup-manager=disabled --root /.slipfork/run run -b /.slipfork/oci/sf8-pg_clusters_psql sf8-pg_clusters_psql
  64534   62455 S    nsenter --target=64474 --user --preserve-credentials --mount --net --ipc --pid --wdns=/home/slipfork/demo -- sh -eu -c python3 /app/server.py
  64538   64534 S    sh -eu -c python3 /app/server.py
  64541   64538 S    python3 /app/server.py
EXIT=0
```

Baseline, edit to `-r2`, `restart`, and the body after it:

```console
$ uv run --no-project tools/do_vm.py ssh -- curl -fsS -H 'Host: demo.localhost' http://127.0.0.1/
{"debug": true, "hello": "world-M3.28.13", "host": "slipfork-dev-vm", "secret_len": 32, "world": "demo"}
EXIT=0

$ uv run --no-project tools/do_vm.py ssh -- /usr/sbin/runuser -u slipfork -- bash -lc 'cd ~/demo && sed -i "s/\"hello\": \"world-M3.28.13\"/\"hello\": \"world-M3.28.13-r2\"/" app/server.py && grep -n "hello.*world-M3" app/server.py && slipfork restart; echo "RESTART_EXIT=$?"'
76:                    "hello": "world-M3.28.13-r2",
[ready]     api (internal :8000 in world netns)
[restarted] api, pg_clusters.psql
world     demo (running, changed)
app       http://demo.localhost
worktree  /home/slipfork/demo
next      slipfork open | slipfork db | slipfork logs | slipfork restart | slipfork down
lineage   rootfs 71e00bc47fe4 cached · postgres:18 cached · seed - - · base 4570132bffb8 reused
RESTART_EXIT=0

$ uv run --no-project tools/do_vm.py ssh -- curl -fsS -H 'Host: demo.localhost' http://127.0.0.1/
{"debug": true, "hello": "world-M3.28.13", "host": "slipfork-dev-vm", "secret_len": 32, "world": "demo"}
EXIT=0
```

`ps` reports the restarted component as `exited`, and the previous Python process (pid 64541) is
still alive and still bound to `:8000` inside the world netns; its parent is now the world's PID 1
(`slipfork-world-init`, pid 64474) because its own parent died:

```console
$ uv run --no-project tools/do_vm.py ssh -- /usr/sbin/runuser -u slipfork -- bash -lc 'cd ~/demo && slipfork ps; ps -eo pid,ppid,stat,cmd | grep -E "nsenter|server.py" | grep -v grep; echo "EXIT=$?"'
state     running
lineage   rootfs 71e00bc47fe4 cached · postgres:18 cached · seed - - · base 4570132bffb8 reused
KIND             ID                STATE    PID    PORT  UPTIME
component        api               exited   -      -     -
backing_service  pg_clusters.psql  running  64754  5432  3s
  64541   64474 S    python3 /app/server.py
  64754   62455 S    nsenter --target=64474 --user --preserve-credentials --mount --net --ipc --pid --wdns=/ -- /.slipfork/bin/crun --cgroup-manager=disabled --root /.slipfork/run run -b /.slipfork/oci/sf8-pg_clusters_psql sf8-pg_clusters_psql
EXIT=0
```

The new component's stderr names the collision:

```console
$ uv run --no-project tools/do_vm.py ssh -- /usr/sbin/runuser -u slipfork -- bash -lc 'cd ~/demo && slipfork logs --stderr; echo "EXIT=$?"'
pg_clusters.psql: 2026-11-18 04:09:34.538 UTC [12] LOG:  checkpoint starting: end-of-recovery immediate wait
pg_clusters.psql: 2026-11-18 04:09:34.648 UTC [16] FATAL:  the database system is not yet accepting connections
pg_clusters.psql: 2026-11-18 04:09:34.648 UTC [16] DETAIL:  Consistent recovery state has not been yet reached.
pg_clusters.psql: 2026-11-18 04:09:34.848 UTC [12] LOG:  checkpoint complete: wrote 56 buffers (0.3%), wrote 3 SLRU buffers; 0 WAL file(s) added, 0 removed, 0 recycled; write=0.005 s, sync=0.266 s, total=0.320 s; sync files=45, longest=0.014 s, average=0.006 s; distance=248 kB, estimate=248 kB; lsn=0/1C05050, redo lsn=0/1C05050
pg_clusters.psql: 2026-11-18 04:09:34.859 UTC [1] LOG:  database system is ready to accept connections
api: Traceback (most recent call last):
api:   File "/app/server.py", line 106, in <module>
api:     main()
api:   File "/app/server.py", line 100, in main
api:     server = ThreadingHTTPServer(("0.0.0.0", PORT), Handler)
api:              ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
api:   File "/usr/local/lib/python3.12/socketserver.py", line 457, in __init__
api:     self.server_bind()
api:   File "/usr/local/lib/python3.12/http/server.py", line 140, in server_bind
api:     socketserver.TCPServer.server_bind(self)
api:   File "/usr/local/lib/python3.12/socketserver.py", line 478, in server_bind
api:     self.socket.bind(self.server_address)
api: OSError: [Errno 98] Address already in use
EXIT=0
```

### Root cause (a stop one level too shallow)

- The component is spawned as `nsenter … -- sh -eu -c python3 /app/server.py`
  (`crates/slipfork-engine/src/btrfs_engine.rs::spawn_world_process`), so the real service is the
  **grandchild** `python3`, not the first child.
- `wait_for_inner_pid` (`crates/slipfork-engine/src/service.rs`) captures the **first** host child —
  here `sh` pid 64538 — and `stop_service_process` signals only that captured identity. `sh` exits
  without forwarding the signal; `python3` is reparented to the world's PID 1 and keeps `:8000`
  bound (pid 64541 above).
- The supervisor (`nsenter`) exits with its child, so the watcher flips the row to `Exited`; the next
  `stop_service_process` returns early at the `state != Running` guard and never reaps the orphan.
- `down` hides the defect: it reaps `slipfork-world-init`, and `unshare --kill-child=SIGKILL` kills
  every process left in the PID namespace. `down` + `up` therefore applies the edit correctly:

```console
$ uv run --no-project tools/do_vm.py ssh -- /usr/sbin/runuser -u slipfork -- bash -lc 'cd ~/demo && slipfork down && slipfork up; echo "EXIT=$?"'
[down] world stopped (worktree kept)
[ready]     api (internal :8000 in world netns)
world     demo (running, changed)
app       http://demo.localhost
worktree  /home/slipfork/demo
next      slipfork open | slipfork db | slipfork logs | slipfork restart | slipfork down
lineage   rootfs 479f73c1a7b0 rebuilt 10.3s · postgres:18 cached · seed - - · base 4570132bffb8 reused
EXIT=0

$ uv run --no-project tools/do_vm.py ssh -- curl -fsS -H 'Host: demo.localhost' http://127.0.0.1/
{"debug": true, "hello": "world-M3.28.13-r2", "host": "slipfork-dev-vm", "secret_len": 32, "world": "demo"}
EXIT=0
```

### A second gap in the same step: `restart` reuses the cached rootfs

The `restart` that followed the app edit reported `rootfs 71e00bc47fe4 cached`, while the next
`down` + `up` produced `rootfs 479f73c1a7b0 rebuilt` and served the new string. `reboot()` re-resolves
`snow.yml` modes/env/resources and re-runs hooks, but it never re-materializes the world's rootfs,
so an edit to **app source baked into the image** is not picked up by `restart` even once the stop
above is fixed. The task's wording allows either an app-file edit or a `snow.yml` hook edit; only the
latter can work on the current build, and this run's app-file edit does not.

Suggested follow-up (a fix node, not this verification node): signal the whole world-process process
group (or the deepest `nsenter` descendant), reap an orphaned service even when its row is already
`Exited`, and make `reboot()` re-hash/re-materialize the rootfs — or explicitly document that
`restart` only applies `snow.yml`/env/hook edits, which contradicts its own help text ("run the same
boot pipeline as `up`").

### What does hold in this step

- `[restarted] …` renders (here `[restarted] api, pg_clusters.psql`).
- A second `up` reports unchanged, human and JSON (`changed: false`, `restarted: []`; the JSON also
  shows the component row `exited`, the same defect seen from the wire):

```console
$ uv run --no-project tools/do_vm.py ssh -- /usr/sbin/runuser -u slipfork -- bash -lc 'cd ~/demo && slipfork up; echo "EXIT=$?"'
world     demo (running, unchanged — run `slipfork restart` to apply edits)
app       http://demo.localhost
worktree  /home/slipfork/demo
next      slipfork open | slipfork db | slipfork logs | slipfork restart | slipfork down
lineage   rootfs 01a4b324b7cb rebuilt 7.4s · postgres:18 cached · seed - - · base 4570132bffb8 reused
EXIT=0

$ uv run --no-project tools/do_vm.py ssh -- /usr/sbin/runuser -u slipfork -- bash -lc 'cd ~/demo && slipfork --json up; echo "EXIT=$?"'
{
  "changed": false,
  "lineage": {
    "base": {
      "key": "4570132bffb8d480614da23adf1ad1bbe56ab5bf5512b93386dacfcb71497107",
      "millis": null
    },
    "rootfs": {
      "cache": "miss",
      "digest": "01a4b324b7cb62b5a537be65b3765b1ed99ec7bcf5f9b77736e2e1b543ea93b6",
      "millis": 7406
    },
    "seed": null,
    "services": [
      {
        "cache": "hit",
        "id": "postgres:18",
        "millis": null
      }
    ]
  },
  "next": [
    "slipfork open",
    "slipfork db",
    "slipfork logs",
    "slipfork restart",
    "slipfork down"
  ],
  "ok": true,
  "origin": "http://demo.localhost",
  "resources": [
    {
      "id": "demo",
      "reason": "no_remote_session",
      "resolved": "local",
      "strategy": "prefer_remote"
    }
  ],
  "restarted": [],
  "services": [
    {
      "id": "api",
      "kind": "component",
      "pid": null,
      "port": null,
      "state": "exited",
      "uptime_secs": null,
      "world": "demo"
    },
    {
      "id": "pg_clusters.psql",
      "kind": "backing_service",
      "pid": 63370,
      "port": 5432,
      "state": "running",
      "uptime_secs": 6,
      "world": "demo"
    }
  ],
  "state": "running",
  "worktree": "/home/slipfork/demo",
  "world": "demo"
}
EXIT=0
```

- `restart` on a never-booted world exits `1` naming `slipfork up`:

```console
$ uv run --no-project tools/do_vm.py ssh -- /usr/sbin/runuser -u slipfork -- bash -lc 'cd ~/demo && slipfork restart --world demo:never-booted; echo "EXIT=$?"'
EXIT=1
slipfork: world 'demo:never-booted' is not running; boot it with `slipfork up`
```

- `-v` restores `[resources]` and the per-node `[lineage]` table:

```console
$ uv run --no-project tools/do_vm.py ssh -- /usr/sbin/runuser -u slipfork -- bash -lc 'cd ~/demo && slipfork -v up; echo "EXIT=$?"'
world     demo (running, unchanged — run `slipfork restart` to apply edits)
app       http://demo.localhost
worktree  /home/slipfork/demo
next      slipfork open | slipfork db | slipfork logs | slipfork restart | slipfork down
lineage   rootfs 479f73c1a7b0 rebuilt 10.3s · postgres:18 cached · seed - - · base 4570132bffb8 reused
[resources] demo: prefer_remote -> local (no_remote_session)
[lineage]   rootfs       479f73c1a7b0   rebuilt       10350ms
[lineage]   postgres:18  -              cached        -
[lineage]   seed         -              -             -
[lineage]   base         4570132bffb8   reused        -
EXIT=0
```

## 7. `ps` rows and fanned-out `logs` prefixes

```console
$ uv run --no-project tools/do_vm.py ssh -- /usr/sbin/runuser -u slipfork -- bash -lc 'cd ~/demo && slipfork ps; echo "EXIT=$?"'
state     running
lineage   rootfs 479f73c1a7b0 rebuilt 10.3s · postgres:18 cached · seed - - · base 4570132bffb8 reused
KIND             ID                STATE    PID    PORT  UPTIME
component        api               running  65180  8000  2s
backing_service  pg_clusters.psql  running  65144  5432  2s
EXIT=0
```

```console
$ uv run --no-project tools/do_vm.py ssh -- /usr/sbin/runuser -u slipfork -- bash -lc 'cd ~/demo && slipfork logs; echo "EXIT=$?"'
pg_clusters.psql: 
pg_clusters.psql: PostgreSQL Database directory appears to contain a database; Skipping initialization
pg_clusters.psql: 
api: demo-api: world=demo db=ready
api: demo-api: listening on 0.0.0.0:8000
EXIT=0
```

Every fanned-out line is prefixed `<source>: `. (Observation, tied to §6: the reported component
pid is the `sh -eu -c` wrapper, not the `python3` that owns `:8000` — in the §6 capture the `ps`
row's pid was `64538` = `sh -eu -c python3 /app/server.py`, while `64541` =
`python3 /app/server.py` held the port.)

## 8. `fork` with no argument and no ambient git identity

The VM's `slipfork` account carried a global git identity from an earlier session
(`remy.sanchez@niji.fr`), and the seeded demo carries a repo-local identity. To reproduce the
reported condition — no identity the fork could borrow — both were removed for this one run and
restored afterwards; the script shows each probe. Branch `feat/ux` derives fork `feat-ux` (a first
attempt on a plain `feat-vm` branch was refused — §8.1).

```console
$ uv run --no-project tools/do_vm.py ssh -- /usr/sbin/runuser -u slipfork -- bash -lc 'bash /tmp/opencode-m32813-fork.sh; echo "SCRIPT_EXIT=$?"'
$ git branch --show-current
feat-vm
$ git config --global user.email
remy.sanchez@niji.fr
GLOBAL_EMAIL_EXIT=0
$ git config --global user.name
Rémy Sanchez
GLOBAL_NAME_EXIT=0
$ git config --local user.email
demo@slipfork.invalid
LOCAL_EMAIL_EXIT=0
--- remove every identity source for one run (restored at the end) ---
$ git config --global user.email
GLOBAL_EMAIL_EXIT=1
$ git config --local user.email
LOCAL_EMAIL_EXIT=1
--- put the checkout on a feature branch (feat/ux -> fork feat-ux) ---
M	app/server.py
Switched to branch 'main'
Deleted branch feat-vm (was 9a95661).
Switched to a new branch 'feat/ux'
$ git config --global user.email
GLOBAL_EMAIL_EXIT=1
$ slipfork fork
[ready]     api (internal :8000 in world netns)
world     demo:feat-ux (running, changed)
app       http://feat-ux.demo.localhost
worktree  /home/slipfork/demo/.worktrees/feat-ux
cd        cd /home/slipfork/demo/.worktrees/feat-ux
next      slipfork open --world demo:feat-ux | slipfork db --world demo:feat-ux | slipfork logs --world demo:feat-ux | slipfork restart --world demo:feat-ux | slipfork down --world demo:feat-ux
lineage   rootfs 479f73c1a7b0 cached · postgres:18 cached · seed - - · base 4570132bffb8 reused
FORK_EXIT=0
--- fork checkout facts ---
feat-ux
BRANCH_EXIT=0
9a95661
HEAD_EXIT=0
 M app/server.py
STATUS_EXIT=0
$ slipfork list
WORLD            STATE     ORIGIN                                   WORKTREE
demo             running   http://demo.localhost                    /home/slipfork/demo
demo:feat-ux     running   http://feat-ux.demo.localhost            /home/slipfork/demo/.worktrees/feat-ux
--- restore the owner's git identity ---
$ git config --global user.email
remy.sanchez@niji.fr
GLOBAL_EMAIL_EXIT=0
EXIT=0
SCRIPT_EXIT=0
```

No `Author identity unknown`: the fork's WIP snapshot commit is authored by the pinned identity
(`-c user.name=slipfork -c user.email=slipfork@localhost`), proven end-to-end because the fork's
checkout was dirty (`M app/server.py`) and `refs/slipfork/feat-ux/wip` still resolved later (§11).

The fork's origin is port-less and routes:

```console
$ uv run --no-project tools/do_vm.py ssh -- bash -lc "curl -fsS -o /dev/null -w '%{http_code}\n' -H 'Host: feat-ux.demo.localhost' http://127.0.0.1/; curl -fsS -H 'Host: feat-ux.demo.localhost' http://127.0.0.1/; echo EXIT=\$?"
200
{"debug": true, "hello": "world-M3.28.13-r2", "host": "slipfork-dev-vm", "secret_len": 32, "world": "feat-ux"}
EXIT=0
```

### 8.1 A plain branch name (no slash) is refused, by design

A branch whose sanitized name equals an existing branch cannot become a fork id, because a fork
creates `refs/heads/<fork>`. Reproduced after the live suites, on the restored world (the `slipfork
up` block that re-booted the world after the suites is elided at the top of this excerpt):

```console
$ uv run --no-project tools/do_vm.py ssh -- /usr/sbin/runuser -u slipfork -- bash -lc 'bash /tmp/opencode-m32813-fork-refusal.sh; echo "SCRIPT_EXIT=$?"'
$ git checkout -b feat-vm
BRANCH_ADD_EXIT=0
$ slipfork fork
Switched to a new branch 'feat-vm'
slipfork: fork 'feat-vm' already exists (refs/heads/feat-vm, refs/slipfork/feat-vm/wip, or .worktrees/feat-vm); refusing to overwrite it
FORK_EXIT=1
$ git checkout feat/ux && git branch -D feat-vm
M	app/server.py
Switched to branch 'feat/ux'
CHECKOUT_BACK_EXIT=0
Deleted branch feat-vm (was 9a95661).
BRANCH_DELETE_EXIT=0
EXIT=0
SCRIPT_EXIT=0
```

The derived form therefore works for branch names that sanitize to a different id (`feat/ux` ->
`feat-ux`). This is a UX wart worth a follow-up (the message is clear and exit `1`, so nothing is
silently lost), not the reported identity crash.

## 9. `shell` (one orientation line, no `getcwd() failed`) and `exec -- pwd`

```console
$ printf 'exit\n' | ssh -F /dev/null -o IdentitiesOnly=yes -o IdentityAgent=none -o ControlMaster=no -o ControlPath=none -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile=.slipfork/do-vm/known_hosts-620073485 -i ~/.cache/slipfork/do-vm/id_ed25519 -tt root@165.232.86.174 "/usr/sbin/runuser -u slipfork -- bash -lc 'cd ~/demo && slipfork shell'"   # CRs stripped for display
Slipfork demo — account: slipfork (from root: sudo -iu slipfork)
  cd ~/demo
  slipfork
  slipfork up
  slipfork list
exit
world rootfs is the app stage (no host tools; use slipfork ps / logs / open)
slipfork@slipfork-dev-vm:/home/slipfork/demo$ exit
exit
Connection to 165.232.86.174 closed.
EXIT=0
```

Exactly one orientation line, a prompt already inside `/home/slipfork/demo`, and no
`getcwd() failed`. The same transcript shows the login banner (finding 11) instead of a
`getent passwd` line.

```console
$ uv run --no-project tools/do_vm.py ssh -- /usr/sbin/runuser -u slipfork -- bash -lc 'cd ~/demo && slipfork exec -- pwd >/tmp/m32813-exec-out 2>/tmp/m32813-exec-err; echo "EXEC_EXIT=$?"; echo "--- stdout"; cat /tmp/m32813-exec-out; echo "--- stderr bytes"; wc -c < /tmp/m32813-exec-err; cat /tmp/m32813-exec-err'
EXEC_EXIT=0
--- stdout
/home/slipfork/demo
--- stderr bytes
0
```

## 10. `open --remote` and the tunnel exercised from this machine

```console
$ uv run --no-project tools/do_vm.py ssh -- /usr/sbin/runuser -u slipfork -- bash -lc 'cd ~/demo && slipfork open --remote; echo "EXIT=$?"'
world   demo
ssh     ssh -N -L 8080:127.0.0.1:80 slipfork@slipfork-dev-vm
app     http://demo.localhost:8080
EXIT=0
```

The printed tunnel target is the daemon's real ingress port `80`. The recipe was then exercised
from this machine with only the unresolvable VM hostname replaced by the droplet IP (the printed
`-L 8080:127.0.0.1:80`, account `slipfork` and local port used verbatim), and a browser-style `Host`
header carrying the forwarded `:8080`:

```console
$ ssh -F /dev/null -o IdentitiesOnly=yes -o IdentityAgent=none -o ControlMaster=no \
      -o ControlPath=none -o StrictHostKeyChecking=accept-new \
      -o UserKnownHostsFile=.slipfork/do-vm/known_hosts-620073485 \
      -o ExitOnForwardFailure=yes -o BatchMode=yes \
      -i ~/.cache/slipfork/do-vm/id_ed25519 \
      -N -L 8080:127.0.0.1:80 slipfork@165.232.86.174 &
$ curl -fsS -o /dev/null -w '%{http_code}\n' -H 'Host: demo.localhost:8080' http://127.0.0.1:8080/
200
EXIT=0
$ curl -fsS -H 'Host: demo.localhost:8080' http://127.0.0.1:8080/
{"debug": true, "hello": "world-M3.28.13-r2", "host": "slipfork-dev-vm", "secret_len": 32, "world": "demo"}
EXIT=0
```

The tunnel process was backgrounded, probed, and killed after the two requests.

Not exercised: the owner's laptop itself (no laptop in this session); the recipe was exercised from
the machine that ran this node, which is the same laptop-side role.

## 11. `db`, `logs`, `drop`, and the `git fetch slipfork` round-trip

```console
$ printf '\pset pager off\nselect count(*) as items from items;\n\q\n' | ssh -tt … root@165.232.86.174 "TERM=xterm timeout 60 /usr/sbin/runuser -u slipfork -- bash -lc 'cd ~/demo && slipfork db demo'"   # CRs stripped
\pset pager off
select count(*) as items from items;
\q

Slipfork demo — account: slipfork (from root: sudo -iu slipfork)
  cd ~/demo
  slipfork
  slipfork up
  slipfork list
psql (18.7 (Debian 18.7-1.pgdg13+2))
Type "help" for help.

demo=# \pset pager off
Pager usage is off.
demo=# select count(*) as items from items;
 items 
-------
     4
(1 row)

demo=# \q
Connection to 165.232.86.174 closed.
EXIT=0
```

(A first attempt without `\pset pager off` left `psql` in its pager and was killed by the local
timeout after already printing the `PostgreSQL 18.7` banner and the query result; the transcript is
kept verbatim in `evidence/11a-db-pager-hang-first-try-raw.txt`.)

`logs` is in §7. The fetch round-trip and teardown:

```console
$ uv run --no-project tools/do_vm.py ssh -- /usr/sbin/runuser -u slipfork -- bash -lc 'bash /tmp/opencode-m32813-fetch-drop.sh; echo "SCRIPT_EXIT=$?"'
$ git remote add slipfork slipfork://feat-ux
REMOTE_ADD_EXIT=0
slipfork	slipfork://feat-ux (fetch)
slipfork	slipfork://feat-ux (push)
$ git fetch slipfork feat-ux
From slipfork://feat-ux
 * branch            feat-ux    -> FETCH_HEAD
 * [new branch]      feat-ux    -> slipfork/feat-ux
FETCH_BRANCH_EXIT=0
$ git fetch slipfork '+refs/slipfork/feat-ux/wip:refs/slipfork/feat-ux/wip'
FETCH_WIP_EXIT=0
$ git rev-parse refs/slipfork/feat-ux/wip
c37f83d0d5adbc8e250f450b977e4b9054dce039
REV_PARSE_EXIT=0
$ git log --oneline -1 refs/slipfork/feat-ux/wip
c37f83d slipfork: fork snapshot
LOG_EXIT=0
$ slipfork drop feat-ux
[drop] fork world and worktree removed
DROP_EXIT=0
$ slipfork list
WORLD            STATE     ORIGIN                                   WORKTREE
demo             running   http://demo.localhost                    /home/slipfork/demo
LIST_EXIT=0
$ git branch --list feat-ux; git worktree list
BRANCH_LIST_EXIT=0
/home/slipfork/demo  9a95661 [feat/ux]
$ ls .worktrees
WORKTREES_LS_EXIT=0
EXIT=0
SCRIPT_EXIT=0
```

`drop` removed the world, the worktree, the branch and the fork ref; the canonical world kept
running.

## 12. Two-tier secret proof (local vault `0600`; remote tier unexercised)

```console
$ uv run --no-project tools/do_vm.py ssh -- /usr/sbin/runuser -u slipfork -- bash -lc 'bash /tmp/opencode-m32813-secret.sh; echo "SCRIPT_EXIT=$?"'
$ printf '%s' local-tier-M3.28.13-marker | slipfork secret set LOCAL_ONLY
[secret] stored secrets::LOCAL_ONLY (value never printed)
SET_EXIT=0
$ slipfork secret list
DEMO_SECRET
LOCAL_ONLY
SECRET_LIST_EXIT=0
$ slipfork up
world     demo (running, unchanged — run `slipfork restart` to apply edits)
app       http://demo.localhost
worktree  /home/slipfork/demo
next      slipfork open | slipfork db | slipfork logs | slipfork restart | slipfork down
lineage   rootfs 479f73c1a7b0 rebuilt 10.3s · postgres:18 cached · seed - - · base 4570132bffb8 reused
UP_EXIT=0
$ grep -rIl -- local-tier-M3.28.13-marker ~/.cache/slipfork
STATE_GREP_EXIT=1
$ ls -l ~/.cache/slipfork
drwx------ 1 slipfork slipfork 154 Nov 18 04:05 /home/slipfork/.cache/slipfork
drwx------ 1 slipfork slipfork  42 Nov 18 04:05 /home/slipfork/.cache/slipfork/run
$ stat -c '%a %n' ~/.local/share/slipfork/vaults/demo.json
600 /home/slipfork/.local/share/slipfork/vaults/demo.json 155 bytes
STAT_EXIT=0
$ grep -c LOCAL_ONLY ~/.local/share/slipfork/vaults/demo.json
1
VAULT_GREP_EXIT=0
$ grep -rIl -- local-tier-M3.28.13-marker ~/.local
/home/slipfork/.local/share/slipfork/vaults/demo.json
LOCAL_TREE_GREP_EXIT=0
EXIT=0
SCRIPT_EXIT=0
```

The plaintext exists in exactly one place among the trees searched on the VM (the project's local
`0600` vault) and in nothing under the state root (`grep` found zero files, exit `1`, which is
grep's "no match"). Only `~/.local` and the state root were searched; no claim is made about the
rest of the VM filesystem. `DEMO_SECRET` (the `modes.local` `!!` value the component received,
`secret_len: 32`) is a local-tier vault key too.

**Not exercised:** the remote OSPP tier. This environment has no OSSP server and no paired endpoint
(`[resources] demo: prefer_remote -> local (no_remote_session)` is the daemon's own statement of
that), so "a remote secret's plaintext never appears on disk" is asserted by the process docs
(`ARCHITECTURE.md` §3.1, `docs/design/ospp.md`) and by the unit tests, not by a live capture here.
No remote secret was set, and none is claimed.

## 13. Live suites, final state, and the droplet for the owner

`uv run --no-project tools/do_vm.py test` — five suites, all green (adjacent lines elided; the full
92-line log is `evidence/13a-do-vm-test.txt`):

```console
$ uv run --no-project tools/do_vm.py test
     Running tests/critical_btrfs_live.rs (target/debug/deps/critical_btrfs_live-8d5e3720f4561f1e)
running 1 test
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 31.63s
     Running tests/critical_btrfs_live.rs (target/debug/deps/critical_btrfs_live-e92de28fb464dfa4)
running 1 test
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 31.49s
     Running tests/critical_cli.rs (target/debug/deps/critical_cli-b098109aeaa20016)
running 30 tests
test result: ok. 30 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 190.34s
     Running tests/critical_slug.rs (target/debug/deps/critical_slug-5b6e89e6b2f59380)
running 6 tests
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.53s
     Running tests/critical_ospp_daemon.rs (target/debug/deps/critical_ospp_daemon-ba9cfe35154b50d1)
running 6 tests
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 211.90s
unchanged slipforkd.socket, slipforkd.service, /etc/apparmor.d/slipfork; enabled
EXIT=0
```

The suites are green while §6 fails live. `critical_ospp_daemon`'s
`pushed_env_update_restarts_only_the_affected_component` (visible in the log) and the CLI restart
tests therefore do not cover a component whose `run_command` keeps `sh -eu -c` as a distinct parent
— which is exactly the shape `slipforkd` itself creates for every component
(`crates/slipfork/src/daemon.rs:2248-2253`). A protected regression test for that shape belongs to
the fix node.

`make check` — the whole local gate (plan, guard, stub, ruff, pytest, support, bun, cargo
fmt/clippy/doc/test, zensical, docs honesty):

```console
$ make check
uv run --no-project tools/plan.py validate
plan OK
uv run --no-project tools/guard.py
guard: OK (657 file(s) checked)
uv run --no-project tools/check_stub_adapters.py --check all
check_stub_adapters: OK (92 production file(s), 6 composition root(s))
…
150 passed in 13.52s
…
check_support: OK (10 integration(s); all evidence pointers verified and docs/support-matrix.md in sync)
…
 16 pass
 0 fail
…
uvx --offline zensical build --strict
Build started
No issues found
Build finished in 0.18s
uv run --no-project tools/check_docs_honesty.py
check_docs_honesty: OK (10 doc(s) match real CLI subcommands: published pages plus support/demo/README.md)
EXIT=0
```

The task's own acceptance commands:

```console
$ uv run --no-project tools/plan.py validate
plan OK
EXIT=0
```

```console
$ uv run --no-project tools/guard.py
guard: OK (662 file(s) checked)
EXIT=0
```

```console
$ test -s plan/nodes/M3/M3.28/M3.28.13/RESULT.md && grep -c 'EXIT=' plan/nodes/M3/M3.28/M3.28.13/RESULT.md
116
EXIT=0
```

(The count is above the required 12 and grows as this file grows; `guard.py` saw more files than
inside `make check` because `evidence/` was added in between.)

## Unexercised (stated, never claimed)

- **Remote OSPP secret tier (§12):** no OSSP server or paired endpoint exists in this environment,
  so no remote secret was set and none of the "remote plaintext never reaches disk" behaviour was
  captured live here. The local tier was exercised end-to-end.
- **The owner's laptop (§10):** the printed `ssh -N -L …` recipe was exercised from the machine
  that ran this node (the laptop-side role) after replacing only the unresolvable VM hostname with
  the droplet IP.
- **The `-v` `[resources]` rows for a *remote* resolution:** this daemon has no remote session, so
  the row honestly reads `prefer_remote -> local (no_remote_session)`; the remote branch of that
  renderer is not exercised.

Final droplet state:

```console
$ uv run --no-project tools/do_vm.py status
droplet  slipfork-dev-vm  id=620073485  region=ams3  size=s-4vcpu-8gb  ip=165.232.86.174
volume   slipfork-dev-vm  id=131a2765-e1ba-11f1-b67b-d6114774f516  region=ams3  100 GiB
keys     authorized_keys fingerprints:
         == /root/.ssh/authorized_keys
         256 SHA256:nZVhV69SFiOZLtnUMnPnu982J+lqO/40YjMiQiqKSu8 slipfork-do-vm (ED25519)
         2048 SHA256:SGXbyi5u4i8RO0JgxubkI2RZ2N7M4bCFB4HdUkgas9w remy@doc (RSA)
         == /home/slipfork/.ssh/authorized_keys
         256 SHA256:nZVhV69SFiOZLtnUMnPnu982J+lqO/40YjMiQiqKSu8 slipfork-do-vm (ED25519)
         2048 SHA256:SGXbyi5u4i8RO0JgxubkI2RZ2N7M4bCFB4HdUkgas9w remy@doc (RSA)
ssh      ssh -i /home/remy/.cache/slipfork/do-vm/id_ed25519 root@165.232.86.174
sync     tools/do_vm.py sync
down     tools/do_vm.py down --yes
EXIT=0
```

Left running on purpose (the state this node found, plus a booted demo): the scheduled units,
`slipforkd.socket` active on `:80`, and the `demo` world running and serving
`{"hello": "world-M3.28.13-r2", …}` through the port-less origin. The checkout is on branch
`feat/ux` with the demo edit, the `slipfork` remote `slipfork://feat-ux`, and no fork world or
`.worktrees/feat-ux` left (`drop` removed them). The `slipfork` account's global
`~/.gitconfig` and the demo's repo-local identity were restored to their arrival values.

```console
$ uv run --no-project tools/do_vm.py ssh -- bash -lc "curl -fsS -o /dev/null -w '%{http_code}\n' -H 'Host: demo.localhost' http://127.0.0.1/; echo EXIT=\$?"
200
EXIT=0
```

```console
$ uv run --no-project tools/do_vm.py ssh -- /usr/sbin/runuser -u slipfork -- bash -lc 'cd ~/demo && slipfork list; slipfork ps; echo "EXIT=$?"'
WORLD            STATE     ORIGIN                                   WORKTREE
demo             running   http://demo.localhost                    /home/slipfork/demo
state     running
lineage   rootfs 479f73c1a7b0 cached · postgres:18 cached · seed - - · base 4570132bffb8 reused
KIND             ID                STATE    PID    PORT  UPTIME
component        api               running  78362  8000  176s
backing_service  pg_clusters.psql  running  78330  5432  176s
EXIT=0
```

World subvolumes under the demo state root (`/var/lib/slipfork` paths are relative):

```console
home/.cache/slipfork/snapshots/demo-seed-empty
home/.cache/slipfork/snapshots/demo-base-4570132bffb8d480614da23adf1ad1bbe56ab5bf5512b93386dacfcb71497107
home/.cache/slipfork/worlds/canonical-6
home/.cache/slipfork/worlds/canonical-1
home/.cache/slipfork/worlds/canonical-2
home/.cache/slipfork/worlds/canonical-3
home/.cache/slipfork/worlds/canonical-4
home/.cache/slipfork/worlds/canonical-5
home/.cache/slipfork/worlds/canonical-7
home/.cache/slipfork/worlds/canonical-8
home/.cache/slipfork/worlds/canonical-9
```

No fork world subvolume survives `drop` (nothing named for `feat-ux`), so the fork teardown is
clean. Observation outside the twelve findings: the nine `worlds/canonical-*` entries are all one
world id's state dirs — `canonical-1..7` predate this run (the 2026-11-17 sessions), `canonical-8`
came from this run's first boot after `sync`, and `canonical-9` from the boot after
`do_vm.py test` re-installed the daemon. Every daemon handover orphans the previous world dir (a
fresh daemon cannot re-adopt it) and `prune` does not reap it while the checkout exists. Not one of
the reported findings, and not assessed further here.

To stop the billing (the owner's decision; keeps the 100 GiB volume unless `--delete-volume`):

```console
$ uv run --no-project tools/do_vm.py down --yes
```
