# M3.19 RESULT — live verification on the Btrfs DO VM

- **Date:** 2026-11-17 (UTC)
- **Branch / base:** `release/0.1.0`, base commit `053596ee33193d5e947e933bf2c0506f366ff3f8` (working tree synced as-is)
- **Droplet:** `slipfork-dev-vm` id `620073485`, ams3, `165.232.86.174`, volume `131a2765-e1ba-11f1-b67b-d6114774f516` (100 GiB Btrfs at `/var/lib/slipfork`)
- **Verdict:** `STATUS: blocked` (`BLOCK_KIND: direction-challenge`). The Btrfs ioctl path is proven live (RO snapshot + two writable clones), but **no world can start a `crun` backing service**: Ubuntu 24.04's `kernel.apparmor_restrict_unprivileged_userns=1` denies the in-world `crun` its nested user namespace because `/etc/apparmor.d/slipfork` does not cover `/.slipfork/bin/crun`. Kernel audit evidence is in §2.3. The engine suite, the demo's `up`/`fork`, and therefore the benchmark all stop at the same first assertion.

---

## 1. Environment

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

```console
$ uv run --no-project tools/do_vm.py sync
synced 501 file(s) to /home/slipfork/slipfork on 165.232.86.174
EXIT=0
```

Host: Ubuntu 24.04, 4 vCPU, 7941 MiB RAM, cargo/rustc 1.94.0, docker buildx 0.30.1, `kernel.apparmor_restrict_unprivileged_userns = 1`, `/etc/apparmor.d/slipfork` loaded.

---

## 2. Phase 1 — live suites

### 2.1 `do_vm.py test` — FAILED (exit 101)

```console
$ uv run --no-project tools/do_vm.py test
    Updating crates.io index
    Updating git repository `https://github.com/OffworldNexus/tpmd`
 Downloading crates ...
  Downloaded fastrand v2.5.0
  Downloaded errno v0.3.15
  Downloaded zmij v1.0.23
  Downloaded version_check v0.9.5
  Downloaded thiserror v2.0.21
  Downloaded serde_derive v1.0.229
  Downloaded memchr v2.8.3
  Downloaded unicode-ident v1.0.27
  Downloaded serde v1.0.229
  Downloaded proc-macro2 v1.0.107
  Downloaded tempfile v3.27.0
  Downloaded serde_json v1.0.154
  Downloaded once_cell v1.21.4
  Downloaded getrandom v0.4.3
  Downloaded quote v1.0.47
  Downloaded generic-array v0.14.7
  Downloaded typenum v1.20.1
  Downloaded hex v0.4.3
  Downloaded rustix v1.1.6
  Downloaded crypto-common v0.1.7
  Downloaded cpufeatures v0.2.17
  Downloaded thiserror-impl v2.0.21
  Downloaded sha2 v0.10.9
  Downloaded serde_core v1.0.229
  Downloaded itoa v1.0.19
  Downloaded digest v0.10.7
  Downloaded cfg-if v1.0.5
  Downloaded syn v3.0.10
  Downloaded bitflags v2.13.2
  Downloaded block-buffer v0.10.4
  Downloaded base64 v0.22.1
  Downloaded libc v0.2.191
  Downloaded linux-raw-sys v0.12.1
    Finished `test` profile [unoptimized + debuginfo] target(s) in 23.51s
     Running tests/critical_btrfs_live.rs (target/debug/deps/critical_btrfs_live-842ff7ac9a2426cd)

running 1 test
test btrfs_kernel_ro_snapshot_two_clones_same_port_services_and_ipc_isolation ... docker.io/library/postgres:18
ba28425e6d6fb1e6e63f0b3afa8e707180a89bd658230138484884482e0fbad3
docker.io/library/redis:8
7881ddbf99a32da1b32bb602c11cc1c7e2f665ba8eeaa0c24b231b8eef8aa400
FAILED

failures:

---- btrfs_kernel_ro_snapshot_two_clones_same_port_services_and_ipc_isolation stdout ----

thread 'btrfs_kernel_ro_snapshot_two_clones_same_port_services_and_ipc_isolation' (35659) panicked at crates/slipfork-engine/tests/critical_btrfs_live.rs:741:33:
clone a: spawn postgres: engine protocol/execution error: service 'postgres' exited before port 5432 accepted connections (captured logs retained)
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
slipfork-engine: leaked world 'canonical-1' reclaimed on BtrfsEngine drop (ctl=/var/lib/slipfork/home/.cache/slipfork/test-tmp/btrfs-liven0HncE/state/ctl/canonical-1, subvolume=/var/lib/slipfork/home/.cache/slipfork/test-tmp/btrfs-liven0HncE/state/worlds/canonical-1)
slipfork-engine: leaked world 'fork-sib-2' reclaimed on BtrfsEngine drop (ctl=/var/lib/slipfork/home/.cache/slipfork/test-tmp/btrfs-liven0HncE/state/ctl/fork-sib-2, subvolume=/var/lib/slipfork/home/.cache/slipfork/test-tmp/btrfs-liven0HncE/state/worlds/fork-sib-2)


