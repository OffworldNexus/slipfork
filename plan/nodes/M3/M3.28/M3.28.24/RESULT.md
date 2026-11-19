# RESULT M3.28.24 (attempt 4)

Verdict: **done**. Attempt 2's stop-depth, rootfs-relocation, and demo work stands; this attempt
closes the Quad gate's six MAJORs and six MINORs. `restart` now stops every recorded row (including
an `Exited` component whose descendant outlived its `sh`), refuses a frozen world and a pruned
snapshot before it mutates anything, rolls a failed reload back to a non-live record, and shares one
`stop_identity` path with the world PID 1. Coverage now proves the new `reload_world` port contract
and that a rebuilt rootfs actually serves its new content.

## 1. Gate findings, one by one

### MAJOR 1 — `stop_world_services` skipped `Exited` rows

`BtrfsEngine::stop_world_services` filtered the row set to `ServiceState::Running`, so the exact
`M3.28.13` shape — the `sh -eu -c` wrapper exited, its `python3` descendant still owned the port —
was never group-signalled; `freeze_world` then sealed the subvolume over a live writer. The filter is
gone: the list is built from **every recorded service** (as `reload_world` already did), and the
per-row stop itself still reaches the whole process group. `stop_reaps_a_group_descendant_that_
outlives_its_supervisor` now spawns a second `Exited`-shaped component in the same world, asserts its
group survives its wrapper, then calls `freeze_world`: the freeze succeeds, the row becomes
`Stopped`, the group is empty, and the port no longer answers.

### MAJOR 2 — load-bearing conformance coverage of `reload_world`

`assert_world_engine_conformance` threads the live `reload_world` outcome across three clauses:
when the live call returns `Ok(())`, it asserts that a pre-reload service row was cleared and that
`world_lineage` recorded the reload spec's `rootfs` and `boot.base` facts, requires
`WorldNotReloadable` after `freeze_world`, and requires `WorldGone` (never `Unsupported`) on the
destroyed handle; the typed engine-named `Unsupported` is permitted on the frozen and destroyed
arms only when the live call itself declined `reload_world`.

### MAJOR 3 — reload's pre-flight ran after the destroy

`clone_world` and `reload_world` now both call one shared `BtrfsEngine::world_preflight(spec)` first
(`spec.validate()`, world-scoped `HOME`, `SnapshotNotFound`, and `validate_rootfs_reference`) before
stopping services or touching subvolumes, so a malformed spec or a pruned snapshot leaves the
world's PID 1, services, subvolume, and state untouched. Tests:
`a_reload_naming_a_pruned_snapshot_fails_before_it_touches_the_world` covers both `SnapshotNotFound`
and a missing-`HOME` `InvalidWorldSpec` before any mutation.

### MAJOR 4 — no reload lifecycle gate

`reloadable(record)` mirrors `sealable`'s status match: `Live` (or a `Failed` record retrying)
reloads; `Freezing`/`Frozen` fails closed with the new `EngineError::WorldNotReloadable`; `Destroyed`
is `WorldGone`. New test `a_frozen_world_refuses_a_reload_over_its_read_only_subvolume` freezes a
world, asserts the typed refusal, and still destroys the frozen record.

### MAJOR 5 — two `ProcessIdentity` stop implementations

`BtrfsEngine::stop_identity(identity, child, term_grace, kill_grace, d_state_grace, label)` is
shared by `WorldProcess` service groups (`D_STATE_KILL_GRACE`) and `reap_world_init` (world-init
policy unchanged: `term_grace`, then `WORLD_STOP_KILL_GRACE`, **no** `D`-state budget), propagating
non-`ESRCH` `SIGKILL` delivery errors (`cannot SIGKILL <label> (pid N): …`) before `wait_reaped`;
`OciContainer` stop keeps its own `crun kill` / `crun delete` ladder because containers are
signalled through `crun` inside the world rather than by host `ProcessIdentity`. `wait_reaped` is
the single survivor reporter (`pid` + `/proc` state). The M3.27 grace-policy work is untouched.

