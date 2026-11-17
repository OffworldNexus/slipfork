# M3.28.12 RESULT — VM Harness: `:80` via `daemon install`, Tracked `support/demo`, Login Banner

- **Status:** `blocked` — **only** on one local gate that is not this node's file: `make check` fails
  at `rs-fmt` inside `crates/slipfork-edge/tests/critical_edge.rs` (M3.28.20's uncommitted edit, not
  rustfmt-formatted). Every other `make check` target and every `# Acceptance` command, including
  the live `:80` `200`, is green (§7, §8).
- **Node:** `M3.28.12` (`release/0.1.0`, base `19658f8`, attempt 3)
- **Scope (only):** `tools/do_vm.py`, `tools/tests/test_do_vm.py`, `support/demo/**` (new)
- **Authority:** `docs/design/dx-0.1.0-ux.md` §1/§2, `ADR-0011`
- **Droplet:** `165.232.86.174` (`slipfork-dev-vm`), volume `131a2765-…` (100 GiB Btrfs at
  `/var/lib/slipfork`)

## 1. What changed

1. **One AppArmor renderer (invariant 3).** `render_apparmor_body` is deleted. The profile reaches
   the VM only through `slipfork daemon install`, which renders the exact bytes of
   `slipfork daemon unit --only apparmor --bin-dir …`. The harness installs the four binaries into
   `/usr/local/bin` and names the same `--bin-dir` operands it hands the installer.
2. **`sync` is the install step.** After replacing `/home/slipfork/slipfork` it seeds the demo,
   runs `cargo build -p slipfork -p slipfork-engine --bins` under the suite envelope, installs the
   four binaries with `cmp -s` (a byte-identical source is never rewritten), releases a daemon from
   an older build (§2), and runs
   `"/usr/local/bin/slipfork" daemon install --user slipfork --bin-dir /usr/local/bin --bin-dir
   <tree>/target/debug --bin-dir <tree>/target/release`, then asserts
   `systemctl is-active slipforkd.socket` = `active`, `aa-status` shows the profile, and an
   unprivileged `unshare --user` still succeeds. `test` runs the same build+install before its
   suites. No step uses `|| true`; a missing binary names `tools/do_vm.py test`.
3. **Tracked demo template (`support/demo/**`, invariant 2).** `snow.yml` (one build image, one
   component on internal `:8000`, `/` ingress, `pg_clusters[{id: psql, version: "18"}]`,
   `pg_databases[{id: demo, cluster: psql}]`, `modes.all` + `modes.local`), `Dockerfile`
   (`FROM python:3.12-slim AS app`), `app/server.py`, `db/0001_schema.sql`, `db/0002_seed.sql`,
   `slipfork.toml` (`project = "demo"`), port-less `README.md`. No `.gitignore` (the M3.26 `fork`
   hazard), no host/IP/key names, no secrets. `sync` copies it to `/home/slipfork/demo`,
   `chown -R slipfork:slipfork`, one commit with a preset identity
   (`Slipfork Demo <demo@slipfork.invalid>`), and stamps `~/demo/.git/slipfork-demo-stamp`. A later
   `sync` prints `unchanged`; a pre-existing directory **without** the stamp fails closed.
4. **Login banner (invariant 4).** Provisioning writes exactly one file,
   `/etc/profile.d/slipfork-demo.sh`; it prints for `slipfork` and `root` (no `getent passwd` +
   `sudo -u slipfork -i` archaeology) and names the account plus `cd ~/demo`, `slipfork`,
   `slipfork up`, `slipfork open`.
5. **No `getent passwd` anywhere (invariant 4).** Key installation takes `user:path` pairs
   (`_authorized_key_entries`), the account check is `id -u`, and every demo/home path is a harness
   constant. `test_no_getent_or_password_step_anywhere` pins it for every renderer.
