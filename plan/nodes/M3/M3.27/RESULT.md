# M3.27 Result — reap worlds, the harness daemon, and D-state world inits

Attempt 2/3, base `9a79afc81a20eb5dc0a911525a82f6f0aa673df6`.
Files changed (all inside `crates/slipfork/tests/**`; zero production files):

- `crates/slipfork/tests/support/mod.rs`
- `crates/slipfork/tests/critical_cli.rs`
- `crates/slipfork/tests/critical_slug.rs`

## Design

1. **World residue reaper.**
   - Each constructor states its adapter explicitly (no probe-chosen driver):
     `Daemon::spawn_child` stores `Arc::new(KernelBtrfsDriver::new())` — the adapter `slipforkd`'s
     composition root builds; the child only exists on a Btrfs state root because `slipforkd`
     refuses any other `--state-dir` before serving. `Daemon::serve_in_process` stores the exact
     `Arc<TestTreeDriver>` it passes to `compose_daemon`. `Daemon::driver()` exposes it.
   - `Daemon::shutdown` (and therefore `Drop`) kills/waits a child or unlinks the in-process
     socket, then calls `reap_world_residue(scratch, state_dir, driver, budget)`.
   - `reap_world_residue` is the single composition, in the only safe order: world-init processes
     first (a live world init still mounts its world, and a mounted subvolume cannot be destroyed),
     then subvolumes. `Daemon::assert_no_world_residue` and the free
     `assert_no_world_residue` both call it, so `critical_cli` no longer has to remember the order.
   - `reap_world_subvolumes_with` walks the suite state root and asks the injected driver at every
     directory for subvolume roots. Only the sealed lineage root `<state_dir>/snapshots` is excluded
     (path comparison, not a bare file-name comparison, so a world directory named `snapshots`
     deeper in the tree is still reaped). Failure returns the surviving subvolume names plus the
     driver's own error; an unreadable directory is an error and a missing state root is a proof
     that no daemon wrote there.
   - The zero-setup auto-spawn proof has no `Daemon` handle. In its Btrfs-only branch it names
     `KernelBtrfsDriver` directly (documented at the call site), and in its non-Btrfs refusal branch
     — where no engine or world was ever created — it runs the process-only
     `reap_world_init_processes_within`.
2. **Harness daemon guard.**
   - `ensure_daemon` returns `Daemon`, not `Option<Daemon>`: every test runs on a fresh scratch
     layout, so an already-served layout is a harness bug and panics with an actionable message. The
     redundant pre-handout process assertion was deleted; the `Drop`/shutdown proof covers it.
   - `critical_slug` binds the handle and passes it to its invocation helper, so every invocation's
     residue proof uses that daemon's own driver and captured budget.
   - `Daemon::shutdown` reports a `child.wait()` failure as a reap failure instead of ignoring it.
