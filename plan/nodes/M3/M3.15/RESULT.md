# Result M3.15 — `slipforkd` Btrfs composition root, pre-daemon `slipfork doctor`, `daemon unit --only apparmor`, and the CLI-critical harness rewire

Base: `3bc93e3` (`release/0.1.0`). Node: `plan/nodes/M3/M3.15/NODE.md`.
Lead amendment honoured: `WorldId` / `resolve_world_selector` are **deferred to 0.2.0**; the shipped
`--world <fork>` semantics are untouched (no `WorldId`, no qualified-id parsing in this node).

## 1. Wiring map (Composition Root)

| Concern | Owner after this node |
|---|---|
| `KernelBtrfsDriver` construction | `crates/slipfork/src/main.rs` (`doctor`) **and** `crates/slipfork/src/bin/slipforkd.rs` (engine) — the two composition roots only |
| `BtrfsEngine::with_driver(..., Arc::new(KernelBtrfsDriver))` | `crates/slipfork/src/bin/slipforkd.rs` over the explicit, canonicalized `--state-dir` |
| `OsHostProbe` construction | `crates/slipfork/src/main.rs` (`OsHostProbe::new(binary_dir())`); the adapter lives in `crates/slipfork::doctor` |
| `doctor` → engine | none: `doctor::run(&dyn SubvolumeDriver, &dyn HostProbe, …)` holds **zero** `WorldEngine` |
| `incus` / `ReflinkNetnsEngine` in the CLI, daemon, and their suites | deleted (all of them) |
| Base-seed sealing | `DaemonService::ensure_base_snapshot` treats `EngineError::SnapshotExists` as “already sealed → clone it”, for both the `<project>-seed-empty` snapshot and the `<project>-base-<sha12>` seal (never re-sealed) |
| `--host-caches` resolution | single `slipfork::daemon::host_caches(home)` helper shared by `slipforkd` and the in-process test composition root |

`slipforkd` now resolves `--state-dir`/`--runtime-dir` once (`ensure_private_dir` + `fs::canonicalize`)
*before* `Layout::resolve`, so the Btrfs engine never sees a relative path. `BtrfsEngine::with_driver`
runs the magic-only `ensure_supported` gate at boot: a non-Btrfs state root exits 1 with
`EngineError::BtrfsRequired` (statfs code + `BTRFS_SUPER_MAGIC` + the `user_subvol_rm_allowed,noatime`
hint) before any `worlds/` or `snapshots/` directory exists.

## 2. `slipfork doctor` seam and report shape

New `crates/slipfork/src/doctor.rs` (`#![forbid(unsafe_code)]`, `ARCHITECTURE.md` §3.3 / `ADR-0010` §4):

- `HostCheckOutcome { Ok | Failed { detail, hint } }` (two variants only), `NetnsCheck { Executed(..) | Skipped }`
  (`Skipped` synthesized **only** by `doctor::run` when `unshare_userns != Ok`), `DoctorReport`,
  `trait HostProbe { probe_userns, probe_netns }`, `OsHostProbe`, `pub fn run(driver, host, state_dir, uid, gid)`.
- `OsHostProbe` runs `unshare --user --map-user=<uid> --map-group=<gid> -- true` and
  `… --keep-caps --net --ipc --pid --fork -- ip link set lo up` with `env_clear()` +
  fixed `PATH` + `LC_ALL=C` + `LANGUAGE=C`; an `Operation not permitted` stderr feeds
  `slipfork_engine::userns_remediation_hint(bin_dir)`, a missing `ip` feeds the `iproute2` hint.
- `filesystem_name` maps `0x9123683e→btrfs`, `0x58465342→xfs`, `0xef53→ext4`, `0x01021994→tmpfs`,
  `0x794c7630→overlayfs`, else `unknown(0x<hex>)`.
- Human text (this XFS host, verbatim):
  ```
  state_dir: /home/remy/.cache/slipfork
  filesystem: xfs
  btrfs_super_magic: mismatch
  subvolume_ioctl: skipped
  noatime: missing
  unshare_userns: ok
  netns: ok
  fix: sudo mount -o user_subvol_rm_allowed,noatime /dev/<device> <state_dir>
  ```
  exit **1**; `doctor` created nothing (the pre-existing state root is untouched).
- `--json` projection is exactly `ADR-0010` §4: flattened `state_dir`, `probed_path`, `fstype`
  (`"0x58465342"`), `btrfs_super_magic`, `noatime`, `filesystem`, `uid`, `gid`, `ok`,
  `subvolume_ioctl {status[,errno,hint]}`, `unshare_userns {status[,detail,hint]}`,
  `netns {status[,detail,hint]}`, `fixes` (order `btrfs → unshare_userns → netns`).
- **No `crun` probe and no `crun` field** (`ADR-0009` §1 / `ADR-0010` §4).
- `noatime` is advisory: it never feeds `ok` or `fixes`, so `fixes.is_empty() ⇔ ok` holds.