6. **Provisioning writes no unit and no profile**, and it links the account's cache tree (`~/.cache`)
   at `/var/lib/slipfork/home/.cache`, verifying with `stat -f -c %t` that the link resolves to a
   Btrfs filesystem. A login-shell `slipfork` and the installed unit therefore name one state root
   (`<state_dir>/run/daemon.sock`) with **no** VM-only `--state-dir` on either side.
7. **Provision-time version skew (the real bug, attempt 2).** The first live `sync` failed closed
   inside the product verb with
   `cannot read the running slipforkd's ingress before installing: daemon control protocol error:
   missing field `lineage`` — a stale auto-spawned daemon from a previous build answered on the
   control socket, and `daemon install` inspects that daemon before writing anything, so re-runs
   could never converge. `render_daemon_handover` (embedded before `daemon install`) now owns it:
   `systemctl show --property=LoadState --value <unit>` decides per unit (`not-found` skipped, a
   known state is stopped, anything else fails closed); whatever `LISTEN`s on
   `<runtime_dir>/daemon.sock` is found **by socket ownership** (`ss -xlpnH`, matched on the
   configured path *and* its `readlink -f` form, so the resolved symlink target matches too, never a
   broad process-name match), `SIGTERM`ed, and awaited — bounded (30 × 1 s) — until no listener holds
   the path. "No listener" is the release condition `slipforkd` itself uses (`bind_private` replaces
   a leftover stale file); a surviving listener fails closed naming the path and pids. Live evidence:

   ```console
   $ ps -o pid,user,args -C slipforkd                # before
       PID USER      COMMAND
      1664 slipfork /usr/local/bin/slipforkd --listen 127.0.0.1:0 --state-dir /home/slipfork/.cache/slipfork --runtime-dir /home/slipfork/.cache/slipfork/run
   $ uv run --no-project tools/do_vm.py sync
   demo     unchanged /home/slipfork/demo (seeded once; owner edits are kept)
   slipfork: stopping the slipforkd holding /home/slipfork/.cache/slipfork/run/daemon.sock (pid 1664)
   unchanged slipforkd.socket, slipforkd.service, /etc/apparmor.d/slipfork; enabled
   synced 572 file(s) to /home/slipfork/slipfork on 165.232.86.174
   ```

8. **Banner quoting bug (attempt 1) fixed.** The body was wrapped in `_banner_body='…'` while itself
   containing `<<'SLIPFORK'`, so the quotes were eaten. Provisioning now writes it through a
   `<<'SLIPFORK_BANNER'` heredoc and compares with `cmp -s`; the live file carries `cat <<'SLIPFORK'`
   (§4).

## 2. Unit tests (`tools/tests/test_do_vm.py`)

`143 passed`. Coverage of the install path: the four-binary install + `cmp` idempotency (fake
`install`; second run performs zero installs), missing-binary / non-`active` socket /
unloaded-profile fail-closed paths, the four-line profile rendered by the built product for the
harness's own `--bin-dir` operands (and `render_apparmor_body`/`profile slipfork` asserted absent
from the tool source), demo seeding (one commit, preset identity, clean `git status`, owner edit
preserved on the second run, unstamped directory refused), the banner content and its single
`profile.d` file, the cache-symlink + Btrfs-verification step, `sync`/`test` step order
(demo → build → install), `sync` failing closed on a refused install, and the handover: units
stopped per `LoadState`, a real `LISTEN` holder `SIGTERM`ed and reaped by the loop, a
`SIGTERM`-ignoring holder failing closed after the bound, a non-`LISTEN` row ignored, `ss` failure
failing closed, and no socket at all meaning zero `ss`/`kill` work.

## 3. Verified `# Acceptance` (real transcripts)

```console
$ uv run --no-project pytest tools/tests -q
error: Failed to spawn: pytest
  cause: No such file or directory (os error 2)

$ uv run --no-project --with pytest pytest tools/tests -q
.......................................................................  [100%]
143 passed in 18.89s
```

(The literal command cannot spawn `pytest` in this environment — the same reason `make check`'s
`py-test` target uses `--with pytest`; that target passes, §8.)

