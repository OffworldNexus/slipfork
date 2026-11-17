# RESULT M3.13 — `BtrfsEngine`

- **Status:** done (gate fixes A–H applied; implementation + unit suites green; `make check` /
  `make rs-test` red only in the legacy world-booting suites owned by `M3.14`/`M3.21`, verbatim below)
- **Base commit:** `a06100d`; branch `release/0.1.0`
- **Files written:** `crates/slipfork-engine/src/btrfs_engine.rs` (new),
  `crates/slipfork-engine/src/{lib.rs,service.rs,world_init.rs}`,
  `crates/slipfork-engine/src/bin/slipfork-world-init.rs`.
  `Cargo.toml`/`Cargo.lock` needed **no** change (no new dependency).
  `crates/slipfork/`, `crates/slipfork-engine/tests/`, `docs/`, and `reflink_netns.rs` were not touched.

## 0. Gate fix table (functional PASS, legality 4 MAJOR, cruft 4 MAJOR, ioc SPEC_BLIND_SPOT)

| Fix | Finding | Change |
|---|---|---|
| **A** | legality MAJOR — unvalidated rootfs reference | `validate_rootfs_reference` requires `app_rootfs == <state_dir>/cache/rootfs/<digest>/app` and fails closed `InvalidWorldSpec`; called at the top of `clone_world` (before any subvolume) and in `materialize_world`. New test `a_rootfs_outside_the_content_addressed_cache_is_refused`. |
| **B** | legality MAJOR + functional MINOR — a failed teardown bricked the record | `WorldRecord.destroying` fences the world (`is_gone()` → `WorldGone` for every caller) while `destroy_world` stays **re-enterable**: services/init/ctl steps are idempotent, the launcher stays in the record until the PID namespace is gone, and the record is removed only on success. New test `a_failed_teardown_is_retryable_and_never_leaks_the_world` (fake fails the first `destroy_subvolume` with `EBUSY`, second call completes). |
| **C** | legality MAJOR — snapshot source discrimination | A source that is a managed subvolume under `worlds/` **or** `snapshots/` (detected via `driver.list_children`) is sealed with one `snapshot_ro` and never byte-copied; only a plain external directory (the daemon's base-seed dir) is staged with `copy_tree`; anything else fails closed naming both legal sources. Tests `a_snapshot_subvolume_source_is_sealed_without_copying`, `an_unmanaged_directory_under_snapshots_is_refused_as_a_source` (plus the existing `worlds/` cases). |
| **D** | legality MAJOR — silent snapshot replace | New `EngineError::SnapshotExists { id }`; `create_snapshot` and `freeze_world` fail closed instead of destroying an existing snapshot (`ADR-0010` §3 immutability). Test `sealing_an_existing_snapshot_id_fails_closed` covers both paths and proves the world is still freezable afterwards. |
| **E** | cruft MAJOR — phantom remediation | `userns_remediation_hint` no longer names `slipfork daemon unit --only apparmor` (the flag does not exist) and no longer forward-references `OsHostProbe`; it states the sysctl and the profile content an operator installs manually. Doc comment records: *M3.15 re-adds the `--only apparmor` command to this hint when the flag ships.* |
| **F** | cruft MAJOR — duplication | `abort_service` deleted: one `stop_service_process(.., grace)` (`Duration::ZERO` for a failed spawn). One `crun_prefix` builder feeds `crun_command`, `service_entry_command`, and `container_control`. One `sealable` precondition replaces the two freeze status matches; one `live_record` replaces six `Destroyed → WorldGone` guards; one `stop_services_in_order` replaces both two-phase stop loops; one `seal_snapshot` replaces both seal sites; `service.rs` has one `poll_until` (three waits) and one `pump` (two readers) plus a single `POLL_INTERVAL`. Behaviour unchanged; all tests pass. |
| **G** | ioc MAJOR — stale ctl dir / false ready / seq restart | `boot_world` removes `<ctl>/<id>` before writing `world-init.json` (no inherited `init.ready`/`netns.sock`); readiness requires a live `UnixStream::connect` to `netns.sock`, not just the marker; `allocate_world_id` picks the first free `<slug>-<seq>` from `driver.list_children(worlds/)` and `clone_world` skips an `EEXIST` name (bounded to 8 attempts) instead of failing. Test `world_ids_skip_names_a_previous_daemon_left_behind`. |
| **H** | minors | Removed the vacuous `.slipfork-subvol` assertion (replaced by `list_children(world_dir).is_empty()` — a real "no nested subvolumes" check) and the dead `let _ = WorldMount`; doc now says "the only Btrfs-backed engine; `slipforkd` wires it in `M3.15`"; the AppArmor `EPERM` classification runs only when the `unshare` launcher itself exited (`unshare_failed`). |

`WorldStatus::Destroying` was **not** added: `WorldStatus` is shared with `ReflinkNetnsEngine`,
which matches it exhaustively in `reflink_netns.rs` (outside this node's paths, deleted by `M3.21`),
so the engine-local `destroying` flag carries the same semantics without an out-of-scope edit.

## 1. Layout (one subvolume per world, `ADR-0010` §2)

| Path | Kind | Notes |
|---|---|---|
| `<state_dir>/worlds/<slug>-<seq>` | writable Btrfs subvolume | the **one** CoW unit per world; `slug` = `canonical` / `fork-<fork>` / `slot-<n>` via `target_slug`; `seq` is the first free number |
| `<state_dir>/snapshots/<id>` | read-only Btrfs subvolume | produced by `create_snapshot` / `freeze_world` (`BTRFS_SUBVOL_RDONLY`); never replaced |
| `<state_dir>/ctl/<slug>-<seq>` | plain directory | engine-derived control dir (lead ruling: no `WorldSpec.control_dir`), recreated per boot: `netns.sock`, `world-init.json` (`0600`), `init.ready`, `oci/`, `oci-run/`, `mountpoints/`, `merged/`, `etc/{passwd,group}` |
| `<subvol>/rootfs` | symlink → `WorldSpec.rootfs.app_rootfs` | read-only reference to the content-addressed `cache/rootfs/<digest>/app`; validated (fix A), repointed when stale, never copied or mutated |
| `<subvol>/state/{pgdata,redis,s3}` | plain directories | **no nested subvolumes** (`BTRFS_IOC_SNAP_CREATE_V2` does not recurse); concrete leaves (`pgdata/<id>`, `redis/<id>`, `s3/<bucket>`) are created from `StateSubdirMount.host_subdir` at `spawn_service` |

`ctl/` is strictly disjoint from the subvolume, so a seal never captures a socket, a `crun` state
root, or synthesized host identity files.

## 2. Supervision port map (zero logic duplicated inside the new engine)

Moved out of the ported body into shared modules and consumed by `btrfs_engine.rs`:

| Helper | New home |
|---|---|
| `poll_until` (one bounded-wait loop), `wait_for_inner_pid`, `wait_for_exit`, `ProcessIdentity`, `kill_and_reap_child`, `pump` (one reader loop), `spawn_log_reader`, `world_cwd_wrapper`, `container_id`, `host_child_pids`, `run_streamed`, `drain_stderr`, `ensure_container_destination`, `world_guest_for_host`, `POLL_INTERVAL` | `service.rs` (`pub(crate)`, next to `ServiceLogs`) |
| `world_init_mount`, `image_guest_rootfs`, `synthesize_identity_files` (+ private `host_identity_name`, `with_identity_line`), guest-path constants `WORLD_STATE_GUEST`, `OCI_BUNDLES_GUEST`, `OCI_RUN_GUEST`, `OCI_IMAGES_GUEST`, `PASSWD_GUEST`, `GROUP_GUEST`, `INGRESS_GUEST` | `world_init.rs` (`pub(crate)`) |

Stays in `btrfs_engine.rs` (engine-owned, no second copy): `BtrfsEngine`/`Shared`/`WorldRecord`,
`entry_prefix`, `world_init_argv`, `crun_prefix`, `boot_world`, `materialize_world`,
`validate_rootfs_reference`, `spawn_world_process`, `spawn_oci_container`, `crun_command`,
`container_for`, `stop_services_with_grace`, `stop_live_services`, `stop_service_process`,
`finish_service_stop`,
`reap_world_init`, `is_managed_subvolume`, `seal_snapshot`, `allocate_world_id`, `live_record`,
`sealable`, `container_control`, `signal_container`, `delete_container`, `spawn_service_watcher`,
`copy_tree`, `io_error`, `userns_denied`, and the whole `impl WorldEngine`.

`dial_world_tcp` (SCM_RIGHTS over `ctl/<id>/netns.sock`), the in-netns `http-ingress.sock` bridge
wiring, `exec_in_service`, `exec_in_world`, `entry_command`, `service_entry_command`,
`service_logs`, `spawn_service`, `stop_service` are behaviorally identical to the reflink engine's
protocol; only the state/control paths moved.

## 3. Ordering

**`freeze_world`** (`ADR-0010` §3): `sealable` (only `Live`; gone → `WorldGone`, sealed →
`WorldNotFreezable`) → stop `WorldProcess` services → stop `OciContainer` services (bounded
`SIGTERM` → `SIGKILL`, 5 s each) → re-check `sealable`, fence `Freezing` → `seal_snapshot`
(`SnapshotExists` if the id is taken) → `Frozen`. A seal failure rolls the record back to `Live`; a
world whose teardown started mid-seal drops the half-claimed snapshot and fails closed with
`WorldGone`.

**`destroy_world` & `BtrfsEngine::Drop`** (re-enterable, shared `teardown_world` helper): fence
`destroying` → stop services (app components first, then containers; a stop whose bounded
`kill_and_reap_child` fails keeps `ServiceState::Running`, its log open, and the supervisor in the
record so the retry re-enters the stop path, while only `finish_service_stop(Ok)` stores `Stopped`)
→ `reap_world_init` (close the
`stdin` lifeline pipe so `slipfork-world-init` observes `EOF` and exits `0` without hitting
unhandled-`SIGTERM` `SIGNAL_UNKILLABLE` drops, bounded-wait the `unshare` launcher up to 5 s, else
escalate to `SIGKILL` on `/proc/<pid>/ns/pid`-verified `init_pid` and `launcher` with bounded
10 s `WORLD_STOP_KILL_GRACE` **before** polling `/proc/<init_pid>`, propagating any `kill`/wait
failure with the launcher PID while keeping `launcher` in the record for retry) → remove
`<state_dir>/ctl/<id>` → `driver.destroy_subvolume(<worlds>, <id>)` (driver clears
`BTRFS_SUBVOL_RDONLY` first) → drop the world record. `BtrfsEngine::Drop` runs the same
`teardown_world` helper (`Duration::ZERO` service grace, bounded by `WORLD_STOP_KILL_GRACE`) and
emits one `stderr` diagnostic naming the leaked `ctl` and `subvolume` paths per world.

**`create_snapshot`**: existing id → `SnapshotExists`; a managed subvolume under `worlds/` or
`snapshots/` → one `snapshot_ro` (no copy); a non-subvolume under either → fail closed naming both
legal sources; any other plain directory (the daemon's empty base-seed dir) → import once into
`worlds/stage-<id>`, seal, destroy the staging subvolume.

## 4. `--ipc` & supervisor-lifeline argv (`ADR-0010` §6)

```
unshare --user --map-user=$UID --map-group=$GID --keep-caps --mount --net --ipc --pid --fork \
        --kill-child=SIGKILL --mount-proc <world_init_bin> <ctl>/world-init.json
nsenter --target=<init_pid> --user --preserve-credentials --mount --net --ipc --pid --wd=/ -- …
```

Comment in `world_init_argv`/`entry_prefix` states the reason: a private SysV shared-memory and
POSIX message-queue namespace per world, so parallel `postgres` containers never collide on
`shmget` key `90000` or port-derived keys, plus `--kill-child=SIGKILL` and an `O_CLOEXEC` `stdin`
lifeline so `slipfork-world-init` exits `0` on `EOF` without leaving orphaned `unshare` supervisors.
`bin/slipfork-world-init.rs`'s header doc matches.

## 5. Unit tests (`cargo test -p slipfork-engine --lib btrfs_engine` → 22 passed)

1. `construction_fails_closed_off_btrfs_before_creating_anything` — injected non-Btrfs driver →
   `BtrfsRequired { path, actual_fstype, mount_hint }`, nothing created, only `ensure_supported` ran.
2. `relative_paths_are_rejected_without_touching_the_cwd`.
3. `layout_is_one_subvolume_with_a_disjoint_control_directory` — world ids, `worlds/`, `snapshots/`,
   `ctl/` disjointness, `rootfs`/`state` paths.
4. `the_world_init_and_every_namespace_entry_gain_a_private_ipc_namespace` — exactly one `--ipc` in
   both the `unshare` argv and the `nsenter` prefix.
5. `freeze_stops_every_service_before_sealing_the_subvolume` — recording fake observes the service
   process gone at `snapshot_ro`; seal source is the whole world subvolume; second freeze is
   `WorldNotFreezable`.
6. `destroy_stops_services_reaps_the_init_drops_the_control_dir_then_the_subvolume` — service
   `Stopped`, init reaped, ctl gone before `destroy_subvolume`, handle then `WorldGone`.
7. `a_world_directory_that_is_not_a_subvolume_is_refused_as_a_snapshot_source`.
8. `a_plain_snapshot_source_is_imported_into_one_sealed_subvolume` (stage → seal → drop, no residue).
9. `a_managed_subvolume_source_is_sealed_without_copying`.
10. `clone_world_refuses_an_unknown_snapshot_before_creating_a_subvolume`.
11. `world_state_categories_are_plain_directories_inside_the_subvolume` — rootfs reference repointed
    when stale; `state/{pgdata,redis,s3}` are plain dirs; no nested subvolume; idempotent.
12. `service_spec_validation_rejects_empty_argv_before_spawning`.
13. **A** `a_rootfs_outside_the_content_addressed_cache_is_refused` — `clone_world` and
    `materialize_world` both refuse, and no subvolume is created.
14. **B** `a_failed_teardown_is_retryable_and_never_leaks_the_world` — first `destroy_subvolume`
    fails `EBUSY`, world is `WorldGone` meanwhile, second call completes and removes the record.
15. **C** `a_snapshot_subvolume_source_is_sealed_without_copying` (`snapshots/` source).
16. **C** `an_unmanaged_directory_under_snapshots_is_refused_as_a_source`.
17. **D** `sealing_an_existing_snapshot_id_fails_closed` (`create_snapshot` **and** `freeze_world`).
18. **G** `world_ids_skip_names_a_previous_daemon_left_behind`.
19. `reap_world_init_closes_stdin_lifeline_without_stalling_on_sigterm_immune_init`.
20. `reap_world_init_escalates_to_sigkill_and_reaps_launcher_when_init_ignores_eof_and_sigterm`.
21. `a_failed_service_stop_keeps_the_service_running_so_a_retried_teardown_reaps_it` — an injected
    `kill_and_reap_child` failure leaves `ServiceState::Running`, the log open, and the supervisor
    recorded, so the retried teardown re-enters `kill_and_reap_child`, reaps the service process, and
    only then reports `Stopped` with a closed log (`finish_service_stop` is the one success gate).
22. `dropping_the_engine_reaps_live_world_launchers_and_services` (including subvolume/ctl GC and
    `/proc/<pid>/ns/pid` PID-recycling refusal).

The `#[cfg(test)]` `RecordingDriver` journals `EnsureSupported`/`CreateSubvolume`/`SnapshotRo`/
`SnapshotWritable`/`DestroySubvolume`/`ListChildren`, tracks subvolume identity, reports whether a
watched path still existed at seal/destroy time, and can fail the first N destroys. It is
deliberately **not** a second behavioural double: the read-only-seal/marker/clear-RO matrix stays on
`slipfork_testkit::btrfs::TestTreeDriver` for `M3.14`'s integration suites (per `ARCHITECTURE.md`
§1's dev-dep double-compile rule).

## 6. Acceptance (exact exits)

| # | Command | Exit |
|---|---|---|
| 1 | `cargo check --workspace --all-targets` | **0** |
| 2 | `cargo test -p slipfork-engine --lib btrfs_engine` | **0** (`22 passed; 0 failed`) |
| 3 | `cargo test -p slipfork-engine --lib` | **0** (`61 passed; 0 failed`) |
| 4 | `cargo clippy --workspace --all-targets -- -D warnings` | **0** |
| 5 | `! grep -rn 'FICLONE\|reflink_tree\|reflink_file\|IncusEngine\|ReflinkNetnsEngine\|StatefulRam' crates/slipfork-engine/src/btrfs_engine.rs` | **0** (no output) |
| 6 | `grep -n -- '"--ipc"' crates/slipfork-engine/src/btrfs_engine.rs` | **0** (lines 386, 408 + 2 test assertions) |
| 7 | `grep -n 'BtrfsRequired' crates/slipfork-engine/src/lib.rs crates/slipfork-engine/src/btrfs_engine.rs` | **0** (lib.rs:426 + engine doc/construction/test) |
| 8 | `grep -n 'pub use btrfs_engine::BtrfsEngine' crates/slipfork-engine/src/lib.rs` | **0** (`35:`) |
| 9 | `cargo fmt --all -- --check` | **0** |
| 10 | `uv run --no-project --with pytest pytest -q tools/tests` | **0** (`112 passed`) |
| 11 | `uv run --no-project tools/guard.py` | **0** (`guard: OK (492 file(s) checked)`) |
| 12 | `uv run --no-project tools/plan.py validate` | **0** (`plan OK`) |
| 13 | `make check` | **2** — red at `ts-test` (see below) |
| 14 | `make rs-test` | **2** — red at `critical_engine` (see below) |

Terminology gate scoped to my paths:
`! grep -riE 'option [ab]|spike [ab]'` over the five touched files → **0** (no output).

### `make check` (verbatim, load 14.03 → 14.62 on 8 cores)

```
bun test packages/opencode-slipfork/tests
...
error: slipfork fork 'feat-live' failed: {"error":"slipforkd did not serve /home/remy/.cache/sf-e2e-SfW7Rl/xdg-cache/slipfork/run/daemon.sock within 15s; see /home/remy/.cache/sf-e2e-SfW7Rl/xdg-cache/slipfork/run/daemon.log","ok":false}
(fail) opencode-slipfork live world (M2.18) > (unnamed) [15923.07ms]

 15 pass
 1 fail
make: *** [Makefile:47: ts-test] Error 1
make check exit=2
```

Everything before `ts-test` was green in the same run (`plan OK`, `guard: OK`,
`ruff check/format` clean, `112 passed`, `check_support: OK (10 integration(s))`). Because `make`
aborts on the first failing target, `rs-fmt`/`rs-lint`/`rs-test`/`docs-check` did not run inside
`make check`; they were run separately (items 4 and 9 above, plus
`make rs-fmt rs-lint docs-check` → **0**, including `RUSTDOCFLAGS=-D warnings cargo doc` and
`zensical build --strict` + `check_docs_honesty: OK`).

### `make rs-test` (verbatim; only `critical_engine` red)

```
---- in_netns_bridge_port_80_reaches_host_ingress_and_dials_back_into_the_world stdout ----
thread '…' panicked at crates/slipfork-engine/tests/critical_engine.rs:1084:36:
destroy: Execution("world init pid 1652627 is still alive 10s after SIGKILL")

---- r10_tmpfs_fails_closed_without_silent_byte_copy_fallback stdout ----
thread '…' panicked at crates/slipfork-engine/tests/critical_engine.rs:203:43:
/tmp tmpfs must fail FICLONE closed: 426

test result: FAILED. 12 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 58.14s
error: test failed, to rerun pass `-p slipfork-engine --test critical_engine`
make: *** [Makefile:88: rs-test] Error 101
make rs-test exit=2
```

Both reds are pre-existing and environmental, not attributable to this node:

1. `world init pid … is still alive 10s after SIGKILL` is `ReflinkNetnsEngine`'s fixed
   `WORLD_STOP_GRACE` (`reflink_netns.rs`, **untouched** — `git diff --name-only` has no
   `reflink_netns.rs`) under host load ~14; exactly the M3.1 note that the legacy stop path must
   become bounded-but-abort-aware. `BtrfsEngine` implements that bounded/abort-aware reap.
2. `r10_tmpfs_fails_closed_without_silent_byte_copy_fallback` assumes `/tmp` is RAM-backed tmpfs;
   on this host `stat -f -c '%T' /tmp` → `xfs`, so `probe_reflink_dir` succeeds and the test's
   `expect_err` panics. Environment assumption of the legacy suite (`AGENTS.md` #8), not code.

Every suite before `critical_engine` was green in the same run: `--lib --bins` (19/6/3/52/1/4/14/7/1
passed), `--doc` (all ok), `critical_dag` 11, `critical_support` 4, `critical_world_config` 13,
`critical_edge` 33, `critical_rootfs_contract` 13, `critical_ospp` 32, `critical_snow` 12,
`critical_vcs` 23, `critical_world` 15, `critical_rootfs_live` 0 passed / 2 ignored. Because `make`
aborts on the first failure, `critical_oci_runtime`, `critical_slug`, `critical_vcs_remote`,
`critical_cli`, `critical_l0_wiring`, and `critical_ospp_daemon` did **not** run in this pass; they
exercise only `ReflinkNetnsEngine` and are re-homed by `M3.14`/`M3.15`.

## 7. Honest residuals

1. **Real kernel path is unverified here.** This host's `<state_dir>` is XFS, so the live
   `BTRFS_IOC_*` sequence, `user_subvol_rm_allowed` teardown, and `--ipc` SysV isolation across
   parallel `postgres` worlds are `M3.19`'s (Btrfs DO VM) evidence. Everything in this node ran
   against the injected driver plus the real `unshare`/`nsenter` supervision path.
2. **`rootfs` is a read-only reference, not a copy.** `<subvol>/rootfs` is a symlink to
   `WorldSpec.rootfs.app_rootfs`, which must be `<state_dir>/cache/rootfs/<digest>/app` (fix A). The
   `ADR-0010` §2 target layout seeds a real `<subvol>/rootfs` tree once per base; that arrives with
   `WorldEngine::ensure_base` + `BaseSeedSpec` in 0.2.0. `materialize_world` repoints a stale
   reference and refuses a non-symlink `rootfs` entry rather than silently booting a foreign tree.
3. **`state` leaves are created per service.** `WorldSpec` carries no `state_subdirs` list in
   0.1.0, so `clone_world` materializes `state/{pgdata,redis,s3}` and `spawn_service` creates the
   concrete `state/<category>/<id>` leaf from `StateSubdirMount.host_subdir`. The daemon-authored
   list is deferred with `state_subdirs`.
4. **`create_snapshot` has one non-CoW source.** A plain external directory (the daemon's
   `<state_dir>/base/<project>` seed dir) is staged into one subvolume and sealed; a managed
   subvolume is sealed with a single ioctl. This keeps "a snapshot is always a subvolume" true for
   every clone/freeze consumer without teaching them a second snapshot kind.
5. **`SnapshotExists` is a new contract for the daemon (M3.15).** `ensure_base_snapshot` currently
   calls `create_snapshot(<seed>, "<project>-seed-empty")` on every boot; after a daemon restart the
   id already exists, so `M3.15` must treat `EngineError::SnapshotExists` as "this base is already
   sealed, clone it" (look up / reuse the sealed snapshot) instead of surfacing an error. Recorded
   here because `crates/slipfork/` is outside this node.
6. **`reflink_netns.rs` keeps private copies of the moved helpers** until `M3.21` deletes the file
   (out of this node's paths). `btrfs_engine.rs` contains no second copy of any helper.
7. **`EngineError::UsernsDenied` + `userns_remediation_hint` were added** (`lib.rs`) because
   `ARCHITECTURE.md` §3.3 / `ADR-0010` §§4, 7 assign both to M3.13 and `M3.15` cannot add them
   (`crates/slipfork/src/**` only). The hint names no CLI command (fix E); `M3.15` re-adds
   `--only apparmor` to it when that flag ships. Boot classifies only an `unshare`-launcher failure
   with `EPERM` as `UsernsDenied`.
8. **`spawn_service` readiness is TCP-accept only** (`ServiceSpec.readiness_port`), matching the
   current trait surface; `ReadinessProbe`/`ServiceReadinessFailed` are 0.2.0 (`ADR-0002` §2).
9. **`WorldStatus::Destroying` is an engine-local `destroying` flag, not a new enum variant**
   (fix B): the shared `WorldStatus` is exhaustively matched in `reflink_netns.rs`, which is outside
   this node's paths and is deleted by `M3.21`.

## 8. Deferred scope (0.2.0, per the binding lead ruling — not silently dropped)

`slipfork_world::WorldId` + `resolve_world_selector`, `WorldSpec.world` / `control_dir` /
`state_subdirs` injection, `BaseKey::derive` + `BaseSeedSpec` + `WorldEngine::ensure_base`,
`WorldRef`-typed `EngineError` fields, `ReadinessProbe`/`ServiceReadinessFailed`, and
`prune_snapshots`. **`M3.23` relabels the corresponding `ARCHITECTURE.md`/`ADR` target-state
references as 0.2.0** so 0.1.0 docs stay honest. `WorldSpec` is unchanged in this node; the engine
derives `<state_dir>/ctl/<slug>-<seq>` itself.