Unit tests in `doctor.rs` cover non-Btrfs `None` ioctl + mount hint + “creates nothing”, the Btrfs
success path, `Skipped` synthesis, deterministic fix order, the JSON keys, the failed-errno branch,
and the filesystem-name table, using `slipfork_testkit::btrfs::TestTreeDriver` and a local
two-variant `FakeHostProbe`.

## 3. AppArmor `userns` profile

- `crates/slipfork/src/apparmor.rs`: `render_userns_profile(bin_dir)` renders the exact 4 lines of
  `ADR-0010` §7 (`abi <abi/4.0>,` / `profile slipfork <bin_dir>/slipfork{,d} flags=(unconfined) {` /
  `  userns,` / `}`), no file or mount rules.
- `slipfork daemon unit --only socket|service|apparmor` (`UnitKind::Apparmor`); with no `--only` all
  three render, and `--json` carries `socket`/`service`/`apparmor` (`null` for the ones not asked for).
- `slipfork_engine::userns_remediation_hint` now names `slipfork daemon unit --only apparmor` plus the
  `apparmor_parser -r` reload (the engine-side re-add this node owed).

## 4. Test harness change (`crates/slipfork/tests/support/mod.rs`)

- `Daemon` now has two honest backends: the real `CARGO_BIN_EXE_slipforkd` child on a Btrfs host, and
  an **in-process** `slipfork::daemon` on any other host, composed exactly like `slipforkd`
  (`Layout::resolve`, `runtime_assets`, `DockerfileStageBuilder`, `FileVaultProvider` coordinates,
  `FallbackDeviceKeySigner` over the software key, `RepositoryWorkspaces`, `LayoutProjectVaults`,
  `DiskSeedDumpStore`, `WorldRegistry`, `UnavailableTunnelRegistrar`) with
  `BtrfsEngine::with_driver(Arc::new(TestTreeDriver::btrfs()))`, a real bound
  `<state_dir>/http-ingress.sock` (`0600`), and `DaemonControlServer::bind` + accept thread on
  `<runtime_dir>/daemon.sock`. The real CLI / `git-remote-slipfork` binaries still drive it.
- `Daemon::start_in(scratch, DaemonLayout)` allows an explicit state/runtime/data triple;
  `Daemon::try_start_with_path` keeps the real child (PATH isolation for the `.jj` adapter);
  `Daemon::shutdown` kills the child or unlinks the in-process socket; `Daemon::keep_daemon` keeps a
  daemon alive for the process lifetime.
- `critical_slug.rs` (which never used `Daemon`) now ensures the in-process daemon for its scratch
  on a non-Btrfs host so `snow resolve` still travels through the typed socket; the
  auto-spawn-teardown assertion is Btrfs-only (there is no child to reap elsewhere).
- `critical_ospp_daemon.rs`'s `.jj` routing test keeps the real child (`try_start_with_path`) and
  asserts the fail-closed `BtrfsRequired` + hint on a non-Btrfs host; `critical_vcs_remote.rs` uses
  `Daemon::shutdown()` instead of touching `daemon.child`.
- `critical_cli.rs`'s zero-setup test branches: on Btrfs it proves auto-spawn + `0600` socket +
  SIGTERM teardown; off Btrfs it asserts the CLI surfaces the refused `slipforkd` and the daemon log
  names the exact mount hint, binding no socket.

## 5. New live suite `crates/slipfork/tests/critical_btrfs_live.rs`

Spawns the real `CARGO_BIN_EXE_slipforkd` (no `#[ignore]`, no silent skip):

- **Non-Btrfs host:** exit 1, stderr names `is not on Btrfs`, `0x9123683e`, and
  `mount -o user_subvol_rm_allowed,noatime`; no `worlds/`, `snapshots/`, or `daemon.sock`; no
  "reflink" fallback wording; one diagnostic line that the live path runs on the Btrfs VM.
- **Btrfs host:** the full production path — fixture `snow.yml` with a `postgres:18` `pg_cluster` +
  `pg_database`, `up --no-dump`, one origin URL, a sealed `<project>-base-<sha12>` snapshot, `exec`,
  `list --json`, `down`.

## 6. Deferred / residual

- **`WorldId` / `resolve_world_selector` / qualified `--world`**: deferred to 0.2.0 by the lead
  amendment; `--world <fork>` is unchanged and `main.rs` keeps `raw world + cwd` on the wire.
