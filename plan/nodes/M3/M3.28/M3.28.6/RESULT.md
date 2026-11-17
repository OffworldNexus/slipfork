# RESULT M3.28.6 — `slipfork daemon install`, `:80` socket activation, AppArmor profile

## What shipped

- `daemon install [--user <ACCOUNT>] [--bin-dir <DIR>]… [--forward-port <PORT>]`: writes
  `/etc/systemd/system/slipforkd.{socket,service}` + `/etc/apparmor.d/slipfork` with the exact
  `slipfork daemon unit` bytes, then `daemon-reload`, `apparmor_parser -r`, releases any
  zero-setup `slipforkd` still holding `daemon.sock` (typed `Shutdown` + bounded await), and
  `systemctl enable --now slipforkd.socket slipforkd.service`. Idempotent (byte equality; a second
  run performs zero writes).
- **AppArmor injection is closed:** `validate_bin_dir` rejects non-absolute/empty paths and `%`,
  whitespace/control, `{`, `}`, `,`, `"`, `\`; `render_userns_profile` now returns
  `Result<_, String>` and is shared by `daemon unit` and `daemon install`. The refusal is one
  escaped line; `daemon unit --bin-dir $'/tmp/a\nprofile x,'` exits non-zero and writes nothing.
- **One account/spec derivation:** `target_account(identity, is_root, explicit_user)` +
  `ingress_unit_spec(account, layout, exe, port)` are shared by `daemon unit` (new `--user` flag)
  and `daemon install`; edge exports `INGRESS_ADDRS`/`INGRESS_PORT`. A real `critical_cli` test
  compares `install`'s rendered bytes to a **real `daemon unit --user …` subprocess**.
- **Handover:** `DaemonRequest::Shutdown` / `DaemonReply::Shutdown` / `DaemonControl::shutdown`
  (default fails closed for backends that cannot stop their transport) + client/server wiring;
  the server releases `daemon.sock` and exits when acknowledged. `install` calls it after writing
  and before `enable --now`, with a 15 s bounded await that names
  `sudo systemctl start slipforkd.socket` on timeout.
- `doctor` has four typed ingress states: `ActivePort80`, `ActiveOtherListeners` (active but not
  on `:80`, honest `daemon install` hint), `Inactive`, `NotInstalled`; text + `--json`; `ok` and
  `fixes` unaffected. `ADR-0011` §3 updated.
- `--runtime-dir` deleted from the CLI global options (client always `<state_dir>/run`); only
  `up`/`fork` auto-spawn, everything else prints one line. `slipforkd.service` gained
  `Requires=`/`After=slipforkd.socket` and `[Install] WantedBy=multi-user.target`; golden updated.
- Cruft pass: `Layout::for_account` dropped its unused runtime override; `InstallReport` renders
  its rows once; `SystemdIngressProbe::new` removed; `connect_or_spawn` reuses `served_client`;
  `write_file_atomic` fsyncs the parent directory after `rename`; module docs + `cli.md` state
  that a mid-sequence failure converges by re-running.
- `docs/reference/cli.md`: `daemon install`, repeatable `daemon unit --bin-dir`, `daemon unit
  --user`, no global `--runtime-dir`, no `| sudo tee` / manual `apparmor_parser` recipe, ingress
  sample matches the binary (``run `slipfork daemon install` ``, no `sudo` inside output).

## Round-4 close-out (scope widening accepted)

- `docs/reference/cli.md` documents all four ingress statuses and their real shapes
  (`active_other_listeners` → `port: null`, `listen: [...] | null`, non-null `hint`).
- `critical_cli`'s doctor allowlist accepts `active_other_listeners`.
- **XDG mismatch closed:** `InstallRequest`/`daemon unit` now take the client's
  `$XDG_CACHE_HOME`/`$XDG_DATA_HOME` (bundled as `ClientRoots`) and pass them through
  `Layout::for_account`, so the installed unit and the client derive one state/runtime root.
  New unit test pins `--state-dir /xdg/cache/slipfork --runtime-dir /xdg/cache/slipfork/run`.
- `UnitSpec::validate` rejects `%` in `ExecStart`, `--state-dir`, and `--runtime-dir`
  (`--state-dir '/tmp/%h/state'` → one line, exit 1); new unit test covers all three roles.
- `systemctl show -p Listen` failing or returning nothing now yields
  `ActiveOtherListeners { listen: None }` with a distinct “cannot confirm :80” hint; it never
  claims it read the listener set. Tested for `Some([...])` and `None`.
- Every mid-sequence installer effect error now ends with `— re-run to converge`.
- `critical_edge.rs` was restored; the `DaemonControl::shutdown` default fails closed for
  backends that cannot stop their transport, so no unauthorized protected test change remains.
- Engine remediation hint still out of paths (disclosed below).

## Acceptance (exact commands and outputs)

```console
$ cargo test -p slipfork --lib
test result: ok. 61 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.47s

$ cargo test -p slipfork --test critical_cli -- --test-threads=1
test result: ok. 19 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 62.33s

$ cargo run -q -p slipfork -- daemon unit --bin-dir /usr/local/bin --bin-dir /tmp/ux/bin --only apparmor | grep -c slipfork
1

