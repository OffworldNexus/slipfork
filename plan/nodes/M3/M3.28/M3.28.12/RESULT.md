# M3.28.12 RESULT — VM Harness: `:80` via `daemon install`, Tracked `support/demo`, Login Banner

- **Status:** `blocked` — **only** on one gate that is not this node's file: `make check` stops at
  `rs-lint`/rustdoc inside `crates/slipfork/src/daemon_unit_install.rs` (M3.28.21's uncommitted
  `release_daemon` docs link a private item). Every other `make check` target and every
  `# Acceptance` command — including the live port-less `200` and all five live suites — is green
  (§6–§8).
- **Node:** `M3.28.12` (`release/0.1.0`, base `9e392f6`, attempt 4)
- **Scope (only):** `tools/do_vm.py`, `tools/tests/test_do_vm.py`, `tools/check_docs_honesty.py`,
  `support/demo/**` (new); `RESULT.md`
- **Authority:** `docs/design/dx-0.1.0-ux.md` §1/§2, `ADR-0011`
- **Droplet:** `165.232.86.174` (`slipfork-dev-vm`), volume `131a2765-…` (100 GiB Btrfs)

## 1. What changed in this attempt

1. **Zero vaporware in what the VM prints.** The login banner and the seeded
   `support/demo/README.md` now advertise only verbs the current binary lists: bare `slipfork`,
   `up`, `list`, `fork`, `drop`, `db`, `logs` (and `daemon install` in the harness docs). Every
   `open`/`restart`/`ps`/`exec`/`forward`/`prune`/`shell` mention and every invented transcript is
   gone; the README keeps only output that was actually captured (`curl … -w '%{http_code}\n'` →
   `200`).
2. **The gate cannot rot again.** `tools/check_docs_honesty.py` scans `support/demo/README.md`
   (`SEEDED_DOCS`) alongside the published pages, and
   `test_banner_and_demo_readme_only_advertise_installed_verbs` runs the built `slipfork --help`
   (reusing `extract_help_subcommands`, one source) and fails closed naming the offending verb:
   `… advertises \`slipfork <verb>\`, which \`slipfork --help\` does not list`. Negative proof:
   temporarily appending ``slipfork open`` made the gate exit 1 with
   `support/demo/README.md: published doc advertises unimplemented command 'slipfork open'`.
3. **`db` takes the database id.** The README documents `slipfork db demo` (the declared
   `pg_databases.<id>`), never the cluster id `psql`.
4. **Two-tier secrets.** `DEMO_SECRET !! random_hex(16)` moved from `modes.all` to `modes.local`;
   `modes.all` now carries only `DATABASE_URL` and `PGUSER`.
5. **The harness handover workaround is deleted.** M3.28.21 landed `release_daemon` in
   `slipfork daemon install` (stop the units, shut a typed holder down, ownership-prove any other
   holder, fail closed on an unprovable one), so `render_daemon_handover`, its `ss`/`kill` snippet,
   its six tests, and the `socket_wait_*`/`daemon_socket` config knobs are gone. `sync`/`test` now
   call the product verb and assert `systemctl is-active slipforkd.socket`.
6. **Surface text and disclosure.** The argparse description says `:80 socket activation` (not
   "AppArmor userns"), and `sync --help`/`test --help` state that both verbs stop the units and hand
   the control socket over, so running worlds' processes stop until the next `slipfork up`.
7. **Banner coherence.** The banner is still written once by provisioning, but it is a *guard*: it
   prints only for `slipfork`/`root` when `[ -d /home/slipfork/demo ]` and
   `command -v slipfork` both hold, so a host brought up by `up` alone never advertises a demo or a
   CLI that `sync` has not yet created.
8. **Build inputs are honest.** `PRODUCT_BINARIES` is now asserted against
   `cargo metadata --format-version 1 --no-deps` on the VM (one source of the target list), the
   build names those `--bin` targets explicitly, `BUILD_PROFILE` drives both the build flag and the
   installed `target/<profile>` path, and a metadata/shipped-set disagreement fails closed
   (`cargo metadata disagrees with the shipped binary set: missing [git-remote-slipfork], …`).
9. **Defensive quoting.** Every config path interpolated into a remote shell command goes through
   `shlex.quote` (`HOME=…`, `runuser -u …`, `cp -a …`, `install …`, `--bin-dir …`, `_source=…`);
   the authorized-keys step now passes `user path` as positional words instead of a `user:path`
   token. Rendered bytes for the default paths are unchanged (verified by a before/after diff).
10. **Test fixtures deduped.** One `_fake_bin` builder and one `systemctl` fake (handles both
    `show`/`is-active`) serve every rendered-snippet test; the repeated `getent`/`passwd`
    negations live only in `test_no_getent_or_password_step_anywhere`.

## 2. Unit tests (`tools/tests/test_do_vm.py`)

```console
$ uv run --no-project --with pytest pytest tools/tests -q
.......................................................................  [100%]
141 passed in 18.43s
```