```console
$ uv run --no-project ruff check tools
error: Failed to spawn: ruff
  cause: No such file or directory (os error 2)

$ uvx --offline ruff check tools && uvx --offline ruff format --check tools
All checks passed!
12 files already formatted

$ grep -q 'daemon install' tools/do_vm.py && echo product-install
product-install
$ ! grep -rn 'render_apparmor_body' tools/do_vm.py && echo single-renderer
single-renderer
$ test -f support/demo/snow.yml && test -f support/demo/Dockerfile && test -f support/demo/README.md && echo demo-tracked
demo-tracked
$ find support/demo -name .gitignore | wc -l
0
```

## 4. Live provisioning: the banner

`up` re-runs provisioning only when the readiness marker is absent
(`docs/design/do-vm-harness.md` §1 step 6/§3), so the documented marker-delete repair was used:

```console
$ uv run --no-project tools/do_vm.py ssh -- rm -f /var/lib/slipfork/.slipfork-ready
$ uv run --no-project tools/do_vm.py up --yes
… reusing droplet 620073485 (165.232.86.174) and volume 131a2765-… …
$ uv run --no-project tools/do_vm.py ssh -- cat /etc/profile.d/slipfork-demo.sh
# Slipfork dev-VM quickstart banner, written by `tools/do_vm.py` provisioning (harness-owned).
if [ -n "${BASH_VERSION:-}" ] && [ -t 1 ]; then
  _slipfork_account=$(id -un)
  if [ "$_slipfork_account" = "slipfork" ] || [ "$_slipfork_account" = "root" ]; then
    cat <<'SLIPFORK'

slipfork demo — account: slipfork (from root: sudo -iu slipfork)
  cd ~/demo
  slipfork
  slipfork up
  slipfork open
SLIPFORK
  fi
  unset _slipfork_account
fi
$ uv run --no-project tools/do_vm.py ssh -- bash -c \
    'script -qec "sudo -iu slipfork bash -lc true" /dev/null | sed -e "s/\r$//"'
slipfork demo — account: slipfork (from root: sudo -iu slipfork)
  cd ~/demo
  slipfork
  slipfork up
  slipfork open
```

(Printed twice because that probe runs two login shells — `sudo -i` and the explicit `bash -lc`; a
normal login prints it once.)

## 5. Live `sync` → `:80` → port-less origin (the acceptance that matters)

