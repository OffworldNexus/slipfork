# RESULT M3.24

Base `94831c1` (`[M3.21] Scorched-earth deletion…`). This node's own diff touches `Makefile`,
`crates/slipfork-testkit/src/lib.rs`, and the test suites/harnesses under
`crates/slipfork/tests/` and `crates/slipfork-engine/tests/`. Two **lead-adopted product changes**
landed in `29ff39f` while the node was active and are part of the diff against the base:

| Product change | Test evidence |
|---|---|
| `PostgresReady` clock + sleeper injection for the base-seed postmaster readiness gate (`crates/slipfork/src/daemon.rs`): the gate reads time only through the injected closures, so no test waits 30 real seconds. | `cargo test -p slipfork --lib` → `daemon::tests::postgres_readiness_gate_drives_the_injected_clock_and_sleeper ... ok` (31 passed) |
| `slipfork dag plan --repo` deleted (`crates/slipfork/src/{cli,main}.rs`): the plan is always rooted at the VCS toplevel. | `critical_slug::dag_plan_repo_flag_is_gone_and_rejected_by_the_parser` pins exit 2 + stderr naming `--repo` (7 passed) |

The concurrent-rewrite provenance is §Residual 2 below; the attempt-2 corrections are appended at
the end of this file.

## What moved where

| Change | File |
|---|---|
| `pub fn poll_until(bound, interval, probe)` + `pub const POLL_INTERVAL` | `crates/slipfork/tests/support/mod.rs`, `crates/slipfork-engine/tests/support/mod.rs` |
| `terminate_process_group` / `ProcessGroupTermination` (factored `kill -TERM -- -<pgid>` block) | `crates/slipfork/tests/support/mod.rs` |
| `reap_world_init_processes` / `assert_no_world_init_processes` / `world_init_processes` | `crates/slipfork/tests/support/mod.rs` |
| `Daemon::shutdown` now reaps the worlds it booted and fails on survivors | `crates/slipfork/tests/support/mod.rs` |
| Hand-rolled `Instant` loop → `support::poll_until(REDIS_STOP_DEADLINE, …)` | `crates/slipfork/tests/critical_l0_wiring.rs` |
| Three inline `poll_until(10 s, …)` → `(ENGINE_SETTLE_DEADLINE, POLL_INTERVAL, …)`; `thread::sleep(QUIET_WINDOW)` → `observe_for(QUIET_WINDOW)` | `crates/slipfork-engine/tests/critical_engine.rs` |
| `poll_until`/`SHUTDOWN_DEADLINE` now imported from `support`; kill block → helper; stale `"rt"` literal → `DaemonLayout`; reap assertions | `crates/slipfork/tests/critical_cli.rs` |
| Kill block → helper; reap after every CLI invocation | `crates/slipfork/tests/critical_slug.rs` |
| `rs-test-list` and `TMPDIR :=` comments corrected | `Makefile` |

`critical_slug.rs` also carries a **concurrent, not-mine** rewrite of its `--repo` section (the lead
removed `dag plan --repo` from `crates/slipfork/src/{cli,main}.rs` at 12:39–12:40 while this node
was active and updated the suite in place). My diff in that file is only the kill-block factoring
and the reap; the 147-line delta is dominated by that product-contract change.

## 1. One `poll_until` shape per crate

Both harnesses expose the identical signature, parameter names, semantics and doc:

```rust
pub fn poll_until(
    bound: Duration,
    interval: Duration,
    mut probe: impl FnMut() -> Result<(), String>,
) -> Result<(), String>
```

`pub const POLL_INTERVAL: Duration = slipfork_testkit::POLL_INTERVAL;` is the one documented
default per crate (the testkit's 50 ms), so no second magic number exists. `slipfork-testkit` is
outside this node's `paths` and its `poll_until` fixes the interval, so the harness owns the
explicit-interval form; the four suites and the harness's own daemon waits (`await_daemon_served`,
`wait_daemon_teardown`) all call it. `critical_cli`/`critical_engine` no longer import
`slipfork_testkit::poll_until`.

`! grep -rn 'thread::sleep' crates/slipfork/tests/critical_l0_wiring.rs
crates/slipfork-engine/tests/critical_engine.rs` prints nothing. The engine suite's one remaining
fixed wait is the `QUIET_WINDOW` absence assertion, which is not a poll (there is no state to poll
for); it now calls `support::observe_for(window)`, documented as the one legitimate fixed
observation window.