- **`make rs-test` Makefile list (`WORLD_BOOTING_TESTS`)**: `tools/rs_test_list.py --world-booting`
  fails because the two new `critical_btrfs_live` integration binaries (this node's
  `slipfork:critical_btrfs_live` and M3.14's `slipfork-engine:critical_btrfs_live`) are not yet
  listed in `Makefile`. `Makefile` is outside this node's `paths`; **`lead` must add**
  `slipfork-engine:critical_btrfs_live` and `slipfork:critical_btrfs_live` to `WORLD_BOOTING_TESTS`
  and the two `cargo test -p … --test critical_btrfs_live -- --test-threads=1` lines.
  **Resolved in §9** (lead-widened scope).
- **AppArmor byte-equivalence**: see §7 — the CLI profile and the VM harness profile cannot be
  byte-identical because the harness must attach `/usr/bin/unshare` + `slipfork-world-init` (its
  `cargo test` executable is unlinked), while the product profile attaches `<bin_dir>/slipfork{,d}`.
  The new `tools/tests/test_do_vm.py` test therefore compares the full profile **byte-for-byte after
  normalising the single attachment expression**, and asserts the CLI attachment is exactly
  `<bin_dir>/slipfork{,d}`.
- **`packages/opencode-slipfork` live e2e (out of this node's paths, `M3.20` owns the package)**:
  `bun test packages/opencode-slipfork/tests` now fails its live-world case on this XFS host because
  the plugin's e2e boots a world through the CLI's auto-spawned `slipforkd`, which correctly exits 1
  with `BtrfsRequired` (see §7 #15). `lead` must route the same dual-mode treatment (or a Btrfs-VM
  guarantee) to `M3.20`/`M3.19`; nothing in this node can weaken the Btrfs gate to fix it.
  **Resolved in §9** (lead-widened scope): the e2e is now dual-mode and asserts the fail-closed
  contract on this host.
- **Environment-conditional test coverage (documented, never silent):** the `.jj` routing positive
  case and the missing-`git` PATH-isolation assertion keep the **real child** daemon (the VCS
  adapters probe the ambient `PATH`, which an in-process daemon cannot isolate). On a non-Btrfs host
  they assert the production daemon's fail-closed `BtrfsRequired` + mount hint and print one
  diagnostic line; the routing/PATH assertions run on the Btrfs VM (`tools/do_vm.py`, whose
  `SUITE_HOME=/var/lib/slipfork/home` is on the Btrfs volume).
- `cargo fmt --all -- --check` is **green** (exit 0); every file this node touched is
  `cargo fmt`-clean.

## 7. Acceptance evidence (exact commands, this XFS host, `release/0.1.0`)

| # | Command | Result |
|---|---|---|
| 1 | `cargo test -p slipfork --all-targets` | **exit 0** — lib 28, `critical_btrfs_live` 1, `critical_cli` 14, `critical_l0_wiring` 4, `critical_ospp_daemon` 4, `critical_slug` 6, `critical_vcs_remote` 1; 0 failed, 0 ignored |
| 2 | `cargo clippy -p slipfork --all-targets -- -D warnings` | **exit 0** (`Finished dev profile …`) |
| 3 | `! grep -rni 'incus' crates/slipfork/src/ crates/slipfork/tests/` | **exit 0** (no matches) |
| 4 | `cargo run -q -p slipfork -- daemon unit --only apparmor \| grep -n 'userns'` | `3:  userns,` |
| 5 | `cargo run -q -p slipfork -- doctor; test $? -eq 1` | **exit 1** (asserted by `test $? -eq 1`) |
| 6 | `cargo run -q -p slipfork -- doctor 2>&1 \| grep -n 'user_subvol_rm_allowed,noatime'` | `8:fix: sudo mount -o user_subvol_rm_allowed,noatime /dev/<device> <state_dir>` |
| 7 | `grep -n 'btrfs_super_magic' crates/slipfork/src/main.rs` | `181:/// \`btrfs_super_magic: ok\|mismatch\`, …` |
| 8 | `make rs-test` | **exit 2**, verbatim: `rs-test-list: integration test binaries missing from rs-test: slipfork-engine:critical_btrfs_live slipfork:critical_btrfs_live` / `make: *** [Makefile:73: rs-test-list] Error 1` — Makefile entries out of scope (§6) |
| 9 | `uv run --no-project --with pytest pytest -q tools/tests` | `113 passed` (includes the new AppArmor equivalence test) |
| 10 | `cargo fmt --all -- --check` | **exit 0** |
| 11 | `uvx --offline ruff check tools poc` + `ruff format --check tools poc` | `All checks passed!` / `13 files already formatted` |
| 12 | `uv run --no-project tools/guard.py` | `guard: OK (498 file(s) checked)` |
| 13 | `uv run --no-project tools/plan.py validate` | `plan OK` |
| 14 | `make check` (default `PATH`) | **exit 2** at `ts-test`: `make: bun: No such file or directory` (environment; dispatch warned) |
| 15 | `make check` with `~/.bun/bin` on `PATH` | **exit 2** at `ts-test`: `packages/opencode-slipfork/tests/e2e.critical.test.ts` live-world e2e > `slipfork fork 'feat-live' failed: {"error":"slipforkd exited with exit status: 1 before serving …/slipfork/run/daemon.sock; see …/daemon.log","ok":false}` (`15 pass 1 fail`) — out-of-scope package, see §6 |
| 16 | `! grep -riE 'option [ab]\|spike [ab]' crates/slipfork tools/tests/test_do_vm.py` | **exit 0** (no matches) |

`doctor` on this host (verbatim stdout, exit 1):

```text
state_dir: /home/remy/.cache/slipfork
filesystem: xfs
btrfs_super_magic: mismatch
subvolume_ioctl: skipped
noatime: missing
unshare_userns: ok
netns: ok
fix: sudo mount -o user_subvol_rm_allowed,noatime /dev/<device> <state_dir>
```

`slipfork daemon unit --only apparmor` (verbatim stdout):

```text
abi <abi/4.0>,
profile slipfork /home/remy/offworld-nexus/slipfork/target/debug/slipfork{,d} flags=(unconfined) {
  userns,
}
```

## 8. Files changed (this node's paths only)

- `crates/slipfork/src/bin/slipforkd.rs` — Btrfs-only composition root, canonicalized roots.
- `crates/slipfork/src/daemon.rs` — `SnapshotExists` base-seed handling, shared `host_caches`.
- `crates/slipfork/src/{lib.rs,cli.rs,main.rs}` — module registration, `doctor` rewire, `UnitKind::Apparmor`, `binary_dir`.
- `crates/slipfork/src/doctor.rs` *(new)*, `crates/slipfork/src/apparmor.rs` *(new)*.
- `crates/slipfork/tests/support/mod.rs` — dual-backend daemon harness.
- `crates/slipfork/tests/critical_btrfs_live.rs` *(new)*, plus the dual-mode edits in
  `critical_cli.rs`, `critical_ospp_daemon.rs`, `critical_slug.rs`, `critical_vcs_remote.rs`.
- `crates/slipfork-engine/src/lib.rs` — `userns_remediation_hint` names `--only apparmor`.
- `tools/tests/test_do_vm.py` — AppArmor skeleton/byte-equivalence test (+ `REPO_ROOT`).
- No `Cargo.toml`/`Cargo.lock` change: every dependency already existed.



## 9. Lead-widened follow-up: the two acceptance reds cleared (this dispatch)

Scope widened by the lead to `Makefile` and `packages/opencode-slipfork/tests/**`; no other path was
touched (`RESULT.md` excepted).

### 9.1 `make rs-test-list` is green again

Both new live binaries were missing from the ordered `rs-test` recipe **and** from the world-booting
classification. `Makefile` now lists
`cargo test -p slipfork-engine --test critical_btrfs_live -- --test-threads=1` and
`cargo test -p slipfork --test critical_btrfs_live -- --test-threads=1` (each inside its package's
live-suite group) and both pairs in `WORLD_BOOTING_TESTS`. Nothing else in the target changed: the
parallel/serialized phases stay explicit and auditable.

### 9.2 `packages/opencode-slipfork/tests/e2e.critical.test.ts` is dual-mode

Honest dual-mode, mirroring `crates/slipfork/tests/critical_btrfs_live.rs` — no `test.skip`, no
`describe.skip`, no ignored test, no unconditional early return, no weakened expectation:

- `hostIsBtrfs()` compares `statfsSync(<~/.cache>).type` with `BTRFS_SUPER_MAGIC` (`0x9123683e`),
  the same `f_type` the daemon boot gate reads; the suite's scratch root (and therefore its state
  dir) is always created under `~/.cache`, so one probe describes both. It is computed once as
  `LIVE` and selects the mode at collection time.
- **Btrfs host (`LIVE`):** the five existing tests run with every assertion unchanged (worktree
  world, `$BASE_URL` shell routing, session context hint, host-reachable origin, idempotent
  teardown).
- **Non-Btrfs host:** `beforeAll` still drives the real plugin composition root (`plugin.setup`),
  asserts the worktree strategy is registered, and captures the refusal thrown by
  `strategy.create` (`slipfork fork 'feat-live' failed: …`). The suite then registers one contract
  test that asserts:
  - the surfaced error is the CLI fork failure and names the real
    `<state_dir>/run/daemon.log`;
  - that log carries the exact `EngineError::BtrfsRequired` diagnostic — `is not on Btrfs`,
    `0x9123683e`, `BTRFS_SUPER_MAGIC`, `user_subvol_rm_allowed,noatime` — and no `reflink`
    fallback wording;
  - fail closed: no `<state_dir>/worlds`, no `run/daemon.sock`, no `.worktrees/feat-live` checkout,
    and no live `slipforkd` pid naming the runtime dir;
  - one diagnostic line:
    `non-Btrfs state dir: the full opencode-slipfork world flow runs on the Btrfs VM`.
- `afterAll`'s daemon reaper and the contract test share one `runningDaemons(runtime)` helper (same
  `/proc` cmdline scan as before, no behaviour change).