### MAJOR 6 — unreachable single-pid signaling arms

`spawn_world_process` always records a group or fails closed, so stop no longer re-resolves
`inner.or_else(host_child_pids…)` and no longer has `inner` TERM/KILL arms or a
`wait_reaped(..., None => inner)` fallback. `ServiceProcess::inner` is retained only for the pid
report. A world-process record without a group now fails closed with a typed error instead of
pretending it stopped; the `--lib` fixture spawns a real group leader like production.

### MINOR 7 — failed reload state transitions and truthful storage tracking

`WorldRecord` tracks `WorldSubvolume::{Cloned(SnapshotId), Missing}` as each storage transition
completes (`Missing` once `destroy_subvolume` removes the old base, `Cloned(spec.snapshot)` as soon
as `snapshot_writable` succeeds, and `spec.rootfs` / `boot_lineage(spec)` as soon as
`materialize_world` succeeds). On a subsequent boot failure `reload_world` clears
`init_pid`/`init_identity`/`launcher`, sets `WorldStatus::Failed` (`EngineError::WorldNotLive`), and
removes the half-built control directory without rolling `subvolume`/`rootfs`/`lineage` back to
stale pre-clone facts. Proven by
`reload_across_a_new_base_replaces_the_subvolume_and_never_lies_after_a_failed_boot`.

### MINOR 8 — the rebuild test only proved lineage

`critical_cli`'s rebuild test now uses a component that prints a file `COPY`d from the build context
(`COPY marker.txt /marker.txt`, `run_command: "cat /marker.txt && exec sleep 300"`). The first boot
serves `rootfs-v1`; editing `marker.txt` flips the digest, `restart` reads `rebuilt`/`miss`, and the
retained log serves `rootfs-v2` and never `rootfs-v1`. The same subvolume and state marker survive.

### MINOR 9 — `wait_reaped` had no unit test

`service.rs` gains three: a real `sleep` survivor is reported with its pid and its actual `/proc`
state; a synthetic `D`-state survivor is retried past a zero nominal grace until it disappears; and
a `D`-state survivor past its budget is reported with `pid` and `/proc state 'D'`.

### MINOR 10 — dead `WORKDIR /workspace`

Deleted from `support/demo/Dockerfile`; the image has no `COPY` and the component runs from the
bind-mounted worktree.

### MINOR 11 — §5 below is now an explicit before/after diff.

### MINOR 12 — the recycled-pgid doc overstated atomicity

`ProcessIdentity::signal` now says the group check is a scan followed by `kill(-pgid, …)` and is
**not atomic**; the engine narrows the window by dropping the group handle after a successful stop.

## 2. Tests added or extended (protected)

`crates/slipfork-engine/tests/critical_engine.rs` (21 tests):
`stop_reaps_a_group_descendant_that_outlives_its_supervisor` (+ freeze case),
`a_frozen_world_refuses_a_reload_over_its_read_only_subvolume`,
`a_reload_naming_a_pruned_snapshot_fails_before_it_touches_the_world` (+ missing-`HOME` pre-flight),
`reload_across_a_new_base_replaces_the_subvolume_and_never_lies_after_a_failed_boot`; the
conformance suite now asserts postconditions on live `reload_world`, `WorldNotReloadable` after
`freeze_world`, and strict `WorldGone` on destroyed handles.
`crates/slipfork-engine/src/service.rs`: three `wait_reaped` unit tests.
`crates/slipfork/tests/critical_cli.rs`:
`restart_rebuilds_an_edited_rootfs_input_and_keeps_state_for_unchanged_inputs` now proves the served
`/marker.txt` content changed.

## 3. The five invariants

1. Own process group at spawn — `process_group(0)` + fail-closed `capture_group_leader`; stop signals
   the group, TERM → grace → KILL. **Holds.**