## 2. No duplicated teardown / kill blocks

- `support::terminate_process_group(pgid) -> ProcessGroupTermination { term_delivered,
  members_alive }` owns the two `kill` invocations (`-TERM -- -pgid`, `-0 -- -pgid`). The two call
  sites keep their **different, unchanged** policies: `critical_slug` tolerates `ESRCH` only when
  the group is already empty; `critical_cli::zero_setup_auto_spawn…` still requires the TERM to be
  delivered. No assertion was weakened.
- The `"rt"` literal was **not** in `critical_slug.rs` (stale line refs in `NODE.md`); the only
  occurrence was `critical_cli.rs:435`, and it named a directory nothing creates — a vacuous
  assertion. It is now `DaemonLayout::under(scratch)` (`state_dir`/`runtime_dir`), which makes
  "daemon unit writes no files" check the paths the harness really uses.
- `STREAM_LINE_DEADLINE` (30 s) was already named; its doc now states it is the one remaining
  blocking `recv_timeout` wait in the harness.

## 3. Deadline constants and their derivations

| Constant | Value | Derivation |
|---|---|---|
| `DAEMON_STARTUP_DEADLINE` (testkit) | 40 s | 15 s `SPAWN_TIMEOUT` + 10 s stop grace + margin (comment kept) |
| `SHUTDOWN_DEADLINE` (testkit) | 15 s | `> 10 s` product teardown (comment kept) |
| `REDIS_STOP_DEADLINE` (l0_wiring) | 20 s | the inline 20 s it replaced |
| `ENGINE_SETTLE_DEADLINE` (critical_engine) | 10 s | the three inline 10 s it replaced |
| `REAP_TERM_GRACE` / `REAP_KILL_GRACE` | 1 s / 10 s | new, bounded reaping windows |
| `QUIET_WINDOW` (critical_engine) | 150 ms | unchanged absence window |
| `STREAM_LINE_DEADLINE` | 30 s | unchanged blocking line bound |

**No derivation was possible without breaking the rules.** `SPAWN_TIMEOUT` is a private `const` in
`crates/slipfork/src/main.rs` (the *binary* target; integration tests link the library, so it is
unreachable), and `WORLD_STOP_GRACE` does not exist as a Rust constant anywhere in the workspace —
only as prose in the testkit comment. The engine's `SERVICE_STOP_GRACE`/`WORLD_STOP_KILL_GRACE`
are private too. Widening product `pub` surface is forbidden, and `crates/slipfork-testkit/src/lib.rs`
is outside this node's `paths`, so the two testkit constants keep their hand-copied values and
margin comments. No drift assertion was added because no product constant is reachable from a test.

## 4. Orphan reaping (the real bug)

`reap_world_init_processes(scratch)`:

1. Scan `/proc/*/cmdline`; keep a process whose `argv[0]` ends in `slipfork-world-init` or
   `unshare` **and** one of whose argv entries `Path::starts_with(scratch)` (component-wise, so a
   sibling scratch with a shared prefix cannot match). The engine writes
   `<scratch>/state/ctl/<world>/world-init.json` into both the supervisor and the init argv.
2. `SIGTERM` every match, then `poll_until(REAP_TERM_GRACE, POLL_INTERVAL, …)` re-scanning `/proc`
   by cmdline — never a stale pid.
3. On survivors, `poll_until(REAP_KILL_GRACE, …)` re-scans, `SIGKILL`s the current survivors and
   re-signals on every probe (a signal can be lost to a momentarily unsignalable process). The
   error names every survivor as `pid N (pgid G): <cmdline>`.

Boundedness: at most `1 s + 10 s`, both `poll_until` bounds, no unconditional sleep, no unbounded
loop. A zombie is never reported: `/proc/<pid>/cmdline` is empty for `Z` (verified with a live
probe), so a SIGKILLed-but-unwaited supervisor is correctly treated as dead.

**Process-group safety.** A world init inherits its spawner's process group, and the harness daemon
boots worlds from the *test process*, so `kill(-pgid)` on that group would kill the test runner and
cargo. The reaper therefore signals the group only when it is not the test's own group (a CLI child
started with `process_group(0)`, or an orphan from an earlier run) and signals each matched pid
directly in the own-group case — killing the namespace's PID 1 tears the whole namespace down, and
the supervisor is signalled too. This is the one deliberate deviation from "always group-kill", and
it is a safety invariant, not a fallback.