### 9.3 Exact exits (this XFS host, `release/0.1.0`; agent shell `PATH` augmented with
`~/.local/bin`, `~/.cargo/bin`, `~/.bun/bin` — the bare shell has none of `uv`, `cargo`, `bun`)

| # | Command | Result |
|---|---|---|
| 1 | `make rs-test-list` | **exit 0** (no output; was exit 1 with `integration test binaries missing from rs-test: slipfork-engine:critical_btrfs_live slipfork:critical_btrfs_live`) |
| 2 | `cd packages/opencode-slipfork && bun test tests` | **exit 0** — `16 pass`, `0 fail`, 99 `expect()` calls (was `15 pass 1 fail`) |
| 3 | same dir: `bun test tests/e2e.critical.test.ts` | **exit 0** — `1 pass`, `0 fail`, 12 `expect()` calls, prints the one Btrfs-VM diagnostic line |
| 4 | `make check` | **exit 0** — `plan OK`; `guard: OK (498 file(s) checked)`; ruff; `pytest` 113 passed; support-check; `ts-test` 16 pass / 0 fail (the non-Btrfs diagnostic line); `cargo fmt`; clippy + rustdoc; full `rs-test` incl. `slipfork-engine:critical_btrfs_live` 1 passed and `slipfork:critical_btrfs_live` 1 passed; zensical `--strict` + docs-honesty OK |
| 5 | `uv run --no-project --with pytest pytest -q tools/tests` | **exit 0** — `113 passed` |
| 6 | `uv run --no-project tools/guard.py` | **exit 0** — `guard: OK (498 file(s) checked)` |
| 7 | `uv run --no-project tools/critical_guard.py --base 3bc93e3 --node M3.15` | **exit 1** — see below |
| 8 | `uv run --no-project tools/plan.py validate` | **exit 0** — `plan OK` |