2. Reap what the engine owns — the world-wide stop reaches every recorded row including `Exited`
   ones, and the one stop path reports the surviving pid, its `/proc` state, and the measured time
   since the first `SIGTERM`. The `D`-state exponential backoff applies to **service groups**;
   world-init teardown keeps today's policy — `term_grace`, then `WORLD_STOP_KILL_GRACE`, and
   `reap_world_init` deliberately passes `Duration::ZERO` as its `D`-state budget. Widening that
   world-init budget is node `M3.27`'s job, not this node's. **Holds.**
3. Edited stage inputs rebuild — one `prepare_world` prep path, `rebuilt`/`miss` + new digest on an
   edit, and the rebuilt world serves the new `/marker.txt`; unchanged trees stay `hit` with the
   same subvolume/state. **Holds.**
4. The demo runs from the checkout — Dockerfile has no app source and no dead `WORKDIR`,
   `run_command` is relative to the bind-mounted worktree, SQL is the sibling `db/`. **Holds.**
5. One code path — `boot`/`reboot` share the resolver; `clone_world`/`reload_world` share
   `world_preflight`; `WorldProcess` service groups and world-init stops share `stop_identity`
   (while `OciContainer` keeps its own `crun` ladder). **Holds.**

## 4. Acceptance (verbatim, final tree)

```console
$ cargo test -p slipfork-engine --lib
test result: ok. 72 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 5.03s

$ cargo test -p slipfork-engine --test critical_engine -- --test-threads=1
test a_frozen_world_refuses_a_reload_over_its_read_only_subvolume ... ok
test a_reload_naming_a_pruned_snapshot_fails_before_it_touches_the_world ... ok
test reload_across_a_new_base_replaces_the_subvolume_and_never_lies_after_a_failed_boot ... ok
test stop_reaps_a_group_descendant_that_outlives_its_supervisor ... ok
test result: ok. 21 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 44.77s

$ cargo test -p slipfork --test critical_cli -- --test-threads=1
test restart_rebuilds_an_edited_rootfs_input_and_keeps_state_for_unchanged_inputs ... ok
test restart_stops_a_serving_sh_wrapped_component_and_serves_the_edited_body ... ok
test result: ok. 38 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 140.02s

$ cargo test -p slipfork --test critical_ospp_daemon -- --test-threads=1
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 84.60s

$ cargo clippy --workspace --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.73s

$ make check
… 44 × `test result: ok`; `check_docs_honesty: OK (10 doc(s) …)`
MAKE_CHECK_RC=0

$ uv run --no-project tools/guard.py
guard: OK (674 file(s) checked)

$ cargo fmt --all -- --check
FMT_OK

$ uv run --no-project tools/critical_guard.py --base cdca7d3 --node M3.28.24
critical-guard: OK: authorized by M3.28.24
```

The known `M3.27` non-goal flake — **the base-seed teardown declares the world-init PID 1
unkillable while that pid is stuck in `/proc` state `D`** — did **not** fire in any run of this
attempt. `reap_world_init` now shares `stop_identity` and on failure reads
`… survived SIGTERM and SIGKILL for <measured> (kill grace …, D-state grace 0ns): pid N is still in
/proc state 'D'`, but the refactor changed no world-init grace policy, so the symptom is unchanged.

## 5. Demo proof: before → after (explicit diff from base `cdca7d3`)

**Before** (`git show cdca7d3:support/demo/Dockerfile` and `snow.yml`): the app and its SQL were
baked into the image, so no restart could apply an edit without a Docker rebuild.

```dockerfile
FROM python:3.12-slim AS app

RUN pip install --no-cache-dir "psycopg[binary]>=3.2,<3.3"

WORKDIR /app
COPY app/ /app/
COPY db/ /app/db/

CMD ["python3", "/app/server.py"]
```

```yaml
-    run_command: python3 /app/server.py
```

**After** (`git diff cdca7d3 -- support/demo`):