$ [ "$(cargo run -q -p slipfork -- daemon unit --bin-dir /usr/local/bin --only apparmor | wc -l)" -eq 4 ] && echo four-lines
four-lines

$ cargo run -q -p slipfork -- --json doctor | grep -q '"ingress"' && echo ingress-in-json
ingress-in-json

$ ! grep -rn 'sudo tee' docs/reference/cli.md && echo recipe-gone
recipe-gone

$ cargo clippy --workspace --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.12s
```

## Additional verification

```console
$ cargo fmt --all -- --check
FMT-OK

$ cargo test -p slipfork-edge --test critical_edge
test result: ok. 40 passed; 0 failed ...

$ uv run --no-project tools/critical_guard.py --base 501acf6 --node M3.28.6
critical-guard: OK: authorized by M3.28.6

$ uv run --no-project tools/guard.py
guard: OK (563 file(s) checked)

$ uv run --no-project tools/plan.py validate
plan OK

$ make check
... slipfork --lib: 61 passed ...
... critical_cli: 19 passed ...
... critical_slug: 7 passed ... critical_ospp_daemon: 4 passed ...
... critical_btrfs_live: 1 passed ...
uvx --offline zensical build --strict -> Build finished in 0.27s
check_docs_honesty: OK (10 published doc(s) match real CLI subcommands)
EXIT=0
```

One initial `critical_cli` run hit the known
`world init … survived SIGTERM and SIGKILL for 15s` host flake (`M3.27` owns it); the immediate
re-run passed 19/19 and the full `make check` run passed.

## Root-only effects (not executed on this host)

A real `sudo slipfork daemon install` was **not** run here (it would write `/etc`, load an
AppArmor profile, and `systemctl enable --now` system units). Proven with the recording
`UnitInstaller` unit tests and the real-preview comparison:

- non-root → exact one-line remedy, zero reads/writes/unit calls; direct root / `--user root` /
  unknown account fail closed with zero effects.
- first run writes byte-identical socket/service/AppArmor bytes and calls `daemon-reload`,
  `apparmor_parser -r`, `release_running_daemon`, and exactly one `enable --now` in that order.
- identical bytes → zero writes, one `enable --now`; a changed, already-installed service →
  `restart_service`.
- `daemon_install_bytes_equal_a_real_daemon_unit_preview` (critical_cli) compares the install
  bytes to a real `slipfork daemon unit --user <account> --only …` subprocess.
- `daemon_unit_refuses_an_apparmor_injecting_bin_dir_without_writing` proves the newline/`,`/brace
  injection is rejected in one escaped line with no output and no state directory.

## Out-of-scope mechanical test edits (disclosed)

`--runtime-dir` removal and `<state>/run` changed test scaffolding; no assertions were weakened:

- `crates/slipfork/tests/critical_slug.rs` — dropped the deleted argv, repointed the poisoned
  artifact cache and vault path at `DaemonLayout::under(scratch)`.
- `crates/slipfork/tests/critical_vcs_remote.rs` — `XDG_CACHE_HOME` (state parent) replaces
  `XDG_RUNTIME_DIR` for `git-remote-slipfork`.
- `crates/slipfork/tests/critical_ospp_daemon.rs` — fake `tpmd` socket moved under the daemon's
  real `XDG_RUNTIME_DIR` (`DaemonLayout::under(scratch).runtime_dir.parent()`).

## Not in this node's paths

`crates/slipfork-engine/src/lib.rs` still says `sudo apparmor_parser -r /etc/apparmor.d/slipfork`
in its userns remediation hint; it should point at `sudo slipfork daemon install`. The engine
crate is not authorized for M3.28.6 — left for a later node.

No commit was made; `plan/` other than this `RESULT.md` was not edited.

STATUS: done
NODE: M3.28.6
BASE: 501acf6
FILES: crates/slipfork/src/{main.rs,daemon_unit_install.rs,cli.rs,apparmor.rs,user.rs,doctor.rs,lib.rs,layout.rs,vaults.rs,daemon.rs,bin/slipforkd.rs,bin/git-remote-slipfork.rs}, crates/slipfork/tests/{critical_cli.rs,critical_slug.rs,critical_vcs_remote.rs,critical_ospp_daemon.rs,support/mod.rs}, crates/slipfork-edge/src/{unit.rs,lib.rs,daemon.rs,daemon/client.rs,daemon/server.rs}, crates/slipfork-edge/tests/golden/slipforkd.service, docs/reference/cli.md, docs/decisions/ADR-0011-port-80-daemon-install-and-runtime-dir.md, plan/nodes/M3/M3.28/M3.28.6/RESULT.md
VERIFIED: fmt --check; lib 61; critical_cli 19; critical_edge 40; critical_slug 7; critical_ospp_daemon 4; all NODE cargo-run checks; clippy -D warnings; critical_guard --base 501acf6 --node M3.28.6; guard.py; plan.py validate; make check exit 0
NOTES: root-only install proven via recording installer + real `daemon unit --user` preview; injection test added; engine remediation hint left for a later node (not in paths)