```console
$ uv run --no-project tools/do_vm.py sync            # bundle now carries AF_NETLINK
demo     unchanged /home/slipfork/demo (seeded once; owner edits are kept)
unchanged /etc/systemd/system/slipforkd.socket
wrote     /etc/systemd/system/slipforkd.service
unchanged /etc/apparmor.d/slipfork
enabled   slipforkd.socket + slipforkd.service as slipfork, ingress 127.0.0.1:80 + [::1]:80
next      slipfork up
synced 586 file(s) to /home/slipfork/slipfork on 165.232.86.174

$ uv run --no-project tools/do_vm.py ssh -- systemctl is-active slipforkd.socket
active

$ uv run --no-project tools/do_vm.py ssh -- grep RestrictAddressFamilies /etc/systemd/system/slipforkd.service
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX AF_NETLINK

$ uv run --no-project tools/do_vm.py ssh -- bash -c 'sha256sum /usr/local/bin/slipforkd \
    /home/slipfork/slipfork/target/debug/slipforkd'
2b76a29fe42f4110fd77a1bc8ec17d66d212202e7d0280ce7d1fcbce3ebee1e0  /usr/local/bin/slipforkd
2b76a29fe42f4110fd77a1bc8ec17d66d212202e7d0280ce7d1fcbce3ebee1e0  /home/slipfork/slipfork/target/debug/slipforkd

$ uv run --no-project tools/do_vm.py ssh -- bash -c '/usr/sbin/runuser -u slipfork -- env \
    HOME=/home/slipfork PATH=/usr/local/bin:/usr/bin:/bin bash -c \
    "cd /home/slipfork/demo && slipfork up"'
[resources] demo: prefer_remote -> local (not_offered)
[ready]     api (internal :8000 in world netns)
[origin]    http://demo.localhost

$ uv run --no-project tools/do_vm.py ssh -- curl -fsS -o /dev/null -w '%{http_code}\n' \
    -H 'Host: demo.localhost' http://127.0.0.1/
200

$ uv run --no-project tools/do_vm.py ssh -- curl -fsS -H 'Host: demo.localhost' http://127.0.0.1/
{"debug": true, "hello": "world", "host": "slipfork-dev-vm", "secret_len": 32, "world": "demo"}

$ uv run --no-project tools/do_vm.py ssh -- bash -c '/usr/sbin/runuser -u slipfork -- env \
    HOME=/home/slipfork PATH=/usr/local/bin:/usr/bin:/bin bash -c \
    "cd /home/slipfork/demo && slipfork list"'
WORLD            STATE     ORIGIN                                   WORKTREE
demo             running   http://demo.localhost                    /home/slipfork/demo

$ uv run --no-project tools/do_vm.py ssh -- bash -c "ss -ltnp | grep ':80 '"
LISTEN 0      1024       127.0.0.1:80         0.0.0.0:*    users:(("slipforkd",pid=31366,fd=3),("systemd",pid=1,fd=68))
LISTEN 0      1024           [::1]:80            [::]:*    users:(("slipforkd",pid=31366,fd=4),("systemd",pid=1,fd=69))
```

`:80` is bound by PID 1 and handed to the unprivileged `slipforkd` (`ADR-0011`); the origin carries
no port, and the body is served through it.

## 6. Second `sync`: zero unit rewrites, identical checksums

```console
$ uv run --no-project tools/do_vm.py ssh -- sha256sum /etc/systemd/system/slipforkd.socket \
    /etc/systemd/system/slipforkd.service /etc/apparmor.d/slipfork | tee /tmp/units-before.txt
1b8470e389d5cd466645a1d250ec87755a4d6faf90c80fa5b8b5b8cb9167637d  /etc/systemd/system/slipforkd.socket
45d71ce954ac73dfe7b356e428cf39547e467f0e575965a50cbcfb7ed2d20129  /etc/systemd/system/slipforkd.service
4e2af1463571ba73b3545c9f5f32b92750a45eab4132b9c458dbd0e91b8c7427  /etc/apparmor.d/slipfork

$ uv run --no-project tools/do_vm.py sync
demo     unchanged /home/slipfork/demo (seeded once; owner edits are kept)
unchanged slipforkd.socket, slipforkd.service, /etc/apparmor.d/slipfork; enabled
synced 586 file(s) to /home/slipfork/slipfork on 165.232.86.174

$ uv run --no-project tools/do_vm.py ssh -- bash -c \
    'sha256sum … | diff /tmp/units-before.txt - && echo identical'
identical

$ uv run --no-project tools/do_vm.py ssh -- bash -c '/usr/sbin/runuser -u slipfork -- env \
    HOME=/home/slipfork PATH=/usr/local/bin:/usr/bin:/bin bash -c \
    "cd /home/slipfork/demo && slipfork up"'
[ready]     api (internal :8000 in world netns)
[origin]    http://demo.localhost
$ uv run --no-project tools/do_vm.py ssh -- curl -fsS -o /dev/null -w '%{http_code}\n' \
    -H 'Host: demo.localhost' http://127.0.0.1/
200
```

Both runs leave `slipforkd.socket` `active`, rewrite zero unit bytes, and serve `http://demo.localhost`
(no port) — including after a `sync`-triggered daemon restart. The shipped installer's own second-run
line is the `unchanged slipforkd.socket, slipforkd.service, /etc/apparmor.d/slipfork; enabled` above
(`dx-0.1.0-ux.md` §2 renders it without an `[ok]` prefix; the harness captures the product's exact
line and the checksums as the zero-rewrite proof).