Call sites:
- `Daemon::shutdown` (so every `Daemon`-booting test asserts zero survivors at teardown; during an
  already-unwinding test it reports on stderr instead of double-panicking and hiding the failure).
- `critical_slug::run_slipfork_with_env` after every CLI invocation (the suite boots no world, so
  it is the suite-exit proof that the auto-spawned daemon group leaves nothing).
- `critical_cli::zero_setup_auto_spawn…` on both exit paths (the one world-booting test with no
  harness `Daemon`).

### Proof that the assertion can fail

Temporarily disabling both `kill` calls in `signal_world_init` and running one world-booting test:

```
test container_backed_world_boots_and_redis_targets_the_container ... FAILED
thread 'container_backed_world_boots_and_redis_targets_the_container' (1220079) panicked at
crates/slipfork/tests/support/mod.rs:430:17:
world init processes for /home/remy/.cache/slipfork-cli-tests/l0-unpIpH survived SIGTERM + SIGKILL:
pid 1221331 (pgid 1219722): unshare --user --map-user=1000 --map-group=1000 --keep-caps --mount
--net --ipc --pid --fork --kill-child=SIGKILL --mount-proc
/home/remy/offworld-nexus/slipfork/target/debug/slipfork-world-init
/home/remy/.cache/slipfork-cli-tests/l0-unpIpH/state/ctl/canonical-2/world-init.json;
pid 1221332 (pgid 1219722): /home/remy/offworld-nexus/slipfork/target/debug/slipfork-world-init
/home/remy/.cache/slipfork-cli-tests/l0-unpIpH/state/ctl/canonical-2/world-init.json
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 3 filtered out
EXIT=101
```

The kill was then restored (no `M324` residue: `grep -rn M324 crates/ Makefile` → nothing) and the
suite re-ran green.

## 5. Makefile comment corrections

- `rs-test-list`: now states exactly what `tools/rs_test_list.py --world-booting` proves —
  (1) list↔filesystem parity (every Cargo integration target listed; a missing pair, an orphaned
  `tests/*.rs`, or a duplicate fails), (2) uniqueness of every `<package>:<test>` pair, (3)
  `-- --test-threads=1` on every pair in `WORLD_BOOTING_TESTS` — and that `WORLD_BOOTING_TESTS`
  itself is explicit and human-maintained (the guard cannot discover which suites boot a world).
- `TMPDIR :=`: the claim was verified, not assumed. `make -n TMPDIR=/tmp/opencode/m324-tmp
  rs-test-list` prints `mkdir -p /tmp/opencode/m324-tmp`, i.e. a command-line variable wins over
  `:=`. The comment now says `:=` pins the *gate's* scratch root (cargo/uv), that a caller can
  still override it on the command line, and that the suites' own scratch roots are hardcoded
  under `$HOME/.cache` and never read `TMPDIR`.

## Acceptance