failures:
    btrfs_kernel_ro_snapshot_two_clones_same_port_services_and_ipc_isolation

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 50.22s

error: test failed, to rerun pass `-p slipfork-engine --test critical_btrfs_live`
EXIT=101
```

The harness stops at the first failing suite, so `slipfork:critical_btrfs_live`, `slipfork:critical_cli`, `slipfork:critical_slug`, and `slipfork:critical_ospp_daemon` never ran.

### 2.2 What the suite *did* prove before it failed

`run_kernel_suite` (`crates/slipfork-engine/tests/critical_btrfs_live.rs:130`) executes in order:

1. **passed** — one real `BTRFS_IOC_SNAP_CREATE_V2` read-only snapshot of `state-src` (`state/snapshots/base`), payload read back, and `fs::write(sealed/"must-fail")` refused because `BTRFS_SUBVOL_RDONLY` is set.
2. **passed** — two real writable clones of that snapshot (`canonical-1`, `fork-sib-2`).
3. **failed at its first step** — `spawn_service("postgres", postgres:18)` in clone a.

So the kernel Btrfs snapshot/clone path is green; the failure is the `crun` container start, before any port/IPC/DB assertion.

### 2.3 Root cause (kernel audit evidence)

The engine writes an OCI config with a **user namespace** (`crates/slipfork-engine/src/oci.rs:86-94`: `namespaces: [user, pid, mount, ipc, uts]`, identity `uidMappings`/`gidMappings`), so `crun` creates a nested user namespace inside the world's user namespace. `crun` is staged at `/.slipfork/bin/crun` (`CRUN_GUEST_PATH`) and is executed inside the world's mount namespace. Ubuntu 24.04 transitions that unconfined binary into the `unprivileged_userns` profile and denies `CAP_SYS_ADMIN`:

```console
$ dmesg | grep -i apparmor | tail
[41695.985472] audit: type=1400 audit(1794915169.693:126): apparmor="AUDIT" operation="userns_create" class="namespace" info="Userns create - transitioning profile" profile="unconfined" pid=36044 comm="crun" requested="userns_create" target="unprivileged_userns"
[41695.987105] audit: type=1400 audit(1794915169.695:127): apparmor="DENIED" operation="capable" class="cap" profile="unprivileged_userns" pid=36045 comm="crun" capability=21  capname="sys_admin"
[41696.037118] audit: type=1400 audit(1794915169.745:128): apparmor="AUDIT" operation="userns_create" class="namespace" info="Userns create - transitioning profile" profile="unconfined" pid=36053 comm="crun" requested="userns_create" target="unprivileged_userns"
[41696.038375] audit: type=1400 audit(1794915169.746:129): apparmor="DENIED" operation="capable" class="cap" profile="unprivileged_userns" pid=36054 comm="crun" capability=21  capname="sys_admin"
[42017.807246] audit: type=1400 audit(1794915491.529:130): apparmor="AUDIT" operation="userns_create" class="namespace" info="Userns create - transitioning profile" profile="unconfined" pid=41053 comm="crun" requested="userns_create" target="unprivileged_userns"
[42063.285564] audit: type=1400 audit(1794915537.009:131): apparmor="AUDIT" operation="userns_create" class="namespace" info="Userns create - transitioning profile" profile="unconfined" pid=41413 comm="crun" requested="userns_create" target="unprivileged_userns"
```

`crun` reports this as `unshare: Operation not permitted`, which is exactly the captured service-log tail the daemon prints (see §4).

The loaded profile (harness-rendered by `tools/do_vm.py`) covers only the two executables the world boot needs:

```console
$ cat /etc/apparmor.d/slipfork
abi <abi/4.0>,
profile slipfork /{usr/bin/unshare,/home/slipfork/slipfork/target/debug/slipfork-world-init} flags=(unconfined) {
  userns,
}
```

The product's own renderer is narrower still — it never covers `unshare`, `slipfork-world-init`, or the in-world `crun`:

```console
$ slipfork daemon unit --only apparmor
abi <abi/4.0>,
profile slipfork /usr/local/bin/slipfork{,d} flags=(unconfined) {
  userns,
}
```

`ADR-0010` §7 states that unprofiled children (`unshare`, `slipfork-world-init`, `/.slipfork/bin/crun`) "remain under the unconfined `slipfork` profile across `execve`". That is not what the kernel does: an unprofiled `userns` creator transitions to `unprivileged_userns` and loses `CAP_SYS_ADMIN`. This is a specification blind spot, not a flake — it reproduces on every `up`, before and after the reboot.

---

## 3. Acceptance table (exact commands, exits, output)

| # | Command | Exit | Output |
|---|---|---|---|
| 1 | `uv run --no-project tools/do_vm.py status` | 0 | droplet/volume/keys as §1 |
| 2 | `uv run --no-project tools/do_vm.py sync` | 0 | `synced 501 file(s) to /home/slipfork/slipfork on 165.232.86.174` |
| 3 | `uv run --no-project tools/do_vm.py test` | **101** | §2.1 (`test result: FAILED. 0 passed; 1 failed; …`) |
| 4 | `uv run --no-project tools/do_vm.py ssh -- findmnt -no OPTIONS /var/lib/slipfork` | 0 | `rw,noatime,discard=async,space_cache=v2,user_subvol_rm_allowed,subvolid=5,subvol=/` |
| 5 | `uv run --no-project tools/do_vm.py ssh -- stat -f -c %t /var/lib/slipfork` | 0 | `9123683e` |
| 6 | `uv run --no-project tools/do_vm.py ssh -- slipfork prune --yes` | 0 | `[prune] nothing to reap` |
| 7 | `uv run --no-project tools/do_vm.py ssh -- btrfs subvolume list /var/lib/slipfork \| grep -v /snapshots/` | 0 | *(empty)* |
| 8 | `uv run --no-project tools/do_vm.py ssh -- slipfork up` | 1 | `slipfork: not a git or jj repository: /home/slipfork/slipfork (run slipfork from inside a version-controlled checkout)` — the synced tree carries no `.git`; the intended project context is the demo (§6) |
| 9 | `uv run --no-project tools/do_vm.py ssh -- slipfork shell -- crun --version` | **2** | `error: unexpected argument 'crun' found` / `Usage: slipfork shell [OPTIONS]` |
| 10 | `uv run --no-project tools/do_vm.py ssh -- docker buildx version` | 0 | `github.com/docker/buildx 0.30.1 0.30.1-0ubuntu1~24.04.1` |

All subvolumes present (the acceptance grep correctly hides the sealed bases):

```console
$ btrfs subvolume list /var/lib/slipfork
ID 257 gen 15 top level 5 path home/.cache/slipfork/test-tmp/btrfs-liven0HncE/state/snapshots/base
ID 265 gen 42 top level 5 path home/.cache/slipfork/snapshots/demo-seed-empty
```

`ID 257` is the RO base the failed suite sealed and could not remove on its failure path (its `TempDir` cannot delete a read-only subvolume); `ID 265` is the demo's sealed `demo-seed-empty` base. Both live under a `/snapshots/` path, exactly as `NODE.md` allows.

### 3.1 `slipfork shell -- crun --version` is not a shipped CLI form

`Commands::Shell` (`crates/slipfork/src/cli.rs:151-155`) takes only `--world`; `clap` rejects a trailing argv with exit 2. The shipped non-interactive in-world entry is `slipfork exec -- <argv>` (`docs/reference/cli.md`), and bare `slipfork shell` fails closed naming it:

```console
$ uv run --no-project tools/do_vm.py ssh -- slipfork shell
slipfork: `slipfork shell` needs an interactive terminal; use `slipfork exec -- <cmd>` for non-interactive commands
EXIT=1
```

The in-world `crun --version` assertion could not be satisfied either way because no world boots (§2.3). `docs/design/do-vm-harness.md:288,401,544,635` and `NODE.md` line 46 name a form the CLI does not implement; that is a second, independent acceptance defect for the lead.

---

## 4. Phase 2 — benchmark: NOT RUN (blocked by §2.3)

`poc/run_benchmark.py` cannot produce an honest number: its first world boot (`measure_up_ms` → `slipfork up`) fails with the identical `crun`/AppArmor error, and every later metric (fork, warm-SQL, parallel worlds, browser suites) depends on that boot. No snapshot/clone/warm-SQL/RSS figure was measured, so none is reported here — no zeros, no estimates.

Two secondary gaps for the next attempt, recorded so the next run is not surprised:

- The harness hardcodes host prerequisites the VM does not have: `/usr/lib/postgresql/18/bin/{initdb,pg_dump,pg_restore}` (absent), `/usr/bin/google-chrome` (absent), `/usr/bin/zstd` (present). `preflight()` fails closed naming them.
- `poc/run_benchmark.py` builds/uses `target/release/slipfork` + `target/release/slipforkd`, which now exist on the VM (§5).

I did not install PostgreSQL 18 / Chrome because the run would still stop at the first `up` for the §2.3 reason.

---

## 5. Phase 3 — installed binaries and Btrfs state dirs (done)

Built as `slipfork` from the synced tree (release, `-p slipfork -p slipfork-engine --bins`; the engine binary is required because `slipforkd` resolves `slipfork-world-init` as its sibling — `crates/slipfork/src/runtime.rs:118-128`):

```console
$ cargo build --release -p slipfork -p slipfork-engine --bins
    Finished `release` profile [optimized] target(s) in 3m 25s