`critical_guard.py --node M3.15` flags exactly the three `crates/slipfork-engine/tests/`
modifications owned by the concurrently active `M3.14` (`critical_engine.rs`,
`critical_oci_runtime.rs`, `critical_rootfs_live.rs`); both files this dispatch touched are
authorized (`Makefile`, `packages/opencode-slipfork/tests/**`). The mirrored
`--node M3.14` run reports precisely the complementary set (the four `slipfork/tests/critical_*.rs`
edits plus this e2e file) and authorizes the three engine files — the working tree holds both
in-flight nodes, and the guard scores one node's paths at a time, so there is no uncovered protected
change.

Files changed by this dispatch: `Makefile`, `packages/opencode-slipfork/tests/e2e.critical.test.ts`
(plus this `RESULT.md`).

## 10. Follow-up fix (expert escalation): `world_init_helper()` never trusts a stale helper

**Defect.** `crates/slipfork/tests/support/mod.rs::world_init_helper()` ran
`cargo build -p slipfork-engine --bins` **only** when `target/debug/slipfork-world-init` was
missing. After editing `slipfork-engine`, a standalone
`cargo test -p slipfork --test <suite>` therefore executed the helper binary from an earlier build.
That masked the just-fixed PID-namespace reap bug for an entire test run (`make rs-test` builds
`--bins` first, so `make check` was never affected; only standalone suite runs were unsafe).

**Fix (single file, `crates/slipfork/tests/support/mod.rs`).**

- `helper_is_current(&helper)` — `true` only when the helper exists **and** its
  `metadata().modified()` is at least as new as every `crates/slipfork-engine/src/**/*.rs` file,
  walked with `std::fs::metadata` from `CARGO_MANIFEST_DIR/../slipfork-engine/src` (a work-list
  walk, `.rs` files only). Any unreadable helper mtime, unreadable directory/entry, or unreadable
  source mtime returns `false`, i.e. **rebuild when mtimes are unavailable** — freshness is never
  assumed.
- `world_init_helper()` runs that check inside one `static BUILD_ONCE: std::sync::Once`, so the
  metadata walk and any `cargo build` happen at most once per test process; the `cargo build` runs
  only when the check actually fails (never on every test).
- Failure message stays actionable: the spawn error names
  `` `cargo build -p slipfork-engine --bins` `` plus the helper path and the OS error; a non-zero
  build exit reports the status, the helper path that *would* have been stale, and the captured
  child **stderr**; a successful build that produced no binary names the expected path.

**Proof that the stale path is really taken** (deliberately recreate the bug):

| Step | Command | Output |
|---|---|---|
| 1 | `touch crates/slipfork-engine/src/conformance.rs` (source now newer than the helper) | helper `08:02:00`, source `08:20:31` |
| 2 | `cargo test -p slipfork --test critical_cli -- --test-threads=1` | the test process rebuilt it in-test: helper mtime advanced to `08:20:39` (> the touched `08:20:31`) |

**Acceptance evidence (this XFS host, `release/0.1.0`, `PATH` + `~/.bun/bin`):**