```diff
--- a/support/demo/Dockerfile
+++ b/support/demo/Dockerfile
@@
 FROM python:3.12-slim AS app
 
+# Runtime dependency only: the app (`app/server.py`) and its SQL (`db/*.sql`) run from the
+# bind-mounted checkout, so editing either and running `slipfork restart` applies instantly.
 RUN pip install --no-cache-dir "psycopg[binary]>=3.2,<3.3"
-
-WORKDIR /app
-COPY app/ /app/
-COPY db/ /app/db/
-
-CMD ["python3", "/app/server.py"]
--- a/support/demo/snow.yml
+++ b/support/demo/snow.yml
@@
-    run_command: python3 /app/server.py
+    run_command: python3 app/server.py
--- a/support/demo/app/server.py
+++ b/support/demo/app/server.py
@@
-DB_DIR = APP_DIR / "db"
+# `app/server.py` runs from the bind-mounted checkout, so its SQL is the sibling `db/` directory.
+DB_DIR = APP_DIR.parent / "db"
```

`support/demo/.dockerignore` (new) drops `app/`, `db/`, `README.md`, `slipfork.toml`, and `snow.yml`
from the build context, so an app/SQL edit never flips the content-addressed rootfs digest: `restart`
is a plain restart, never a rebuild of an image that no longer contains the app.

The boot-from-checkout contract is proven by
`restart_stops_a_serving_sh_wrapped_component_and_serves_the_edited_body` (a real `python3` component
started from the world's bind-mounted cwd, edited in place, serving the edited body after `restart`).
No live-VM claim is made in this attempt. The `M3.28.13` §6 re-run remains the live evidence step.

## 6. Known follow-ups (not fixed here, by the gate's instruction)

- Overlapping stop grace constants and the `D_STATE_KILL_GRACE` reporting wording.
- The duplicated `critical_engine` fixture helpers.
- The orphan reaper's 50 ms `waitpid` poll.
- `ARCHITECTURE.md` has no `reload_world` line (owned by the docs node).

## 7. Bounded gate follow-up (daemon only, `crates/slipfork/src/daemon.rs`)

1. **A refused reload no longer fails a healthy world.** `reboot` marked every `reload_world`
   error `WorldState::Failed`, including the engine's pre-flight refusals (`SnapshotNotFound`,
   `WorldNotReloadable`, `WorldNotLive`, `InvalidWorldSpec`) that mutate nothing. The daemon now
   asks the engine instead of guessing from the error: `DaemonService::engine_holds_live_world` calls
   the new `WorldEngine::world_is_live`, which answers `true` only when the record is `Live` **and**
   the world PID 1 identity captured at boot still matches `/proc` (`ProcessIdentity::is_live`). A
   `Live` record whose init already exited — the window in which `stop_services_with_grace` /
   `reap_world_init` have failed but `reload_world` has not stamped `Failed` yet — is **not** live,
   so no lineage read can be mistaken for one. Live ⇒ the record is re-inserted untouched and only
   the error is reported; not live ⇒ `state = Failed` and `services.clear()`, because the engine
   already stopped them. No variant or message is interpreted, and any read error counts as "not
   live" (fail closed).
2. **A torn-down world is no longer called a lost handle.** `list`/`status` and `ps` share the new
   `engine_read_error` renderer: `WorldNotLive` on a `Failed` record reads
   `world '<id>' last boot failed; run `slipfork up` to retry: …`; only a genuinely absent handle
   keeps `is attached but its engine handle is gone`.

Tests added (`#[cfg(test)] mod tests`): the `RecordingEngine` double gained the three real failure
shapes (`ReloadFault::Refused` / `AfterTheGate` / `TeardownBeforeStamp`), and implements
`world_is_live` explicitly. `a_refused_reload_keeps_the_running_world_and_its_service_rows`
asserts the world stays `Running` with `api` still `Running` and `ps` readable;
`a_reload_failure_past_the_gate_fails_the_world_and_ps_names_the_retry` asserts the record is
`Failed` and `ps` names the failed boot and never "engine handle is gone";
`a_reload_failure_that_already_reaped_the_world_init_fails_the_world` is the regression test for the
`Live`-record-with-dead-PID-1 window: it asserts the record still answers `world_lineage` while
`world_is_live` is `false`, and that the daemon lands on `Failed`, never `Running`.

```console
$ cargo check -p slipfork --all-targets
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.87s   # exit 0

$ cargo test -p slipfork --lib daemon
test result: ok. 34 passed; 0 failed; 0 ignored; 0 measured; 74 filtered out; finished in 0.01s   # exit 0

$ cargo fmt --all -- --check          # exit 0
$ uv run --no-project tools/guard.py
guard: OK (674 file(s) checked)       # exit 0
```

## 8. Final bounded pass (engine cleanup + liveness honesty)

1. **A `Live` record is not a live world.** `WorldEngine::world_is_live(&WorldHandle) -> Result<bool,
   EngineError>` is a required port (no default: `BtrfsEngine`, `RecordingEngine`, and the edge
   `StubWorldEngine` all answer it). `BtrfsEngine` returns `status == WorldStatus::Live &&
   init_identity.is_some_and(ProcessIdentity::is_live)`, and the daemon's `reboot` now asks it
   instead of reading a lineage (§7.1). `live_record`'s doc no longer claims status is liveness.
2. **One unbooted-world cleanup ladder.** The duplicated drop-lifeline / `SIGKILL` init /
   `launcher.kill()` / bounded `wait_for_exit` sequence is now `BtrfsEngine::abandon_unbooted_world`,
   called by both `boot_world`'s failure arm and `clone_world`'s failed record insert (each keeps its
   own control-directory and diagnostic handling; the returned stderr tail is `None` when empty).
3. **The survivor report states the measured time.** `wait_reaped`/`wait_reaped_observing` take the
   caller's first-`SIGTERM` `Instant` and report `survived SIGTERM and SIGKILL for <measured> (kill
   grace …, D-state grace …): pid N is still in /proc state 'X'`, so a stop that burned the 60 s
   `D`-state budget no longer claims "for 10s". World-init grace policy is untouched.

Remaining follow-up: `WorldSummary.lineage` is not optional, so `slipfork list`/`status` still
return an error (now the truthful "last boot failed" message) instead of listing a world whose
engine record has no live PID 1; making `list` render it needs an edge type change
(`lineage: Option<WorldLineage>`) and is not part of this fix.

## 9. Two-item bounded fix: the negative half of `world_is_live`

1. **Edge double derives liveness from its own state.**
   `crates/slipfork-edge/tests/critical_edge.rs` `StubWorldEngine::world_is_live` now returns
   `Ok(!self.frozen.lock().expect("stub frozen worlds").contains(world))` after the existing
   `live_world(world)?` lookup, so a frozen (sealed) handle answers `false` and a destroyed handle
   still fails closed with `WorldGone`.
2. **Conformance matrix pins the three answers.** `crates/slipfork-engine/src/conformance.rs`
   (right after the second `freeze_world` assertion) now asserts a frozen world answers
   `world_is_live` as `false`. Together with the existing live-`true` clause (§8 step, line ~353)
   and the destroyed-`WorldGone` clause in the same matrix, an adapter that answers
   `status == Live` alone — or that infers "live" from owning the handle — now fails the suite.
3. **Contract text matches the implementation.** `lib.rs:710-721` already documents
   `Freezing`/`Frozen`/`Failed` → `false` and gone/teardown/foreign → `WorldGone`; `BtrfsEngine`
   (`status == Live && init_identity.is_some_and(is_live_leader)`) and the edge stub both agree, so
   no doc wording change was needed. No third meaning introduced.

Verification (host PATH per brief; `make check` intentionally not run):

```
cargo fmt --all -- --check                                            exit 0
cargo test -p slipfork-engine --lib                                    exit 0  (74 passed)
cargo test -p slipfork-engine --test critical_engine -- --test-threads=1
                                                                       exit 0  (21 passed)
cargo test -p slipfork-edge --test critical_edge -- --test-threads=1    exit 0  (35 passed)
cargo check --workspace --all-targets                                  exit 0
```