## 7. `do_vm.py test` — five live suites, `exit 0`

```console
$ uv run --no-project tools/do_vm.py test
…
     Running tests/critical_btrfs_live.rs (slipfork-engine)
test btrfs_kernel_ro_snapshot_two_clones_same_port_services_and_ipc_isolation ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 27.79s
     Running tests/critical_btrfs_live.rs (slipfork)
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 28.89s
     Running tests/critical_cli.rs
test result: ok. 21 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 120.46s
     Running tests/critical_slug.rs
test dag_plan_repo_flag_is_gone_and_rejected_by_the_parser ... ok
test git_submodule_bare_and_separate_git_dir_fail_closed_with_actionable_override_hints ... ok
test invalid_project_label_fails_closed_until_rescued_by_valid_slipfork_toml_override ... ok
test jj_workspace_uses_the_shared_git_resolver_and_never_shells_out_to_jj ... ok
test missing_snow_file_names_the_unreadable_path ... ok
test non_vcs_directory_fails_closed_and_corrupt_git_surfaces_real_git_stderr ... ok
test worktree_resolves_to_canonical_repo_slug_and_honors_only_vcs_root_slipfork_toml ... ok
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.96s
     Running tests/critical_ospp_daemon.rs
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 106.72s
$ echo $?
0
```

The two attempt-2 blockers are gone: the unit carries `AF_NETLINK`, and `critical_slug` is **7/7** on
the VM (M3.28.20). After the suite run the demo was re-`up`'d and the `:80` `200` re-verified (§5).

## 8. Local gates

```console
$ uv run --no-project tools/plan.py validate
plan OK
$ uv run --no-project tools/guard.py
guard: OK (586 file(s) checked)
$ make plan-check guard py-lint py-test support-check ts-test   # each target
plan-check: ok
guard: ok
py-lint: ok
py-test: ok
support-check: ok
ts-test: ok
$ make rs-lint && make rs-test && make docs-check
rs-lint: ok
rs-test: ok
docs-check: ok
check_docs_honesty: OK (10 published doc(s) match real CLI subcommands)

$ make check
…
Diff in /home/remy/offworld-nexus/slipfork/crates/slipfork-edge/tests/critical_edge.rs:3299:
-        families.split_whitespace().any(|family| family == "AF_NETLINK"),
+        families
+            .split_whitespace()
+            .any(|family| family == "AF_NETLINK"),
make: *** [Makefile:52: rs-fmt] Error 1
```

**`make check` exits 2 at `rs-fmt` on one file this node does not own:**
`crates/slipfork-edge/tests/critical_edge.rs` (M3.28.20's uncommitted edit is not `rustfmt`-clean).
`cargo fmt --all -- --check` reports exactly that one file and zero under `tools/` or `support/`:

```console
$ cargo fmt --all -- --check 2>&1 | grep '^Diff in' | sed 's/:[0-9]*:$//' | sort -u
Diff in /home/remy/offworld-nexus/slipfork/crates/slipfork-edge/tests/critical_edge.rs
```

`crates/**` is outside this node's paths, so it was not touched; `make check` will pass once
M3.28.20 runs `cargo fmt`.

## 9. Open follow-ups (reported, not edited)

1. **`docs/design/do-vm-harness.md` is stale** — §2/§3 say provisioning writes
   `/etc/apparmor.d/slipfork` (it no longer does; `slipfork daemon install` is the only writer), and
   §1's `sync`/`test` descriptions predate the build+install steps and the demo seed/skew handover.
   `docs/**` is outside this node's paths; needs an `M3.28.14`-owner rewrite.
2. **`crates/slipfork-edge/tests/critical_edge.rs` formatting** (M3.28.20) — the only `make check`
   failure, §8.
3. The pre-tracking demo was moved aside, not deleted: `/home/slipfork/demo.untracked-20261117`.