EXIT=0
```

Installed as root:

```console
$ install -m 0755 -t /usr/local/bin target/release/{slipfork,slipforkd,git-remote-slipfork,slipfork-world-init}
-rwxr-xr-x 1 root root  1687992 /usr/local/bin/git-remote-slipfork
-rwxr-xr-x 1 root root  5654584 /usr/local/bin/slipfork
-rwxr-xr-x 1 root root   804280 /usr/local/bin/slipfork-world-init
-rwxr-xr-x 1 root root 12661056 /usr/local/bin/slipforkd
```

```console
$ command -v slipfork slipforkd git-remote-slipfork slipfork-world-init   # root
/usr/local/bin/slipfork
/usr/local/bin/slipforkd
/usr/local/bin/git-remote-slipfork
/home/slipfork/slipfork/target/debug/slipfork-world-init   # owner-envelope PATH prefix; /usr/local/bin copy also present
$ slipfork --version
slipfork 0.2.0

$ command -v slipfork slipforkd git-remote-slipfork slipfork-world-init   # slipfork
/usr/local/bin/slipfork
/usr/local/bin/slipforkd
/usr/local/bin/git-remote-slipfork
/usr/local/bin/slipfork-world-init
$ slipfork --version
slipfork 0.2.0
```

Version string is `0.2.0` today (the `0.1.0` bump lands in M3.23; the lead re-syncs/re-installs).

State dirs on Btrfs for both users:

```console
$ ls -ld /home/slipfork/.cache /var/lib/slipfork/home/.cache /var/lib/slipfork/root-home/.cache
lrwxrwxrwx 1 root     root     29 /home/slipfork/.cache -> /var/lib/slipfork/home/.cache
drwx------ 1 slipfork slipfork 56 /var/lib/slipfork/home/.cache
drwxr-xr-x 1 root     root      0 /var/lib/slipfork/root-home/.cache
```

(`/home/slipfork/.cache` held only `motd.legal-displayed`, preserved by the move; `root-home` is already under the Btrfs mount, so only the directory was created.)

`slipfork doctor` — both users, exit 0, default env:

```console
$ slipfork doctor                                    # root, owner envelope
state_dir: /var/lib/slipfork/root-home/.cache/slipfork
filesystem: btrfs
btrfs_super_magic: ok
subvolume_ioctl: ok
noatime: ok
unshare_userns: ok
netns: ok
EXIT=0