| Command | Exit | Evidence |
|---|---|---|
| `! grep -rn 'thread::sleep' crates/slipfork/tests/critical_l0_wiring.rs crates/slipfork-engine/tests/critical_engine.rs` | 0 | no output |
| `grep -n 'pub fn poll_until' crates/slipfork/tests/support/mod.rs crates/slipfork-engine/tests/support/mod.rs` | 0 | `support/mod.rs:49`, engine `support/mod.rs:57` |
| `make rs-test-list` | 0 | silent |
| `cargo test -p slipfork --test critical_l0_wiring -- --test-threads=1` | 0 | 4 passed (24.04 s on the final tree; an earlier run on the same code: 33.80 s) |
| `cargo test -p slipfork-engine --test critical_engine -- --test-threads=1` | 0 | 15 passed (23.73 s; earlier 25.99 s) |
| `cargo test -p slipfork --test critical_slug -- --test-threads=1` | 0 | 6 passed (1.65 s; earlier 3.30 s) |
| `cargo test -p slipfork --test critical_cli -- --test-threads=1` | 0 | 14 passed (60.18 s; earlier 130.51 s) |
| `cargo clippy --workspace --all-targets -- -D warnings` | 0 | silent (re-run after the lead's 13:00 `daemon.rs` edit) |
| `cargo fmt --all -- --check` | 0 | silent |
| `uv run --no-project tools/guard.py` | 0 | `guard: OK (500 file(s) checked)` |
| `uv run --no-project tools/plan.py validate` | 0 | `plan OK` |
| `make check` | 0 | full log `/tmp/opencode/m324-make-check.log` |
| `pgrep -af 'slipfork-world-init\|unshare'` | — | no world-init/unshare attributable to any suite scratch root |

All four suites were re-run a second time on the final tree (after the lead's last production edit)
and passed with the numbers in the table; the only `git diff` fingerprint change between the two
runs was `plan/`/`docs/` churn from the lead's own `plan.py` calls, not `crates/` or `Makefile`.

## Residual flakes (verbatim) and environment notes

1. **One-off reap failure, before the re-signal loop.** The first full `critical_l0_wiring` run
   after the change failed with:

   ```
   thread 'redis_entry_fails_closed_with_service_not_running_after_the_container_stops' (1058641)
   panicked at crates/slipfork/tests/support/mod.rs:430:17:
   world init processes for /home/remy/.cache/slipfork-cli-tests/l0-Zp5zsj survived SIGTERM + SIGKILL:
   pid 1065802 (pgid 1046463): /home/remy/offworld-nexus/slipfork/target/debug/slipfork-world-init
   /home/remy/.cache/slipfork-cli-tests/l0-Zp5zsj/state/ctl/canonical-2/world-init.json
   ```

   Instrumentation showed the normal path is: both `unshare` and the init ignore `SIGTERM`
   (state `S`), then die on `SIGKILL` within one probe. The one-off was a signal that did not land
   while the host was heavily loaded. Mitigation: every kill-phase probe re-scans by cmdline and
   re-signals, and the kill grace is 10 s (the poll returns as soon as the last process is gone).
   Five subsequent full-suite runs (three instrumented, two clean) passed.

2. **The working tree was concurrently rewritten while this node was active.** Between 12:39:49 and
   12:40:44 the lead changed `crates/slipfork/src/{daemon,cli,main}.rs` (removing `dag plan --repo`)
   and rewrote the `--repo` section of `crates/slipfork/tests/critical_slug.rs`; the first
   `cargo check` failed on a half-applied `resolve_dag_paths` call. The tree was allowed to
   stabilise (mtime + compile check) before any gate ran. The lead's `make rs-test` and an
   unrelated repository's `make check` were also running on this host during the first suite runs.

3. `[unshare] <defunct>` zombies can be visible while a *different* concurrently running
   `critical_l0_wiring` process is alive (its engine `Child` handles are never waited). They are
   dead, have an empty cmdline, and are not attributable to any scratch root; they are reaped when
   that test process exits. No live world-init/unshare process survived any of my runs.

## Attempt 2 (Quad gate fixes)

The Quad gate found three real defects in attempt 1 plus small ones. This section records what was
hollow, what was duplicated, and the proofs. It supersedes the attempt-1 claims named in each item;
the sections above are left as written.

### A1 (legality MAJOR) — the "daemon unit writes no files" assertion was hollow

Attempt 1 (§2) asserted `DaemonLayout::under(scratch)` stayed absent, but the three `daemon unit`
invocations passed no `--state-dir`/`--runtime-dir`, so the command resolved `~/.cache/slipfork`
and never touched the watched paths: the assertion could not fail. Now all three invocations pass
that layout's `--state-dir`/`--runtime-dir` and set `XDG_CACHE_HOME`/`XDG_RUNTIME_DIR`/
`XDG_DATA_HOME` under the scratch root, and the test asserts the explicit layout roots **and** the
default XDG roots stay absent.

Proof it can fail — a temporary `create_dir_all(layout.state_dir())` + write in `daemon_unit`, then
reverted (`git diff crates/slipfork/src/` is empty; `grep -rn M324 crates/ Makefile` prints
nothing):

```
test daemon_unit_renders_the_resolved_user_and_never_a_template_specifier ... FAILED

thread 'daemon_unit_renders_the_resolved_user_and_never_a_template_specifier' (1378425)
panicked at crates/slipfork/tests/critical_cli.rs:450:9:
daemon unit must not create /home/remy/.cache/slipfork-cli-tests/c-wxdvao/state
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 13 filtered out; finished in 0.02s
EXIT=101
```

### A2 (cruft MAJOR) — three `poll_until` bodies

Attempt 1's §1 claim ("the harness owns the explicit-interval form") left three identical poll
loops: `slipfork-testkit`'s fixed-interval one plus byte-identical copies in both harnesses. Now
`crates/slipfork-testkit/src/lib.rs` owns the single implementation:

```rust
pub fn poll_until_with_interval(
    bound: Duration,
    interval: Duration,
    probe: impl FnMut() -> Result<(), String>,
) -> Result<(), String>
```

and `poll_until(bound, probe)` is a call to it with `POLL_INTERVAL`. Both harnesses now only
`pub use slipfork_testkit::{POLL_INTERVAL, poll_until_with_interval as poll_until};` — no local
body and no re-declared constant. `grep -c 'pub fn poll_until'` → testkit 2 (the two functions),
`crates/slipfork/tests/support/mod.rs` 0, `crates/slipfork-engine/tests/support/mod.rs` 0.

### A3 (legality MINOR) — the reaper no longer fails open

`world_init_processes` now `expect`s `/proc` to be readable (an unreadable `/proc` is a failed
inspection, never "no survivors"), and a matched process whose `/proc/<pid>/stat` is unreadable is
reported as a survivor with `pgid unknown` and signalled by pid instead of being dropped.
`signal_world_init` only group-kills a *known* group that is not the test's own, and it `expect`s
the test's own group to be readable rather than guess (an unknown own group could make a group
signal hit the test runner).

### A4 (cruft MAJOR) — Makefile comment

The `rs-test-list` comment claimed "no `tests/` file is left unbuilt or unlisted", which is false
for `crates/slipfork-vcs/tests/critical_conformance_matrix.rs` (an unlisted `#[path]` module of
`critical_vcs`, which the guard accepts). It now states the guard's four real rules: every
`tests/*.rs` belongs to a declared test target (a `[[test]]` path or a `#[path]` module of one);
the list names targets only and every declared target is listed; pairs are unique; every
`WORLD_BOOTING_TESTS` pair carries `-- --test-threads=1`.

### A5 (legality MINOR) — `dag plan --repo` removal is pinned

`critical_slug::dag_plan_repo_flag_is_gone_and_rejected_by_the_parser` asserts exit code 2 and a
stderr naming `--repo`.

### A6 (cruft MINORs)

- `critical_slug` now calls `support::assert_no_world_init_processes(scratch, context)` (the helper
  gained the `context` argument) instead of a second inline reap + `panic!`.
- `await_no_world_init` is gone; one `no_world_init(scratch) -> Result<(), String>` is used by both
  reap loops.
- The reaper doc now says every *wait* re-scans `/proc`, while the initial `SIGTERM` pass signals
  the first scan.
- `observe_for` stays, documented as the suite's named quiet-window primitive (never a poll).

### A7 (ioc MINOR) — the in-process bypass is honest

`ensure_daemon` forgets the in-process daemon, so its `Drop` → `shutdown` → reap never runs. The doc
now says so, and `assert_no_world_init_processes(scratch, …)` runs before the `mem::forget`;
`critical_slug` additionally reaps after every CLI invocation. Attempt 1's §4 claim that "every
`Daemon`-booting test asserts zero survivors at teardown" holds only for non-forgotten handles.

### Attempt-2 acceptance (this tree)

| Command | Exit | Evidence |
|---|---|---|
| `! grep -rn 'thread::sleep' crates/slipfork/tests/critical_l0_wiring.rs crates/slipfork-engine/tests/critical_engine.rs` | 0 | no output |
| `grep -c 'pub fn poll_until' crates/slipfork-testkit/src/lib.rs crates/slipfork/tests/support/mod.rs crates/slipfork-engine/tests/support/mod.rs` | 0 | `2`, `0`, `0` |
| `make rs-test-list` | 0 | silent |
| `cargo test -p slipfork --test critical_l0_wiring -- --test-threads=1` | 0 | 4 passed (56.42 s standalone; 24.68 s in the final `make check`) |
| `cargo test -p slipfork-engine --test critical_engine -- --test-threads=1` | 0 | 15 passed (30.80 s standalone; 23.59 s in the final `make check`) |
| `cargo test -p slipfork --test critical_slug -- --test-threads=1` | 0 | 7 passed (1.92 s standalone; 1.92 s in the final `make check`) |
| `cargo test -p slipfork --test critical_cli -- --test-threads=1` | 0 | 14 passed (100.01 s standalone; 66.57 s in the final `make check`) |
| `cargo test -p slipfork --lib` | 0 | 31 passed |
| `cargo test -p slipfork-testkit --lib` | 0 | 15 passed |
| `cargo clippy --workspace --all-targets -- -D warnings` | 0 | silent |
| `cargo fmt --all -- --check` | 0 | silent |
| `uv run --no-project tools/guard.py` | 0 | `guard: OK (501 file(s) checked)` |
| `uv run --no-project tools/plan.py validate` | 0 | `plan OK` |
| `make check` | 0 | full log `/tmp/opencode/m324-attempt2-make-check.log`; every suite green, `check_docs_honesty: OK (9 published doc(s) …)` |
| `pgrep -af 'slipfork-world-init\|unshare'` | — | only the invoking shell (its own command line contains the pattern); no world-init/unshare attributable to any suite scratch root |

`make check` was run twice: once before this RESULT edit and once after the final
`signal_world_init` fail-closed tweak (the log above is the second, final run). The only later edit
is this plan document, and `guard.py`/`plan.py validate` were re-run green afterwards.

## Attempt 3 (three residual Quad findings)

### F1 (MAJOR) — the `dag plan --repo` deletion broke the benchmark

`poc/run_benchmark.py` still passed `--repo <project>` to `slipfork dag plan`, which this node
deleted from the CLI, so the binary exited 2 at the plan step and `make bench` / `make bench-update`
could never reach the suites. The `"--repo"` / `str(project)` pair is gone; `--file
<project>/slipfork.toml` is the only argument, and the plan still roots at the VCS toplevel of the
`project` cwd the harness already runs in. No other `slipfork dag plan --repo` call exists in the
repo.

Because this XFS host fails closed at the Btrfs gate before any `dag plan`, the fix is proven
directly in a scratch git repo (`$HOME/.cache/slipfork-tmp/proof-repo`, `slipfork.toml` +
`src/lib.txt`):

| Command (cwd = scratch repo) | Exit | Output |
|---|---|---|
| `cargo run -q -p slipfork -- dag plan --file ./slipfork.toml --repo .` | 2 | `error: unexpected argument '--repo' found` / `Usage: slipfork dag plan --file <FILE>` |
| `cargo run -q -p slipfork -- dag plan --file ./slipfork.toml` | 0 | `artifact front            Rebuild (1670aa6f30e1)` |

### F2 (MINOR) — the readiness regression test used the production constants

`daemon::tests::postgres_readiness_gate_drives_the_injected_clock_and_sleeper` injected
`30 s`/`100 ms`, i.e. exactly `POSTGRES_READY_DEADLINE`/`POSTGRES_READY_POLL`, so reverting
`await_postgres_ready` to the hardcoded constants would still have passed. The policy is now
`3 s`/`10 ms` and both counts are asserted exactly against what the loop really produces:
success path = 4 probes and `elapsed == 30` (three 10 ms waits), deadline expiry = 301 probes
(`clock == 3000 ms` first reaches the inclusive `>=` deadline on probe 301) plus `within 3s` in the
message.

### F3 (MINOR) — the Makefile `TMPDIR` claim was false

`Makefile:7-8` claimed all integration suites pin their scratch roots under `$HOME/.cache` and never
read `TMPDIR`; `critical_snow.rs` / `critical_ospp.rs` use `tempfile::tempdir()`, which honours it.
The sentence is now scoped to the world-booting suites and says other suites may use `tempfile`.

### Attempt-3 acceptance (this tree)

| Command | Exit | Evidence |
|---|---|---|
| `cargo test -p slipfork --lib` | 0 | 31 passed (includes the retuned readiness gate) |
| `cargo clippy --workspace --all-targets -- -D warnings` | 0 | silent |
| `cargo fmt --all -- --check` | 0 | silent |
| `uv run --no-project --with pytest pytest -q tools/tests/test_run_benchmark.py` | 0 | 37 passed |
| `uvx --offline ruff check poc tools` / `ruff format --check poc tools` | 0 / 0 | `All checks passed!` / `13 files already formatted` |
| `make rs-test-list` | 0 | silent |
| `make check` | 0 | full log `/tmp/opencode/m324-attempt3-make-check.log`; every suite green, `check_docs_honesty: OK (9 published doc(s) …)` |
| `grep -rn '"--repo"' poc/ tools/ crates/ packages/` | 0 | only unrelated hits: `tools/critical_guard.py:254` and `tools/tests/test_tools.py:203,207,214` (the guard tool's own `--repo`), plus `crates/slipfork/tests/critical_slug.rs:811,821` (the intentional "`--repo` must be rejected" regression test) |

