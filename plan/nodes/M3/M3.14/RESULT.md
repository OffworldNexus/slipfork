# RESULT M3.14 — engine conformance + critical suites on `BtrfsEngine`

- **Status:** done (attempt 3; the final milestone gate's 3 MAJOR findings are fixed — see §7 — on top
  of attempt 2's Quad Gate fixes; the one residual is still the Btrfs-only branch that cannot
  execute on this XFS host by design)
- **Branch / base:** `release/0.1.0` @ `3bc93e3`
- **Driver seam:** every fast suite constructs
  `BtrfsEngine::with_driver(state_dir, uid, gid, world_init, Arc::new(TestTreeDriver::btrfs()))`
  through the one shared `support::WorldFixture`. No per-crate driver file exists; the only double
  is `slipfork_testkit::btrfs::TestTreeDriver` (`publish = false` dev-dependency). Production
  binaries contain no reference to it.

## 1. Quad Gate fixes (attempt 2)

| Finding | Fix |
|---|---|
| **BLOCKER** — `assert_ne!(created_a.stdout, attached_b.stdout)` was unsound (a fresh IPC namespace numbers its first segment `0` in both clones) | Deleted. Independence is now proved **by removal**: `shmctl(shmid, IPC_RMID)` in clone A succeeds, then `shmget(SHM_KEY, 4096, 0)` (no create flag) exits `2` (`ENOENT`) in A while the same call in B exits `0` and returns the `shmid` B created. The `EEXIST` duplicate-in-A and `IPC_CREAT` attach-in-B assertions are retained. |
| **MAJOR** — deleted freeze-then-clone data assertion (base `3bc93e3:crates/slipfork-engine/tests/critical_oci_runtime.rs:427`) | Restored in `critical_btrfs_live.rs`: after `freeze_world`, `pg_a.state() == Stopped` and `redis_a.state() == Stopped`; a `clone_world` of the sealed `after-clone` snapshot boots `postgres:18`/`redis:8` and answers `select v from seed` = `restored` and `GET clone-key` = `clone-value`; the clone is destroyed. |
| **MAJOR (ioc)** — second `KernelBtrfsDriver::new()` mid-test | One `Arc<dyn SubvolumeDriver> = Arc::new(KernelBtrfsDriver::new())` at the test boundary; `Arc::clone` goes to `BtrfsEngine::with_driver`, and the same `Arc` performs the `destroy_subvolume`/`list_children` teardown. |
| **MAJOR (cruft)** — triplicated `engine_test_dir`, two fixtures, duplicated env/log helpers | All deduped into `tests/support/mod.rs`: one `engine_test_dir`, one `WorldFixture` (replaces `critical_engine.rs::WorldFixture` + `critical_oci_runtime.rs::Harness`), and the shared `world_env`/`container_env`/`log_text`/`pg_state`/`PG_DATA`. The copies are deleted; support internals are private; the blanket `#![allow(dead_code)]` is gone. Each suite declares `pub mod support;` because every integration-test binary links a different subset of the shared surface (a genuinely dead private helper still warns). |
| **MINOR** — `read_dir(...).unwrap_or(true)` read a failed listing as clean teardown | Both teardown listings (and the missing-world-init test) now use `.expect("read_dir worlds")` / `.expect("read_dir ctl")`. |
| **MINOR** — hand-rolled `$HOME/.cache/sfbtrfs/...` in the live suite | The live suite root is `support::disk_tempdir("btrfs-live")` (disk-backed, auto-removed). |
| **MINOR** — fail-closed diagnostic named only the fstype | It now names the probed PATH (`probe.probed_path`) and the fstype. |
| **MINOR** — no evidence note in the registry | `support/integrations.toml` records that the real `crun` assertions execute on the Btrfs DO VM while a non-Btrfs host asserts `BtrfsRequired`; `make support-sync` regenerated the matrix byte-identically apart from the already-renamed row. |
| **MINOR (ioc ruling)** — conformance matrix scope | `conformance.rs` states that the shared matrix covers `&dyn WorldEngine` behaviour while RO-clear-before-destroy is asserted in the driver-injected suite. |

## 2. Suite map (what each file asserts)