(The literal `uv run --no-project pytest` cannot spawn `pytest` in this environment — recorded
below. `make check`'s own `py-test` target uses `--with pytest` and passes.)

## 3. Verified `# Acceptance`

```console
$ uv run --no-project pytest tools/tests -q
error: Failed to spawn: pytest
  cause: No such file or directory (os error 2)

$ uvx --offline ruff check tools && uvx --offline ruff format --check tools
All checks passed!
12 files already formatted

$ uv run --no-project tools/check_docs_honesty.py
check_docs_honesty: OK (11 doc(s) match real CLI subcommands: published pages plus support/demo/README.md)

$ grep -q 'daemon install' tools/do_vm.py && echo product-install
product-install
$ ! grep -rn 'render_apparmor_body' tools/do_vm.py && echo single-renderer
single-renderer
$ test -f support/demo/snow.yml && test -f support/demo/Dockerfile && test -f support/demo/README.md && echo demo-tracked
demo-tracked
$ find support/demo -name .gitignore | wc -l
0
```

Negative proof for the new doc gate (appended `slipfork open` to the demo README, then reverted):

```console
$ uv run --no-project tools/check_docs_honesty.py
check_docs_honesty: FAILED (1 vaporware reference(s)):
  X support/demo/README.md: published doc advertises unimplemented command 'slipfork open' (shipped commands: … open absent …)
exit=1
```

## 4. Live provisioning: the banner (guarded, shipped verbs)

```console
$ uv run --no-project tools/do_vm.py ssh -- rm -f /var/lib/slipfork/.slipfork-ready
$ uv run --no-project tools/do_vm.py up --yes
… reusing droplet 620073485 (165.232.86.174) and volume 131a2765-… …
$ uv run --no-project tools/do_vm.py ssh -- cat /etc/profile.d/slipfork-demo.sh
# Slipfork dev-VM quickstart banner, written by `tools/do_vm.py` provisioning (harness-owned).
if [ -n "${BASH_VERSION:-}" ] && [ -t 1 ]; then
  _slipfork_account=$(id -un)
  if { [ "$_slipfork_account" = "slipfork" ] ||
       [ "$_slipfork_account" = "root" ]; } &&
     [ -d /home/slipfork/demo ] && command -v slipfork >/dev/null 2>&1; then
    cat <<'SLIPFORK'

Slipfork demo — account: slipfork (from root: sudo -iu slipfork)
  cd ~/demo
  slipfork
  slipfork up
  slipfork list
SLIPFORK
  fi
  unset _slipfork_account
fi

$ uv run --no-project tools/do_vm.py ssh -- bash -c \
    'script -qec "sudo -iu slipfork bash -lc true" /dev/null | sed -e "s/\r$//"'

Slipfork demo — account: slipfork (from root: sudo -iu slipfork)
  cd ~/demo
  slipfork
  slipfork up
  slipfork list
```

## 5. Seeded demo on the VM (re-seeded with the corrected template)

The previous seed was clean (no owner edits, `git status --short` empty), and the template changed,
so it was removed and re-seeded by `sync`:

```console
$ uv run --no-project tools/do_vm.py ssh -- bash -c 'cd /home/slipfork/demo && git log --oneline; git status --short; grep -n "slipfork " README.md; grep -c "slipfork open\|slipfork restart" README.md'
9a95661 Slipfork 0.1.0 demo (seeded by tools/do_vm.py sync)
15:$ slipfork                 # daemon line, worlds, and the next command per world
16:$ slipfork up              # boots the world and prints its port-less origin
19:$ slipfork list            # world ids, states, and one origin each
22:If nothing answers on the daemon's control socket yet, `slipfork up` is the verb that starts it
28:$ slipfork fork demo-a     # `.worktrees/demo-a` plus its own Btrfs subvolume and origin
29:$ slipfork drop demo-a     # stops it and removes the worktree
38:$ slipfork db demo         # psql 18 inside the world's postgres container (needs a TTY)
39:$ slipfork logs api        # retained component output (the API logs its world and db readiness)
0
$ … sed -n '/^modes:/,$p' snow.yml
modes:
  all:
    envs:
      app:
        DATABASE_URL: '…pg_databases.demo.url…'
        PGUSER: postgres
  local:
    envs:
      app:
        DEMO_SECRET: '…secrets::DEMO_SECRET !! random_hex(16)…'
        DEBUG: 'True'
```

## 6. Live `sync` → `:80` → port-less origin

```console
$ uv run --no-project tools/do_vm.py sync
demo     unchanged /home/slipfork/demo (seeded once; owner edits are kept)
unchanged slipforkd.socket, slipforkd.service, /etc/apparmor.d/slipfork; enabled
synced 589 file(s) to /home/slipfork/slipfork on 165.232.86.174

$ uv run --no-project tools/do_vm.py ssh -- systemctl is-active slipforkd.socket
active

$ uv run --no-project tools/do_vm.py ssh -- bash -c '/usr/sbin/runuser -u slipfork -- env \
    HOME=/home/slipfork PATH=/usr/local/bin:/usr/bin:/bin bash -c \
    "cd /home/slipfork/demo && slipfork up"'
[resources] demo: prefer_remote -> local (not_offered)
[ready]     api (internal :8000 in world netns)
[origin]    http://demo.localhost

$ uv run --no-project tools/do_vm.py ssh -- curl -fsS -o /dev/null -w '%{http_code}\n' \
    -H 'Host: demo.localhost' http://127.0.0.1/
200

$ uv run --no-project tools/do_vm.py ssh -- bash -c '/usr/sbin/runuser -u slipfork -- env \
    HOME=/home/slipfork PATH=/usr/local/bin:/usr/bin:/bin bash -c \
    "cd /home/slipfork/demo && slipfork list"'
WORLD            STATE     ORIGIN                                   WORKTREE
demo             running   http://demo.localhost                    /home/slipfork/demo
```

Second `sync` — the product's handover (no harness `ss`/`kill`), zero unit rewrites:

```console
$ uv run --no-project tools/do_vm.py ssh -- sha256sum /etc/systemd/system/slipforkd.socket \
    /etc/systemd/system/slipforkd.service /etc/apparmor.d/slipfork
1b8470e389d5cd466645a1d250ec87755a4d6faf90c80fa5b8b5b8cb9167637d  /etc/systemd/system/slipforkd.socket
45d71ce954ac73dfe7b356e428cf39547e467f0e575965a50cbcfb7ed2d20129  /etc/systemd/system/slipforkd.service
4e2af1463571ba73b3545c9f5f32b92750a45eab4132b9c458dbd0e91b8c7427  /etc/apparmor.d/slipfork

$ uv run --no-project tools/do_vm.py sync
demo     unchanged /home/slipfork/demo (seeded once; owner edits are kept)
unchanged slipforkd.socket, slipforkd.service, /etc/apparmor.d/slipfork; enabled
synced 589 file(s) to /home/slipfork/slipfork on 165.232.86.174

$ uv run --no-project tools/do_vm.py ssh -- bash -c \
    'sha256sum … | diff /var/tmp/units-before.txt - && echo identical-after-second-sync; systemctl is-active slipforkd.socket'
identical-after-second-sync
active

$ … slipfork up   →  [origin]    http://demo.localhost
$ … curl -H 'Host: demo.localhost' http://127.0.0.1/ -w '%{http_code}\n'
200
```

## 7. `do_vm.py test` — five live suites, `exit 0`

```console
$ uv run --no-project tools/do_vm.py test
test btrfs_kernel_ro_snapshot_two_clones_same_port_services_and_ipc_isolation ... ok
test result: ok. 1 passed; 0 failed; … finished in 32.09s          # slipfork-engine live
test result: ok. 1 passed; 0 failed; … finished in 24.86s          # slipfork live
test result: ok. 21 passed; 0 failed; … finished in 131.07s        # critical_cli
test result: ok. 7 passed; 0 failed; … finished in 3.21s           # critical_slug
test result: ok. 4 passed; 0 failed; … finished in 95.43s          # critical_ospp_daemon
$ echo $?
0
```

After the suite run the demo was re-`up`'d and the port-less `200` re-verified (§6 transcript).

## 8. Local gates

```console
$ uv run --no-project tools/plan.py validate
plan OK
$ uv run --no-project tools/guard.py
guard: OK (589 file(s) checked)
$ make plan-check guard py-lint py-test support-check ts-test rs-fmt   # each target
plan-check: ok
guard: ok
py-lint: ok
py-test: ok
support-check: ok
ts-test: ok
rs-fmt: ok
$ make rs-test && make docs-check
rs-test: ok
docs-check: ok
check_docs_honesty: OK (11 doc(s) match real CLI subcommands: published pages plus support/demo/README.md)

$ make check
…
error: public documentation for `release_daemon` links to private item `HANDOVER_GRACE`
error: could not document `slipfork`
make: *** [Makefile:56: rs-lint] Error 101
```

**`make check` is red on one item this node does not own:** `crates/slipfork/src/daemon_unit_install.rs`
(M3.28.21's uncommitted `release_daemon` doc comment links the private `HANDOVER_GRACE`).
`crates/**` is outside this node's paths, so it was not touched; `make check` passes once M3.28.21
makes that intra-doc link code-spanned (or public).

Also seen mid-attempt (transient, not mine): while `.21` was being written, `sync`'s build failed
with `no method named 'status' found for struct 'DaemonControlClient'` /
`use slipfork_edge::DaemonControl` in the same file, and the same `cargo check` later passed
unchanged. No harness change was made in response; the recorded failure is that transient compile
state.

## 9. Open follow-ups (reported, not edited)

1. **`make check` red at `rs-lint`**: M3.28.21's `daemon_unit_install.rs` rustdoc private-item link
   (§8) — one-line fix in that node's file.
2. **`docs/design/do-vm-harness.md` is stale** (§2/§3 still describe `render_apparmor_body` and the
   old `sync`/`test` steps; the harness no longer writes a profile or a daemon lifecycle).
   `docs/**` is outside this node's paths — assigned to `M3.28.14`.
3. The pre-tracking demo was moved aside, not deleted: `/home/slipfork/demo.untracked-20261117`.