$ slipfork doctor                                    # slipfork, login HOME
state_dir: /home/slipfork/.cache/slipfork
filesystem: btrfs
btrfs_super_magic: ok
subvolume_ioctl: ok
noatime: ok
unshare_userns: ok
netns: ok
EXIT=0
```

---

## 6. Phase 4 — owner demo at `/home/slipfork/demo`

Staged from the working tree, `git init` + one commit as `slipfork` (`d96ebd0 Slipfork demo project`), `git config --global --add safe.directory /home/slipfork/demo` for root.

### 6.1 Tree

```console
$ find /home/slipfork/demo -type f | sort
/home/slipfork/demo/.git/COMMIT_EDITMSG
/home/slipfork/demo/.git/HEAD
/home/slipfork/demo/.git/config
/home/slipfork/demo/.git/description
/home/slipfork/demo/.git/hooks/applypatch-msg.sample
/home/slipfork/demo/.git/hooks/commit-msg.sample
/home/slipfork/demo/.git/hooks/fsmonitor-watchman.sample
/home/slipfork/demo/.git/hooks/post-update.sample
/home/slipfork/demo/.git/hooks/pre-applypatch.sample
/home/slipfork/demo/.git/hooks/pre-commit.sample
/home/slipfork/demo/.git/hooks/pre-merge-commit.sample
/home/slipfork/demo/.git/hooks/pre-push.sample
/home/slipfork/demo/.git/hooks/pre-rebase.sample
/home/slipfork/demo/.git/hooks/pre-receive.sample
/home/slipfork/demo/.git/hooks/prepare-commit-msg.sample
/home/slipfork/demo/.git/hooks/push-to-checkout.sample
/home/slipfork/demo/.git/hooks/sendemail-validate.sample
/home/slipfork/demo/.git/hooks/update.sample
/home/slipfork/demo/.git/index
/home/slipfork/demo/.git/info/exclude
/home/slipfork/demo/.git/logs/HEAD
/home/slipfork/demo/.git/logs/refs/heads/master
/home/slipfork/demo/.git/objects/2d/4556c7f644de3109e6a4a7b0650307b6fed9fe
/home/slipfork/demo/.git/objects/38/b014bc98e0cee22c2f063a4c7829e7133bb49c
/home/slipfork/demo/.git/objects/42/98b4dc0444305015eea39bc56e0f5f0480e6c8
/home/slipfork/demo/.git/objects/45/09f658f3ed5abb5e42c10fe1478d4aadbfd25c
/home/slipfork/demo/.git/objects/5f/33507efd76abdf876901a20ff0af02117428cb
/home/slipfork/demo/.git/objects/6e/465d22eedcc57a0879949bf1eed37fe6f31c15
/home/slipfork/demo/.git/objects/d3/e0dfaefb77ce923fd8aca6e69b2e94ef4f4a68
/home/slipfork/demo/.git/objects/d8/ff5f91a9456394f3597e7b43786839bc91389b
/home/slipfork/demo/.git/objects/d9/6ebd0968d99db139cb3600bbdaddfdeac8162b
/home/slipfork/demo/.git/objects/db/172d04c9f65e0cbf138a0fbc49a6e56a7c031e
/home/slipfork/demo/.git/objects/e2/c1f5afabb9fedff439fbc1c7cf2be72e46e25b
/home/slipfork/demo/.git/packed-refs
/home/slipfork/demo/.git/refs/heads/master
/home/slipfork/demo/Dockerfile
/home/slipfork/demo/README.md
/home/slipfork/demo/app/server.py
/home/slipfork/demo/db/0001_schema.sql
/home/slipfork/demo/db/0002_seed.sql
/home/slipfork/demo/slipfork.toml
/home/slipfork/demo/snow.yml
```

Demo contents: `Dockerfile` (`FROM python:3.12-slim AS app`, `pip install "psycopg[binary]>=3.2,<3.3"`, copies `app/` and `db/`), `app/server.py` (stdlib `http.server` on `:8000`; on startup applies `db/0001_schema.sql` + `db/0002_seed.sql` idempotently, records one `boot` row scoped to its worktree, serves `/` and `/db`), `snow.yml` (one build image `api` with `context: .`, component `api` with `run_command: python3 /app/server.py` hardcoded to port 8000, `internal_ports: [8000]`, one `/` ingress to `api:8000`, `pg_clusters: [{id: psql, version: "18"}]`, `pg_databases: [{id: demo, cluster: psql}]`, `modes.all.envs.app` with `DATABASE_URL: '${{ resources::pg_databases.demo.url }}'`, `PGUSER: postgres`, `DEMO_SECRET: '${{ secrets::DEMO_SECRET !! random_hex(16) }}'`, and `modes.local.envs.app.DEBUG: 'True'`), `slipfork.toml` (`project = "demo"`), `README.md` (copy-pasteable commands, including the `curl -H 'Host: demo.localhost' http://127.0.0.1:<port>/` origin recipe).