| Suite | Host need | Assertions |
|---|---|---|
| `tests/critical_engine.rs` (15 tests) | userns + cached fixture rootfs (no Btrfs, no docker run) | `assert_world_engine_conformance`; two simultaneous clones of one snapshot stay isolated (state + workspace) and host-`$UID:$GID`-owned; parallel `127.0.0.1:18432` bind in both worlds; RW repo + RO artifact bind mounts and `EROFS` refusal; nested-RW `OverlappingMounts`; seal-once freeze lineage + sibling isolation + `WorldGone` double-destroy; `ExecEnv` redaction/wire form/opaque handles; persistent PID 1 stable + reaped (`poll_until`, no sleep); service logs survive idempotent stop, freeze stops the writer before sealing; readiness timeout reaps and releases the id; `SCM_RIGHTS` dial via `<state>/ctl/<world>/netns.sock` (0600, outside the subvolume); two worlds own the same port independently; `nsenter` entry keeps host uid/cwd/PID ns; in-netns `:80` ingress bridge; missing `slipfork-world-init` → `WorldInitFailed`, no half-materialized world; non-Btrfs state dir → `BtrfsRequired` + `mount -o user_subvol_rm_allowed,noatime`, nothing created. |
| ↳ `one_writable_subvolume_per_world_with_ro_snapshot_lineage_and_host_ownership` (new) | same | driver-journal proof of the unified layout: one `snapshot_writable` per world from `snapshots/base` into `worlds/`; one subvolume per world with `rootfs` symlink to `<state>/cache/rootfs/<digest>/app` and plain `state/{pgdata,redis,s3}` (no nested subvolumes); `$UID:$GID`-owned world trees; control dirs disjoint; sibling state-leaf isolation; seal-once freeze lineage; `ClearReadonly` **before** `DestroySubvolume` for a sealed snapshot. |
| ↳ `a_rootfs_outside_the_content_addressed_cache_is_refused_before_a_subvolume_exists` (new) | same | `InvalidWorldSpec` before any subvolume is created (M3.13 gate A, asserted out-of-crate). |
| `tests/critical_oci_runtime.rs` (3 tests) | userns + cached fixture rootfs | kernel-independent container contracts: id/argv shape; unmaterialized backing image; removed image rootfs; absolute/`..` state subdirs; `..` container paths; mount destination absent from the image; readonly mount with `MountMode::ReadWrite`; readonly mount not visible in `WorldSpec.mounts`; readonly/state collision (and no synthesized bundle); a world-process service is `ServiceNotRunning` for `exec_in_service`/`service_entry_command`; unknown logs → `UnknownService`; post-destroy spawn/logs → `WorldGone`. |
| `tests/critical_btrfs_live.rs` (1 dual-mode test) | **Btrfs:** real `BTRFS_IOC_*` + crun + docker; **non-Btrfs:** nothing | one `statfs(<state_dir>).f_type` probe via the one injected `KernelBtrfsDriver`. **Btrfs branch:** base RO snapshot (new-entry write fails), two writable clones, `postgres:18` + `redis:8` `crun` services in **both** clones on the same in-world ports (5432/6379) with real `psql`/`redis-cli` answers, `pg_dump` → read-only volume → `EROFS` refusal → `pg_restore` + query, SysV `shmget(2)` for one fixed key in both worlds (same key created twice ⇒ private `--ipc`; duplicate inside one world ⇒ `EEXIST` 17; **A's `IPC_RMID` then A `ENOENT` / B still attached**), freeze stops both containers, a clone of the sealed snapshot boots the same committed row and cache value, and clear-`BTRFS_SUBVOL_RDONLY`-then-destroy teardown of both snapshots through the same driver `Arc`. **Non-Btrfs branch:** `BtrfsEngine::new` → `EngineError::BtrfsRequired` carrying the probed path + f_type, `BTRFS_MOUNT_HINT` and the exact `mount -o user_subvol_rm_allowed,noatime` remediation; no `worlds/`, `snapshots/`, `ctl/` created; one `eprintln!` diagnostic naming the DO VM. No `#[ignore]`, no silent skip, no early return before an assertion. |
| `tests/critical_rootfs_contract.rs` / `critical_rootfs_live.rs` | unchanged | inherited; both pass on this host. |
| `tests/support/mod.rs` | — | the one shared fixture/helper surface: `engine_test_dir`, `WorldFixture`, `world_env`, `container_env`, `log_text`, `pg_state`, `PG_DATA`, plus the existing `disk_tempdir`/`pinned_crun`/`engine_rootfs`/`oci_rootfs`/`decode_base64`/`copy_tree`. |

`conformance.rs`: documents read-only-snapshot vs writable-clone as distinct operations with no mode
flag (`ADR-0010` §3), and now states that the shared matrix covers `&dyn WorldEngine` behaviour
while RO-clear-before-destroy is asserted by the driver-injected suite. Signature unchanged.

## 3. Invariant-preservation ledger

- Every `critical_engine.rs` assertion was preserved 1:1 (same scenario, same assertion text where
  it still names the behaviour), with only the adapter/constructor and the dial-socket path changed.
- The `assert_ne!` on `shmid` values was unsound, not an invariant: it is replaced by the stronger
  removal proof (A `IPC_RMID` → A `ENOENT`, B still attached with B's own `shmid`). The `EEXIST`
  duplicate assertion is unchanged.
- The freeze-then-clone data assertion deleted in attempt 1 is restored 1:1 (stopped states, cloned
  `postgres:18` row, cloned `redis:8` value, clone destroy).
- `r10_tmpfs_fails_closed…` (`EngineError::ReflinkUnsupported` on tmpfs) → equal replacement
  `r10_a_non_btrfs_state_dir_fails_closed_without_a_silent_fallback` (`BtrfsRequired`, nothing
  created): the substrate gate of the sole engine, not a deleted adapter's reflink probe.
- `tests/critical_oci_runtime.rs` runtime assertions moved intact to `critical_btrfs_live.rs`
  (postgres/redis in two clones, same ports, container exec, read-only dump + `pg_restore`,
  collision refusal stays here). The banned caller-side postgres shim (`wait_postgres_ready`,
  `PG_READY_TIMEOUT`) is gone; the live suite's bounded `query_until`/`exec_ok_until` assert query
  results, not engine readiness (`spawn_service` already returned after its own port gate).
- Deleted with `IncusEngine`: the `FakeIncus` `/1.0` protocol harness (12 assertions). The adapter is
  removed from tests (M3.21 deletes it from `src/`); no equal replacement is possible without
  re-introducing the adapter, and the shared conformance contract is still exercised against the
  sole engine.
- `docker_and_pinned_crun_are_real_inputs` → the pinned-crun SHA assertion now guards the live suite
  (`support::pinned_crun()`), and Docker is a hard input of `support::oci_rootfs` in the Btrfs
  branch (actionable panic, never a skip).

## 4. Support registry diff

```
-id   = "reflink-netns-world-engine"
+id   = "btrfs-world-engine"
 critical_test = "slipfork_engine::critical_engine::r10_r11_btrfs_conformance_cow_isolation_uid_and_parallel_same_port"
 critical_test = "slipfork_engine::critical_btrfs_live::btrfs_kernel_ro_snapshot_two_clones_same_port_services_and_ipc_isolation"   (crun-backing-service-containers)
+# The real `crun` assertions execute on the Btrfs DO VM; a non-Btrfs host asserts `BtrfsRequired`.
```
`make support-sync`: `no demotions; every claim matches its evidence` (tiers stay `declared`, both
recipe pages exist). `docs/support-matrix.md` regenerated by the tool only (one row renamed).

## 5. Acceptance (exact results, attempt 2)

| Command | Exit | Evidence |
|---|---|---|
| `cargo test -p slipfork-engine --all-targets` | **0** | lib 62, world-init bin 0, `critical_btrfs_live` 1, `critical_engine` 15, `critical_oci_runtime` 3, `critical_rootfs_contract` 13, `critical_rootfs_live` 2 — all `ok; 0 failed; 0 ignored` |
| `cargo clippy -p slipfork-engine --all-targets -- -D warnings` | **0** | `Finished dev profile` (zero warnings; the blanket allow is gone) |
| `grep -n '0x9123683e\|BTRFS_SUPER_MAGIC' …/critical_btrfs_live.rs` | 0 | 5 matches (import, probe comment, branch, diagnostic) |
| `grep -n 'BtrfsRequired' …/critical_btrfs_live.rs` | 0 | 4 matches |
| `grep -n 'postgres:18' …/critical_btrfs_live.rs` | 0 | 8 matches incl. `support::oci_rootfs("postgres:18", …)` |
| `! grep -rn 'ReflinkNetnsEngine\|IncusEngine' crates/slipfork-engine/tests/` | 0 | no output |
| `! grep -rn '#\[ignore' crates/slipfork-engine/tests/` | 0 | no output |
| `! grep -rn 'wait_postgres_ready\|PG_READY_TIMEOUT' …/critical_oci_runtime.rs` | 0 | no output |
| `! grep -riE 'option [ab]\|spike [ab]'` (scoped to this node's files) | 0 | no output |
| `make support-sync` | 0 | `no demotions`; matrix written |
| `make support-check` | 0 | `OK (10 integration(s); all evidence pointers verified and docs/support-matrix.md in sync)` |
| `make rs-test-list` | 0 | no output (all five engine binaries listed and classified) |
| `make rs-test` | **0** | 43 `test result: ok` lines, `passed=334 failed=0` (includes `slipfork-engine --test critical_btrfs_live -- --test-threads=1` and the `slipfork` suites) |
| `cargo fmt --all -- --check` | **0** | `FMT_OK` |
| `pytest -q tools/tests` (`uv run --no-project --with pytest pytest -q tools/tests`) | 0 | `113 passed in 11.49s` |
| `uv run --no-project tools/guard.py` | 0 | `guard: OK (498 file(s) checked)` |
| `uv run --no-project tools/plan.py validate` | 0 | `plan OK` |
| `make check` | **2** (red outside this node) | plan/guard/ruff/pytest/support-check/`ts-test` (16 pass, 0 fail)/`rs-fmt`/`rs-lint` all pass; every engine suite passes inside `rs-test`; the run stops at `slipfork:critical_cli::db_redis_exec_resolve_container_entries_and_require_a_tty` (M3.15's suite): `slipfork: checking database 'app' failed with exit code 2: psql: error: connection to server at "127.0.0.1", port 5432 failed: FATAL: the database system is starting up` → `make: *** [Makefile:95: rs-test] Error 101`. The same suite passed in the standalone `make rs-test` run above (exit 0), so this is a readiness flake in a concurrently edited suite outside this node's `paths`. |

## 6. Residuals

1. **The Btrfs branch of the live suite cannot execute on this XFS host by design.** It is compiled
   on every build and asserted on the DO VM (M3.16/M3.19); the new IPC-removal proof and the
   freeze/clone data assertion are part of that branch. The non-Btrfs branch (the production
   fail-closed gate) is the real assertion here.
2. `critical_oci_runtime.rs` uses synthetic backing-image trees for the *pre-spawn validation*
   contract only (documented in-file). No runtime behaviour is claimed from them; the real
   `postgres:18`/`redis:8` proof lives in `critical_btrfs_live.rs`.
3. `make check` stops at `slipfork:critical_cli::db_redis_exec_resolve_container_entries_and_require_a_tty`
   (a readiness race in `crates/slipfork/tests/`, owned by the concurrent `M3.15` work, outside this
   node's `paths`). The same test passed in the standalone `make rs-test` run recorded above; every
   `slipfork-engine` suite passed in both runs.

## 7. Final milestone gate (attempt 3) — 3 MAJOR findings fixed

| Finding | Fix (same node, no assertion deleted without an equal replacement) |
|---|---|
| **[MAJOR]** `critical_btrfs_live.rs:458` — the base suite's container fail-closed assertions were deleted without replacement (base `a602f1e:crates/slipfork-engine/tests/critical_oci_runtime.rs:542-566`) | Restored 1:1 immediately after `engine.stop_service(&client)`: `client.state() == ServiceState::Stopped`; `service_logs(&world_a, "pg-client")` still returns the captured log and `log_text(&client_logs)` is non-empty (logs survive stop); `exec_in_service(&client, ["sleep","0"], …)` returns `EngineError::ServiceNotRunning`; `service_entry_command(&client)` returns `EngineError::ServiceNotRunning`. The client's PID 1 was a silent `sleep infinity`, whose crun stdout is empty by definition, so its argv is now `sh -c "echo pg-client-ready; exec sleep infinity"`: the log assertion is deterministic instead of silently vacuous, with no new sleep and no mock. |
| **[MAJOR]** `tests/support/mod.rs` — hand-rolled base64 decoder silently `continue`d on invalid bytes, so a corrupt/truncated `pg_dump` would truncate silently | `decode_base64` deleted. `base64.workspace = true` added to `crates/slipfork-engine`'s `[dev-dependencies]` (the workspace already pins `base64 = "0.22.1"`); the live suite now decodes with `base64::engine::general_purpose::STANDARD.decode(dump.stdout.trim()).expect("pg_dump must stream canonical base64")`, which fails closed on any non-canonical byte. No reference to `decode_base64` remains anywhere (`grep -rn decode_base64 crates/ packages/ tools/ docs/` → no output). |
| **[MAJOR]** `tests/support/mod.rs:458-476` — `current_uid()`/`current_gid()` shelled out to `id -u`/`id -g` with discarded errors, while `slipfork_engine::current_uid_gid()` was already imported in the same file | Both functions deleted; `fixture_rootfs()` resolves `let (uid, gid) = current_uid_gid();` and passes them into `WorldRootfsRequest`. One canonical source of the invoking identity, no subprocess, no discarded `Result`. |

### Acceptance (exact results, attempt 3)

| Command | Exit | Evidence |
|---|---|---|
| `cargo test -p slipfork-engine --all-targets` | **0** | lib 62, world-init bin 0, `critical_btrfs_live` 1, `critical_engine` 15, `critical_oci_runtime` 3, `critical_rootfs_contract` 13, `critical_rootfs_live` 2 — every suite `ok; 0 failed; 0 ignored` |
| `cargo clippy -p slipfork-engine --all-targets -- -D warnings` | **0** | `Finished dev profile` (zero warnings) |
| `make rs-lint` (`clippy --workspace --all-targets -D warnings` + `RUSTDOCFLAGS=-D warnings cargo doc`) | **0** | both `Finished`, docs generated |
| `cargo fmt --all -- --check` | **0** | no output (`FMT_EXIT=0`) |
| `make support-check` | 0 | `check_support: OK (10 integration(s); all evidence pointers verified and docs/support-matrix.md in sync)` |
| `make rs-test` | **0** | 43 `test result: ok` lines, 335 passed / 0 failed, `EXIT=0` (includes `slipfork-engine --test critical_btrfs_live -- --test-threads=1`) |
| `pytest -q tools/tests` (`uv run --no-project --with pytest pytest -q tools/tests`) | 0 | `113 passed in 14.93s` |
| `uv run --no-project tools/guard.py` | 0 | `guard: OK (498 file(s) checked)` |
| `uv run --no-project tools/plan.py validate` | 0 | `plan OK` |
| `! grep -rn 'decode_base64\|current_uid()\|current_gid()' crates/slipfork-engine/tests/` | 0 | no output (helper deleted, usage migrated) |
| `make check` | **2** (red outside this node) | plan-check / guard / ruff check+format / py-test (113) / support-check / `ts-test` (after adding `~/.bun/bin` to `PATH`; the suite itself is green) all pass; the run stops in `cargo test --workspace --lib --bins` at `slipfork-engine`'s pre-existing `linux_cow::tests::recv_fd_rejects_extra_descriptors_without_leaking_them`: `rejected descriptors must be closed, not leaked (12 -> 13)` (`crates/slipfork-engine/src/linux_cow.rs:408`). That test counts `/proc/self/fd` across a window in which sibling lib tests run concurrently, so a concurrent open fd makes `after > before`; it passed in `cargo test -p slipfork-engine --all-targets` and in both standalone `make rs-test` runs, and 3/3 consecutive `cargo test --workspace --lib --bins` re-runs. Out of this node's `paths` — see residual 4. |
| `make docs-check` | 0 | `No issues found` + `check_docs_honesty: OK (9 published doc(s) match real CLI subcommands)` |

## 8. Residuals after attempt 3

4. **`linux_cow::tests::recv_fd_rejects_extra_descriptors_without_leaking_them` is racy under
   whole-workspace lib runs** (`crates/slipfork-engine/src/linux_cow.rs:405-410`, last touched by
   `M2.7`, outside this node's `paths`). It snapshots `open_descriptors()` before and after `recv_fd`
   while other lib tests execute on sibling threads, so an unrelated concurrent open fd trips
   `after <= before` (`12 -> 13`). It is green in isolation and in the per-package runs this node
   owns; the fix (serialize the measurement or count only the fds the test owns) belongs to a node
   whose `paths` include `src/linux_cow.rs`.