| # | Command | Result |
|---|---|---|
| 1 | `cargo test -p slipfork --test critical_cli -- --test-threads=1` | **exit 0** — `14 passed; 0 failed` (72.41s) |
| 2 | `cargo fmt --all -- --check` | **exit 0** |
| 3 | `uv run --no-project tools/guard.py` | **exit 0** — `guard: OK (498 file(s) checked)` |
| 4 | `make check` | **exit 0** — `plan OK`, `guard: OK (498 file(s))`, ruff, `pytest 113 passed`, `ts-test 16 pass`, fmt, clippy/rustdoc, full `rs-test` (`slipfork:critical_cli` 14 passed, both `critical_btrfs_live` suites 1 passed), zensical `--strict`, `check_docs_honesty: OK` |

**Also observed (not caused by this change, not fixed here):** the first `critical_cli` run after a
cold full rebuild failed `db_redis_exec_resolve_container_entries_and_require_a_tty` with
`slipfork: checking database 'app' failed … psql: … FATAL: the database system is starting up`.
It passes standalone (`1 passed`, 28.25s) and in the two subsequent full runs — a Postgres
readiness race in the world-boot path under load, worth its own node if it recurs.

Files changed by this dispatch: `crates/slipfork/tests/support/mod.rs` (plus this `RESULT.md`).

## 11. Follow-up fix (expert escalation): the Postgres `PM_STARTUP` race is gated server-side

**Defect.** §10 observed `critical_cli::db_redis_exec_resolve_container_entries_and_require_a_tty`
intermittently red with `FATAL: the database system is starting up` on `slipfork up`
(`checking database 'app'`). Root cause: `postgres:18` binds `:5432` milliseconds **before**
postmaster leaves `PM_STARTUP`, and the engine's `readiness_port` probe only proves TCP accept.
`DaemonService::ensure_database` issued its first `psql` immediately after that probe, so under
load it could reach a server that still answers `FATAL: the database system is starting up`.

**Fix (single file, `crates/slipfork/src/daemon.rs`).**

- `POSTGRES_READY_DEADLINE` (30 s) and `POSTGRES_READY_POLL` (100 ms) are named constants beside
  `WORLD_MODE`; the deadline is the only bound, and no fixed sleep was added or raised.