### 6.2 Verbatim transcripts (as `slipfork`, from `/home/slipfork/demo`)

Every command below ran as:

```console
$ uv run --no-project tools/do_vm.py ssh -- /usr/sbin/runuser -u slipfork -- \
    env -u XDG_RUNTIME_DIR HOME=/home/slipfork \
    PATH=/home/slipfork/.cargo/bin:/usr/local/bin:/usr/bin:/bin \
    bash -c 'cd /home/slipfork/demo && <command>'
```

`-u XDG_RUNTIME_DIR` is required only because this non-login `runuser` inherits root's `/run/user/0`; a real `slipfork` login has no `XDG_RUNTIME_DIR` (or its own `/run/user/1000`), and the CLI then uses `<state_dir>/run` as documented.

```console
### slipfork doctor
state_dir: /home/slipfork/.cache/slipfork
filesystem: btrfs
btrfs_super_magic: ok
subvolume_ioctl: ok
noatime: ok
unshare_userns: ok
netns: ok
EXIT=0

### slipfork up
[resources] demo: prefer_remote -> local (not_offered)
slipfork: cannot start backing service 'pg_clusters.psql' in the base seed world: engine protocol/execution error: service 'pg_clusters.psql' exited before port 5432 accepted connections (captured logs retained)
unshare: Operation not permitted
EXIT=1

### slipfork fork feat-1
[resources] demo: prefer_remote -> local (not_offered)
slipfork: cannot start backing service 'pg_clusters.psql' in the base seed world: engine protocol/execution error: service 'pg_clusters.psql' exited before port 5432 accepted connections (captured logs retained)
unshare: Operation not permitted
EXIT=1

### slipfork list
WORLD            STATE     ORIGIN                                   WORKTREE
EXIT=0

### slipfork shell -- crun --version
error: unexpected argument 'crun' found

Usage: slipfork shell [OPTIONS]

For more information, try '--help'.
EXIT=2

### slipfork db demo
slipfork: world 'demo' is not running in this slipforkd; run `slipfork up` first
EXIT=1

### slipfork drop feat-1
[drop] no such fork world; nothing to do
EXIT=0

### slipfork down
[down] no running world matched; nothing to do
EXIT=0
```