3. **D-state / loaded-host flake class.**
   - `ReapBudget` replaces the bare 1 s/10 s constants. It is derived from the host's real
     `/proc/loadavg` 1-minute load and CPU count: `factor = ceil(load / cpus)` clamped to 8, applied
     to 1 s (`SIGTERM`), 10 s (`SIGKILL`) and an extra 20 s granted only while a survivor is
     uninterruptible. The budget is captured **once when the `Daemon` is constructed**; teardown
     calls only `reap_world_init_processes_within` (the load-reading wrapper is deleted), so a
     failing teardown never reads the host load average. No production signature changed.
   - The `SIGKILL` phase reads `/proc/<pid>/stat` every pass, so a `D` pid is retried with exponential
     backoff (50 ms doubling to 2 s) until the D-state extension expires instead of failing at the
     first deadline. The failure reports the real state character and its name, for example
     `pid 4242 (state D = uninterruptible sleep, pgid 4243): …`.
   - Six unit tests in `support::tests` pin the budget derivation/clamp, the `/proc` state parse,
     the state-bearing diagnostic, and the three world-reaper outcomes (success, named survivors,
     no-world-root proof). They run inside every CLI suite binary. The survivor test derives a real
     kernel errno from a real missing-path failure instead of hand-copying `EPERM` (the `EPERM` →
     `SubvolumeDestroyDenied` mapping is the engine's own unit test).

## What the proof does and does not claim

- A surviving world is **destroyed by the harness** at teardown. The suites therefore prove "no
  world subvolume is left undeletable/undeleted under the suite state root", not "the engine would
  have reclaimed a world that outlived its daemon". A world-init process that cannot be signalled
  and a subvolume whose destroy fails each still fail the suite with the survivor's identity.
- On this XFS host the backend is the in-process `TestTreeDriver`. The `KernelBtrfsDriver`
  child-daemon half is compiled and wired but **never executed on this XFS host**; only the emulated
  `TestTreeDriver` path ran. The real kernel path is deferred to the Btrfs VM (M3.28.13).

## # Acceptance — real transcripts (attempt 2)

### `cargo test -p slipfork --test critical_cli -- --test-threads=1`

```
running 36 tests
...
test result: ok. 36 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 158.25s
EXIT=0
```

(Re-run inside `make check`: 36 passed, 233.88 s.)

### `cargo test -p slipfork --test critical_l0_wiring -- --test-threads=1`

```
running 10 tests
...
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 41.06s
EXIT=0
```

(Re-run inside `make check`: 10 passed, 59.00 s.)

### `cargo test -p slipfork --lib support`

```
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 105 filtered out; finished in 0.00s
LIB=0
```

The harness unit tests live in the integration `support` module, so this command filters them out.
The real harness-unit run is:

```
$ cargo test -p slipfork --test critical_l0_wiring -- support::tests --test-threads=1
running 6 tests
test support::tests::process_stat_reads_the_live_state_character_and_process_group ... ok
test support::tests::reap_budget_is_idle_derived_under_no_load_and_scales_with_load ... ok
test support::tests::survivor_diagnostic_reports_the_kernel_state_instead_of_a_bare_timeout ... ok
test support::tests::world_reaper_destroys_what_the_injected_driver_reports ... ok
test support::tests::world_reaper_fails_with_the_surviving_subvolume_names ... ok
test support::tests::world_reaper_is_a_proof_when_no_world_root_exists ... ok
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 4 filtered out; finished in 0.01s
EXIT=0
```

### `btrfs subvolume list ~/.cache/slipfork-cli-tests | grep -v /snapshots/`

**This acceptance command cannot execute on this host.** The host has no Btrfs mount and no
`btrfs` binary:

```
$ stat -f -c %T ~/.cache/slipfork ~/.cache/slipfork-cli-tests
xfs
xfs

$ which btrfs
no btrfs binary

$ btrfs subvolume list ~/.cache/slipfork-cli-tests | grep -v /snapshots/
/bin/bash: line 1: btrfs: command not found
EXIT=127/1   (btrfs 127, grep 1: stdout empty)
```

`docs/knowledge/LESSONS.md` records that `/home/remy` is the 800 GiB XFS volume. The driver-level
evidence is the `TestTreeDriver` world-marker count, reproduced verbatim from the attempt-2 runs:

```
$ find ~/.cache/slipfork-cli-tests -path '*/slipfork/worlds/*.slipfork-subvol' | wc -l
964
```

Measured at the four checkpoints: before attempt 2 = 964, after `critical_cli` = 964, after
`critical_l0_wiring` = 964, after `make check` = 964 (zero new at every step). Scope, honestly:
this counts the marker files that emulate subvolume identity under every
`<state_dir>/worlds/` directory in the test cache; it excludes `snapshots/` and is **not** the
acceptance's kernel-level `btrfs subvolume list | grep -v /snapshots/` (for contrast, the
unscoped marker count including snapshots is 4664). It would not see a world subvolume placed
outside `worlds/`; the reaper's own generic walk does, and the suites fail if it reports an
undeletable survivor.

The 964 are pre-existing leftovers from runs before this node; these suites add zero. Each new
state dir contains only `snapshots/` (sealed base lineage, explicitly out of scope), writable
`ctl/`, and `homes/`. The suites also fail themselves if the reaper cannot destroy a reported
subvolume, so a green suite means "the driver reported none to reap".

### `make check`

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
...
cargo test -p slipfork --test critical_cli -- --test-threads=1       -> 36 passed
cargo test -p slipfork --test critical_l0_wiring -- --test-threads=1 -> 10 passed
cargo test -p slipfork --test critical_ospp_daemon -- --test-threads=1 -> 12 passed
cargo test -p slipfork --test critical_btrfs_live -- --test-threads=1  -> 7 passed
uvx --offline zensical build --strict
No issues found
uv run --no-project tools/check_docs_honesty.py
check_docs_honesty: OK (10 doc(s) match real CLI subcommands: published pages plus support/demo/README.md)
MAKE_CHECK_EXIT=0
```

44 `test result: ok` lines in the log; zero `FAILED`/`panicked`.

### Guards

```
$ uv run --no-project tools/guard.py
guard: OK (672 file(s) checked)

$ uv run --no-project tools/critical_guard.py --base 9a79afc --node M3.27
critical-guard: 9a79afc..working tree, 79 changed file(s)
  M  crates/slipfork/tests/critical_cli.rs  [authorized by crates/slipfork/tests/**]
  M  crates/slipfork/tests/critical_slug.rs  [authorized by crates/slipfork/tests/**]
critical-guard: OK: authorized by M3.27
```

## Loaded-host robustness (attempt 1 code; unchanged by the gate fixes)

`stress-ng` is not installed, so the synthetic load was eight real `yes > /dev/null` busy loops on
the 8-CPU host, on top of a foreign cargo test that was already running:

```
loadavg before: 9.34 11.55 8.67
cargo test -p slipfork --test critical_cli -- --test-threads=1
test result: ok. 36 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 220.54s
EXIT=0
loadavg after: 15.35 13.74 10.21
world subvolume markers before=964 after=964
```

## Honest gaps

- The kernel-Btrfs half of the reaper (`KernelBtrfsDriver` list/destroy on a real mount) is wired
  but unexercised on this XFS host; the emulated `TestTreeDriver` path is what ran. Run both the
  suites and the acceptance grep on the Btrfs VM.
- Temp dirs still survive on XFS because the sealed `snapshots/` roots are `0o555` and `TempDir`
  cannot delete them; that lineage is the explicitly retained `/snapshots/` scope, not a world.
- A genuinely unkillable `D`-state pid cannot be manufactured on this host, so the D-state retry
  path is pinned by the `/proc` state parse + diagnostic unit tests and by code review, not by a
  live D-state process.