- `DaemonService::wait_for_postgres_ready` runs `pg_isready -U postgres -h 127.0.0.1 -d postgres`
  **inside the world's postgres container** through the existing `WorldEngine::exec_in_service`
  path (the container's own OCI `PATH`/env, exactly like every later `psql`) in a bounded retry
  loop. Exit 0 is the only success; on deadline expiry it fails closed with the deadline, the
  `pg_isready` exit code, and the last server-side diagnostic (`readiness_detail`: stderr when
  present, else `pg_isready`'s stdout "rejecting connections"/"no response" line). An
  `EngineError::ServiceNotRunning`/exec failure aborts immediately instead of being retried.
- `ensure_database` calls that gate **before** its first `psql`, so the subsequent `createdb`,
  `pg_restore`, and `CHECKPOINT` calls are all gated by the same server-side proof.
- `exec_service_checked` was split into `exec_service` (the engine call + effective stderr detail,
  whatever the exit code) and `exec_service_checked` (the same fail-closed wrapper as before).
  Behaviour of every existing caller is unchanged; the readiness loop reuses the raw form.
- Unit test `readiness_detail_prefers_stderr_and_falls_back_to_stdout` pins the timeout diagnostic
  for both the `psql`-style stderr and the `pg_isready` stdout cases (plus the empty/whitespace
  edge). No test was weakened, skipped, or special-cased.

**Redis audit (same race?).** There is no daemon-side first use to gate: `redis_clusters.*` is
spawned with `readiness_port: Some(6379)` and the daemon never runs `redis-cli` at boot. The only
`redis-cli` invocation is the interactive `slipfork redis` entry, which resolves the container
through `exec_target` and refuses without a TTY (`crates/slipfork/src/main.rs`). `redis-server`
binds its listener only after loading the dataset, so the TCP-accept probe is genuine readiness —
no `ping` gate exists and none was added.

**Acceptance evidence (this XFS host, `release/0.1.0`, `PATH` + `~/.bun/bin`):**

| # | Command | Result |
|---|---|---|
| 1 | `cargo test -p slipfork --test critical_cli -- --test-threads=1` (run 1) | **exit 0** — `14 passed; 0 failed` (65.52s); `db_redis_exec_resolve_container_entries_and_require_a_tty ... ok` |
| 2 | same (run 2) | **exit 0** — `14 passed; 0 failed` (60.54s); the db/redis test `ok` |
| 3 | same (run 3) | **exit 0** — `14 passed; 0 failed` (55.22s); the db/redis test `ok` |
| 4 | `cargo test -p slipfork --test critical_cli db_redis_exec_resolve_container_entries_and_require_a_tty -- --exact --test-threads=1` | **exit 0** — `1 passed` (22.63s), i.e. `pg_isready` resolved and answered promptly (no 30 s stall) |
| 5 | `cargo clippy -p slipfork --all-targets -- -D warnings` | **exit 0** |
| 6 | `cargo fmt --all -- --check` | **exit 0** |
| 7 | `uv run --no-project tools/guard.py` | **exit 0** — `guard: OK (498 file(s) checked)` |
| 8 | `make check` | **exit 0** — plan/guard/ruff/pytest/ts-test/fmt/clippy/rustdoc/full `rs-test` (`slipfork:critical_cli` 14 passed incl. the db/redis test, `critical_ospp_daemon` 4 passed, both `critical_btrfs_live` 1 passed), zensical `--strict`, `check_docs_honesty: OK` |

Files changed by this dispatch: `crates/slipfork/src/daemon.rs` (plus this `RESULT.md`).

## 12. Follow-up fix (`review-ioc` MAJOR): no write-only global daemon registry

**Defect.** `keep_daemon` held live `Daemon` values (child `Child` handles, bound sockets) in
`static KEPT: std::sync::Mutex<Vec<Daemon>>`. Nothing ever read the vector: it was mutable global
state whose only effect was to suppress `Drop`, and it grew for the whole process.

**Fix (single file, `crates/slipfork/tests/support/mod.rs`).** `keep_daemon` and its `static KEPT`
are deleted (no alias, no shim, no second spawn path). `ensure_daemon` now states the ownership
explicitly:

```rust
let daemon = Daemon::start_in(scratch, layout).expect("start the test daemon in-process");
// An in-process control server cannot be un-wound and a child daemon must outlive this
// helper, so the test process itself owns the daemon until it exits: never run `Drop`.
std::mem::forget(daemon);
```

Behaviour is unchanged — one daemon per `<runtime_dir>/daemon.sock` per test process, never
shut down — and there is now no hidden registry: the lifetime handoff is visible at the single
call site. `grep -rn 'KEPT\|keep_daemon' crates/slipfork/tests/` returns nothing. This supersedes
the "`Daemon::keep_daemon` keeps a daemon alive" wording in §4.

**Acceptance evidence (this XFS host, `release/0.1.0`, `PATH` + `~/.local/bin`, `~/.cargo/bin`, `~/.bun/bin`):**

| # | Command | Result |
|---|---|---|
| 1 | `cargo test -p slipfork --test critical_cli -- --test-threads=1` | **exit 0** — `test result: ok. 14 passed; 0 failed; 0 ignored` (72.49s) |
| 2 | `cargo fmt --all -- --check` | **exit 0** |
| 3 | `uv run --no-project tools/guard.py` | **exit 0** — `guard: OK (498 file(s) checked)` |
| 4 | `uv run --no-project tools/plan.py validate` | **exit 0** — `plan OK` |
| 5 | `make check` | **exit 0** — `plan OK`; `guard: OK (498 file(s))`; `ruff` `All checks passed!` / `13 files already formatted`; `pytest 113 passed`; `check_support: OK (10 integration(s))`; `ts-test` `16 pass / 0 fail` (non-Btrfs diagnostic line); `cargo fmt`; clippy + rustdoc; full `rs-test` (`slipfork:critical_slug` 6, `critical_vcs_remote` 1, `critical_cli` 14, `critical_l0_wiring` 4, `critical_ospp_daemon` 4, both `critical_btrfs_live` 1 — 0 failed anywhere); zensical `--strict` `No issues found`; `check_docs_honesty: OK` |

Files changed by this dispatch: `crates/slipfork/tests/support/mod.rs` (plus this `RESULT.md`).

## 13. Final-gate fixes (`review-ioc` MAJOR ×1, `review-cruft` MAJOR ×2)

### 13.1 One daemon composer — no second composition graph (`review-ioc` MAJOR)

**Defect.** `crates/slipfork/tests/support/mod.rs::serve_in_process` re-implemented
`slipforkd::run`'s whole adapter graph, and had already drifted from production: the device-key
root was `KeyStorePaths::user_then_system(Some(runtime_parent))` instead of the daemon's resolved
`$XDG_RUNTIME_DIR`, and the registry was built with `forward_port: None` instead of the bound
forward listener. Two graphs meant two places to keep in sync forever.

**Fix.** `crates/slipfork/src/daemon.rs` now owns the single wiring function:

- `pub struct DaemonWiring` — every already-resolved composition-root input: `layout`,
  `Arc<dyn SubvolumeDriver>`, `Arc<dyn BrowserOpener>`, `http_ingress_port`, `forward_port`,
  `host_caches`, `inherited`, `runtime` (`RuntimeAssets`), `target_arch`, `uid`, `gid`, and
  `xdg_runtime_dir` (the `tpmd` device-key root).
- `pub fn compose_daemon(DaemonWiring) -> Result<DaemonGraph, String>` builds
  `DockerfileStageBuilder` → `BtrfsEngine::with_driver(...)` → `WorldRegistry` →
  `FallbackDeviceKeySigner` → `DiskSeedDumpStore` → `DaemonService` exactly once, and returns
  `DaemonGraph { service, registry }` so the caller can bind `HttpIngress` over the same registry.
- `DaemonPorts`, `DaemonConfig`, and `DaemonService::new` are now private: `compose_daemon` is the
  only constructor.
- `slipforkd::run` and the harness call it with identical inputs and differ **only** in the driver
  (`KernelBtrfsDriver` vs `TestTreeDriver`) and the browser (`SystemBrowserOpener` vs the marker
  stub). The harness now passes the same `xdg_runtime_dir` the child daemon would see
  (`runtime_parent(runtime_dir)`), so the device-key root can no longer drift.

### 13.2 No blanket `dead_code` allow; dead `start_with_path` deleted (`review-cruft` MAJOR)

- `#![allow(dead_code)]` is gone from `crates/slipfork/tests/support/mod.rs`.
- `Daemon::start_with_path` (zero callers) is deleted; `try_start_with_path` stays (used by the
  `.jj` PATH-isolation test).
- Internals that no suite uses are private again (`FIXTURE_DOCKERFILE`, `world_init_helper`,
  `await_daemon_served`, `CRUN_FILE_NAME`/`CRUN_SHA256`/`CRUN_URL`, `pinned_crun`, `find_on_path`);
  the genuinely shared surface stays `pub`.
- All six suites now declare `pub mod support;`, mirroring
  `crates/slipfork-engine/tests/support/mod.rs`: the shared surface is crate-reachable per test
  binary, while a genuinely dead private helper still trips `dead_code`.

### 13.3 `--world` help names the printed round-trip grammar (`review-cruft` MAJOR)

- `crates/slipfork/src/cli.rs` module doc and all ten `--world` help strings no longer say "World
  fork"; each now reads
  ``World id (`<fork>`, `<project>:<fork>`, or canonical `<project>`); omit for `cwd`'s world``,
  and every flag carries `value_name = "WORLD"` (the placeholder the pinned
  `cli-contract.critical.test.ts` fixture asserts). `slipfork up --help` renders
  `--world <WORLD>` with that text (verified).
- The published reference (`docs/reference/cli.md:72,76`) is `M3.18`'s; the one-line FOLLOWUP is
  filed in `plan/nodes/M3/M3.15/FOLLOWUPS.md` (grammar already matches; only the `<SELECTOR>` vs
  `<WORLD>` placeholder differs).

### 13.4 Acceptance evidence (this XFS host, `release/0.1.0`, `PATH` + `~/.cargo/bin`, `~/.local/bin`, `~/.bun/bin`)

| # | Command | Result |
|---|---|---|
| 1 | `cargo test -p slipfork --all-targets` | **exit 0** — lib 30, `critical_btrfs_live` 1, `critical_cli` 14, `critical_l0_wiring` 4, `critical_ospp_daemon` 4, `critical_slug` 6, `critical_vcs_remote` 1; 0 failed, 0 ignored |
| 2 | `cargo clippy -p slipfork --all-targets -- -D warnings` | **exit 0** — `Finished dev profile` |
| 3 | `cargo test -p slipfork --test critical_cli -- --test-threads=1` | **exit 0** — `test result: ok. 14 passed; 0 failed` (56.88s) |
| 4 | `make check` | **exit 0** — `plan OK`; `guard: OK (501 file(s) checked)`; ruff `All checks passed!` / `13 files already formatted`; `pytest` `113 passed`; `check_support: OK (10 integration(s))`; `ts-test` `16 pass`; fmt; clippy + rustdoc; full `rs-test` (all suites 0 failed); zensical `--strict` `No issues found`; `check_docs_honesty: OK (9 published doc(s))` |
| 5 | `cargo fmt --all -- --check` | **exit 0** |
| 6 | `uv run --no-project tools/guard.py` | **exit 0** — `guard: OK (501 file(s) checked)` |
| 7 | `uv run --no-project tools/plan.py validate` | **exit 0** — `plan OK` |

Files changed by this dispatch: `crates/slipfork/src/daemon.rs`,
`crates/slipfork/src/bin/slipforkd.rs`, `crates/slipfork/src/cli.rs`,
`crates/slipfork/tests/support/mod.rs`, the six `crates/slipfork/tests/critical_*.rs`
(`mod support;` → `pub mod support;`), `plan/nodes/M3/M3.15/FOLLOWUPS.md` (new), plus this
`RESULT.md`.

## 14. Correction: the world selector that actually ships

§6 says `WorldId` / `resolve_world_selector` are deferred to 0.2.0. The typed `WorldId` newtype and the
documented `project::` location are what is deferred; `crates/slipfork/src/daemon.rs` already ships a
**private** `fn resolve_world_selector(raw, repo)` that accepts `<project>`, `<project>:<fork>`, and a bare
`<fork>`, and fails closed on a foreign project prefix and on more than one `:` separator.
`critical_cli` exercises all of it (`other:feat-x`, `acme-app:feat:x`, the printed `acme-app:feat-x`).