Because `up`/`fork` cannot start the `postgres:18` container, the two-world independence proof (two sibling clones, same in-world `:5432`, independent seeded rows, `--ipc` isolation) is not reachable in this attempt. The suite's §2.2 result proves the snapshot/clone half; the container half is the blocker.

The demo files are not repo-tracked; they are left in place at `/home/slipfork/demo` for the owner.

---

## 7. Phase 5 — reboot check (done)

```console
$ doctl compute droplet-action reboot 620073485 --wait
Action (3553037206) completed (5s)
EXIT=0
```

SSH returned ~30 s later with the persistent `--identity` key; the volume remounted and everything but the engine's container path survived:

```console
$ ssh -i ~/.cache/slipfork/do-vm/id_ed25519 root@165.232.86.174 'uptime -p; uptime -s; findmnt -no OPTIONS /var/lib/slipfork; stat -f -c %t /var/lib/slipfork'
up 0 minutes
2026-11-17 11:40:05
rw,noatime,discard=async,space_cache=v2,user_subvol_rm_allowed,subvolid=5,subvol=/
9123683e
EXIT=0
```

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
EXIT=0
```

`slipfork doctor` exits 0 for root (`state_dir: /var/lib/slipfork/root-home/.cache/slipfork`) and for `slipfork` (`state_dir: /home/slipfork/.cache/slipfork`), both `filesystem: btrfs`, `btrfs_super_magic: ok`, `subvolume_ioctl: ok`. The demo's `slipfork up` after reboot fails with the same `unshare: Operation not permitted` (§2.3), so the blocker is not reboot-related.

---

## 8. `make check` (local) and `tools/guard.py`

```console
$ uv run --no-project tools/guard.py
guard: OK (501 file(s) checked)
```

`make check` passes `plan-check`, `guard`, `ruff check`, `ruff format --check`, `pytest tools/tests` (124 passed), `check_support`, `bun test` (16 pass), `cargo fmt --check`, and `cargo clippy --workspace --all-targets -D warnings`, then fails in `rs-lint` on **pre-existing uncommitted work outside this node**:

```console
$ RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
error: public documentation for `doctor` links to private item `filesystem_name`
 --> crates/slipfork/src/doctor.rs:8:39
  |
8 | //! [`run`] is the single composer, [`filesystem_name`] is the single `statfs`-code renderer,
  |                                       ^^^^^^^^^^^^^^^ this item is private
  = note: `-D rustdoc::private-intra-doc-links` implied by `-D warnings`
error: could not document `slipfork`
make: *** [Makefile:54: rs-lint] Error 101
```

This node writes no product code, so it neither introduced nor fixed that link.

---

## 9. Deviations, disclosures, and host changes

1. **`/root/.rustup` and `/root/.cargo` now symlink to `/home/slipfork/.rustup` / `/home/slipfork/.cargo`.** `do_vm.py test` probes `f"{REMOTE_HOME}/.cargo/bin/cargo --version"` over SSH **without** the suite envelope, so as root it had no `RUSTUP_HOME` and rustup answered `error: rustup could not choose a version of cargo to run … no default is configured` (exit 1), which the harness reported as `cargo is not installed on the VM; provisioning is incomplete` (exit 2). The toolchain was installed and working; only the probe's environment was missing. The symlinks make root's default rustup home the same one the harness's own suite envelope already exports (`RUSTUP_HOME=/home/slipfork/.rustup`). **Followup for M3.16:** the probe should set `CARGO_HOME`/`RUSTUP_HOME` like `suite_command` does; a fresh droplet will hit this again.
2. **`slipfork-world-init` was built and installed too** (`-p slipfork-engine --bins`). `slipforkd` resolves it as a sibling executable (`crates/slipfork/src/runtime.rs`), so `slipfork`/`slipforkd` alone cannot boot a world.
3. **`slipfork shell -- crun --version` does not exist** (§3.1). The shipped non-interactive form is `slipfork exec -- <cmd>`; the acceptance line and `docs/design/do-vm-harness.md` should be amended (or `shell` given a trailing argv) by the lead.
4. **`slipfork up` in the acceptance table** runs in the owner envelope at `/home/slipfork/slipfork`, which the harness syncs **without `.git`**; it therefore fails as "not a git or jj repository". The intended project context is `/home/slipfork/demo` (§6.2), where it fails only on the §2.3 blocker.
5. **`XDG_RUNTIME_DIR`** is unset for the demo commands because a non-login `runuser` inherits root's `/run/user/0`; without that, `slipfork` fails with `cannot create /run/user/0/slipfork: Permission denied`.
6. **Failure-path leak:** the failed live suite leaves its sealed read-only base subvolume behind (`home/.cache/slipfork/test-tmp/btrfs-liven0HncE/state/snapshots/base`); `TempDir` cannot delete a read-only subvolume. Harmless for the acceptance grep, but a cleanup gap worth a followup.
7. No tracked repo file other than this `RESULT.md` and `LOG.md` was modified; no `plan.py` call was made; no product code was touched.

---

## 10. Blocker (exact first failing assertion) and options

**First failing assertion (engine suite):**

```console
thread 'btrfs_kernel_ro_snapshot_two_clones_same_port_services_and_ipc_isolation' panicked at crates/slipfork-engine/tests/critical_btrfs_live.rs:741:33:
clone a: spawn postgres: engine protocol/execution error: service 'postgres' exited before port 5432 accepted connections (captured logs retained)
```

**First failing assertion (product path, demo):**

```console
$ slipfork up
slipfork: cannot start backing service 'pg_clusters.psql' in the base seed world: engine protocol/execution error: service 'pg_clusters.psql' exited before port 5432 accepted connections (captured logs retained)
unshare: Operation not permitted
EXIT=1
```

**Cause:** `crun` at `/.slipfork/bin/crun` creates the OCI `user` namespace declared in `crates/slipfork-engine/src/oci.rs`; Ubuntu 24.04's `unprivileged_userns` AppArmor transition denies `CAP_SYS_ADMIN` because `/etc/apparmor.d/slipfork` covers only `/usr/bin/unshare` and the debug `slipfork-world-init` (§2.3 audit lines).

**Remediation paths (need an owner/architect decision; all touch files outside this node's write scope):**

1. **Extend the profile to the in-world crun guest path** — add `/.slipfork/bin/crun` (and `/usr/bin/unshare`, the installed `slipfork-world-init`, and the `/.slipfork/bin/slipfork-world-init` payload) to `render_userns_profile` in `crates/slipfork/src/apparmor.rs` and to `tools/do_vm.py`'s harness profile, then correct `ADR-0010` §7's false "stays under the unconfined profile across execve" claim. Smallest change; needs verification that AppArmor resolves the guest path `/.slipfork/bin/crun` for a task inside the world's mount namespace.
2. **Stage crun at one stable host path** (bind-mount a single `/usr/local/lib/slipfork/crun` into every world) so the profile can name one absolute path instead of a per-world rootfs path. Cleaner profile, but changes the world rootfs contract (`ADR-0009` `/.slipfork/bin` layout) and the content-addressed rootfs digest inputs.
3. **Drop the OCI `user` namespace** and rely on the world's existing `userns` + `crun` `uidMappings` only if the runtime can map without a nested userns; this is the largest behavioural change and needs a security review.

Recommended: **path 1** first (it is the minimal, testable fix and matches the ADR's intent), with path 2 as the durable follow-up if the guest-path match proves unreliable.

---

## 11. Droplet state

**The droplet is LEFT UP** (id `620073485`, `165.232.86.174`), volume `131a2765-e1ba-11f1-b67b-d6114774f516` attached, no world running (`slipfork down` is a no-op; `slipfork list` is empty). The demo is staged at `/home/slipfork/demo` and the binaries are on `PATH` for both users.

The owner may destroy the tagged resources later with:

```console
uv run --no-project tools/do_vm.py down --yes
```

---

## 8. Ruling (architect, H-006)

1. **Mechanism (`ADR-0010` §7):** On Ubuntu 24.04 (`noble`, AppArmor 4.0) with `kernel.apparmor_restrict_unprivileged_userns=1`, `aa_profile_ns_perm` transitions any unprivileged `CLONE_NEWUSER` caller in the built-in `unconfined` label into `unprivileged_userns` (`audit deny capability,`), denying `CAP_SYS_ADMIN` (`EPERM`). Under `flags=(unconfined)`, `profile_transition` ignores file `ix` rules and runs `find_attach` against `aa_path_name` resolved in the **calling task's mount namespace** (`current->nsproxy->mnt_ns->root`) on every `execve`, and `BtrfsEngine::spawn_service` enters a world via `nsenter` (`setns(2)` preserves the caller's AppArmor `cred` label rather than inheriting `slipfork-world-init`'s) before `execve`ing `/.slipfork/bin/crun` inside the `pivot_root`ed world mount namespace. Both `M3.19` failures occurred because `/etc/apparmor.d/slipfork` on `165.232.86.174` attached only `/{usr/bin/unshare,/home/slipfork/slipfork/target/debug/slipfork-world-init}` — neither `critical_btrfs_live-*`, nor `/usr/local/bin/slipfork{,d}`, nor `/.slipfork/bin/crun` — leaving `nsenter -> /.slipfork/bin/crun` in `profile="unconfined"`.
2. **Exact attachment sets (single leading-slash `{...}` brace form):**
   - **Product (`crates/slipfork/src/apparmor.rs::render_userns_profile(bin_dir)` & `crates/slipfork-engine::userns_remediation_hint(bin_dir)`):**
     `/{<rel_bin_dir>/slipfork{,d,-world-init},usr/bin/unshare,.slipfork/bin/crun}` (with `<rel_bin_dir>` = `bin_dir` stripped of leading/trailing `/`, or bare `slipfork{,d,-world-init}` when `bin_dir == "/"`), expanding for `/usr/local/bin` to `/usr/local/bin/slipfork`, `/usr/local/bin/slipforkd`, `/usr/local/bin/slipfork-world-init`, `/usr/bin/unshare`, and `/.slipfork/bin/crun`.
   - **VM harness (`tools/do_vm.py::render_apparmor_body(remote_tree)`):**
     `/{usr/local/bin/slipfork{,d,-world-init},<rel_tree>/target/{debug,release}/slipfork{,d,-world-init},usr/bin/unshare,.slipfork/bin/crun}` (`<rel_tree>` = `remote_tree.strip("/")`), adding Cargo `target/{debug,release}` binaries alongside `/usr/local/bin`, `/usr/bin/unshare`, and `/.slipfork/bin/crun`.
3. **Product-vs-harness byte-equivalence rule (`tools/tests/test_do_vm.py`):** Yes, the harness profile legitimately expands only the `<bin_dir>` prefix clause to cover Cargo `target/{debug,release}` builds alongside `/usr/local/bin`. `test_do_vm.py` must assert exact 4-line output on both renderers and full-profile byte-equality after replacing **only** that `<bin_dir>` prefix clause with `<bin_clause>` (retaining `,usr/bin/unshare,.slipfork/bin/crun}` verbatim rather than erasing the whole attachment to `<files>`), and scope `test_provision_script_installs_no_host_crun_and_keeps_git` to `PACKAGES` / `apt-get install` rather than `"crun" not in script`.
4. **Security verdict:** Worlds are cooperative unprivileged `$UID:$GID` namespace sandboxes rather than hostile-tenant VMs (`ADR-0010`, `docs/design/do-vm-harness.md` §6), so granting `userns,` to in-world `/.slipfork/bin/crun` is strictly within `ADR-0010`'s trust boundary; attaching host `/usr/bin/unshare` widens Ubuntu's opportunistic `userns` gate on that workstation or CI VM to any local caller of `/usr/bin/unshare`, matching stock Ubuntu 24.04's own `/etc/apparmor.d/{crun,runc,chrome}` `flags=(unconfined)` shims without crossing any `$UID` or host-root boundary.
5. **Next implementer scope:** Update `crates/slipfork/src/apparmor.rs`, `crates/slipfork-engine/src/lib.rs` (`userns_remediation_hint`), `docs/reference/cli.md:147`, `tools/do_vm.py` (`render_apparmor_body`), and `tools/tests/test_do_vm.py` (plus unit tests); then on `165.232.86.174` remove `/var/lib/slipfork/.slipfork-ready`, run `tools/do_vm.py up --yes`, `tools/do_vm.py sync`, `tools/do_vm.py test`, reinstall `/usr/local/bin/{slipfork,slipforkd,git-remote-slipfork,slipfork-world-init}`, and verify `/home/slipfork/demo` (`slipfork up`, `slipfork fork`, `slipfork exec -- crun --version`).
