# Result M3.1

Status: **attempt 7 — code complete; the one final frozen-hash sequence is RED and is reported as
red.** The tree is frozen at the hashes below, no gate was retried, and the node stays uncommitted
for the owner's regroup. The red is the **pre-Btrfs engine's fixed 10 s `WORLD_STOP_GRACE`**
teardown (`world init pid … is still alive 10s after SIGKILL`) firing under a foreign load spike
that rose from `1.04 6.41 16.51` to `32.58 … 44.91` while the sequence ran. That engine is deleted
by `M3.21`, and the bounded-but-abort-aware stop requirement is owned by `M3.13`/`M3.14`; it is not
a harness defect and no harness-side edit can make a fixed product window deterministic.

## Attempt-7 disposition

| # | Finding | Disposition |
|---|---|---|
| 1 | **BLOCKER: `critical_edge::socket_activate_systemd_handover` flake** — `free_loopback_port` binds `127.0.0.1:0`, releases, and `systemd-socket-activate` re-binds; a concurrent ephemeral binder wins the port | **FIXED at the source** — `socket_activate_handover_once` is re-run on a **freshly reserved port** up to `HANDOVER_ATTEMPTS = 5`. Only `Failed to listen on …: Address already in use` is retryable; every other early exit, timeout, or disconnected stderr panics with the captured output, the cap panics with the last captured stderr, the readiness signal remains exactly the `Listening on …` line, and every assertion is unchanged. No `sleep()` padding and no serialization of the suite. The `Disconnected` branch now kills/waits the tool **before** joining the stdout reader, so a descendant holding stdout cannot block the join past the deadline |
| 2 | **MAJOR (record): attempt-6 `RESULT.md` cited a stale sequence and stale values** | **FIXED** — one attempt-7 record, one hash table from the final run (§"Final frozen run"), the shipped values quoted once (`DAEMON_STARTUP_DEADLINE = 40 s`, `SHUTDOWN_DEADLINE = 15 s`) |
| 3 | **MINOR: 30 s ready bound and 100 ms poll were literals** | **FIXED** — `HANDOVER_READY_DEADLINE = 30 s` and `HANDOVER_READY_POLL = 100 ms`, used by the loop and the deadline panic |
| 4 | **MINOR: retry classification keyed on an English diagnostic** | **DOCUMENTED** — the `lost_reserved_port` doc comment states that this string is the only classification input, so a wording change fails loudly with captured stderr instead of silently passing |
| 5 | **MINOR: testkit header asserted an unlanded invariant** | **FIXED** — the header cites `ARCHITECTURE.md` §1 as the rule owner **as landed by `M3.25`** |
| 6 | **CONFIDENTIALITY** | **HONOURED** — no sibling/client/other project is named in this file, `LOG.md`, or any file this node owns; foreign load is described only as "an unrelated external repository's worktree `make check`" without names |

### Fresh-port retry — sabotage proofs (both branches, real tool)

The retry was proved by temporarily sabotaging `free_loopback_port` (bind, `std::mem::forget` the
listener, return its port), then reverting:

- **first attempt stolen only** → the tool exits with
  `Failed to listen on 127.0.0.1:<port>: Address already in use`; the retry reserves a fresh port and
  the real handover succeeds: `test result: ok. 1 passed`.
- **every attempt stolen** → exactly 5 attempts, then
  `the real socket handover lost its freshly reserved loopback port on all 5 attempts: a concurrent
  binder kept taking the port between the reservation and the tool's bind, so no live handover could
  be observed; last stderr: Failed to listen on 127.0.0.1:36087: Address already in use …`.

Those runs predate only the later constants/comment/join-order edits; `lost_reserved_port`, the
retry loop, and the attempt cap are byte-for-byte the shipped logic. In the final frozen run the
case passed under load `32.58`: `test socket_activate_systemd_handover ... ok`,
`test result: ok. 31 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.54s`.

### Testkit deadlines — shipped values, quoted once

`crates/slipfork-testkit/src/lib.rs`:

- `DAEMON_STARTUP_DEADLINE = Duration::from_secs(40)` — `// 15 s CLI spawn budget (`SPAWN_TIMEOUT`) + 10 s product stop grace (`WORLD_STOP_GRACE`) + margin.`
- `SHUTDOWN_DEADLINE = Duration::from_secs(15)` — `/// > 10 s WORLD_STOP_GRACE; the caller must outlast the product's own teardown.`
- `POLL_INTERVAL = 50 ms`; `poll_until` is the crate's only bounded-wait primitive, consumed by
  both test crates through `[dev-dependencies]`.

### Guard tool — the real invocation

`make rs-test-list` runs exactly
`@uv run --no-project tools/rs_test_list.py --world-booting $(WORLD_BOOTING_TESTS)`.
There is no `--check` flag: `--check all` is rejected by `argparse`
(`rs_test_list.py: error: unrecognized arguments: --check all`). Every invocation enforces every
invariant; a missing classification is a usage error (`EXIT_USAGE = 64`). The tool derives members
from the root `[workspace] members` (globs expanded), maps `tests/*.rs` / `tests/<name>/main.rs` to
`<package.name>:<test>`, honours `autotests = false` + `[[test]] path`, and fails on a dead
`tests/*.rs` that is neither a declared target nor a `#[path]` module of one.

## Final frozen run — attempt 7, one pass, nothing retried (RED)

Ambient environment: no `TMPDIR` / `CARGO_BUILD_JOBS` / `RUST_TEST_THREADS`; host 10 cores, 48 GiB;
branch `release/0.3.0`, HEAD `490cf494…`. Driver/status/logs: `~/.cache/slipfork-tmp/a8-gate.sh`,
`a8-gate.status`, `a8-*.log`. Started `23:28:43` at load `1.04 6.41 16.51`.

Every gate below was run exactly once, in this order. `in_repo` is the number of `cargo`/`make`/
`rustc` processes whose `cwd` is inside this repository; it was **0 before and after every gate**.

| Gate | Load before → after | Exit | Result |
|---|---|---|---|
| `grep -n 'TMPDIR := $(HOME)/.cache/slipfork-tmp' Makefile` | `1.04 6.41 16.51` | 0 | prints `7:TMPDIR := …` |
| `grep -n 'CARGO_BUILD_JOBS ?= 4' Makefile` | `1.04 6.41 16.51` | 0 | prints `11:CARGO_BUILD_JOBS ?= 4` |
| `! grep -nE 'TMPDIR[^\n]*/tmp(/|$)' Makefile` | `1.04 6.41 16.51` | 0 | no output |
| `make rs-test` | `1.04 6.41 16.51` → `32.58 19.24 18.52` | **2** | **RED** — 331 s, 37 `test result: ok`, 1 FAILED (§below) |
| `make check` (A) | `32.58 19.24 18.52` → `36.71 27.21 21.72` | **2** | **RED** — 159 s, 32 ok, 1 FAILED (§below) |
| `make check` (B, back-to-back) | `36.71 27.21 21.72` → `42.71 32.01 23.95` | **2** | **RED** — 90 s, failed in `ts-test` (§below) |
| `pytest -q tools/tests` | `42.71 32.01 23.95` → `44.91 32.82 24.30` | 0 | `79 passed in 6.86s` |
| `make rs-test-list` | `44.91 32.82 24.30` | 0 | no output |
| `uv run --no-project tools/guard.py` | `44.91 32.82 24.30` | 0 | `guard: OK (483 file(s) checked)` |
| `critical_guard.py --base 6736e08c… --node M3.1` | `44.91 32.82 24.30` | 0 | `critical-guard: OK: authorized by M3.1` (95 changed files) |
| `uv run --no-project tools/plan.py validate` | `44.91 32.82 24.30` | 0 | `plan OK` |
| doctored proofs (`a8-proofs.sh`) | `44.91 32.82 24.30` | 0 | all expected diagnostics (§below) |
| `test -d "$HOME/.cache/slipfork-tmp"` | `44.91 32.82 24.30` | 0 | exists |

**The three reds, exactly as recorded — the product fixed-grace residual, never hidden or retried:**

- `make rs-test`: `crates/slipfork/tests/critical_l0_wiring.rs`
  `host_caches_pass_on_a_matching_glibc_world` panicked at
  `crates/slipfork/tests/support/mod.rs:281` —
  `slipfork: cannot destroy the base seed world: engine protocol/execution error: world init pid
  3943522 is still alive 10s after SIGKILL`; `test result: FAILED. 3 passed; 1 failed; …
  finished in 198.38s`; `make: *** [Makefile:93: rs-test] Error 101`.
- `make check` A: `crates/slipfork-engine/tests/critical_engine.rs`
  `dial_world_tcp_reaches_in_world_listener_via_netns_sock_scm_rights` (`:886`) and
  `redacted_exec_env_and_opaque_world_handles` (`:587`), both
  `destroy: Execution("world init pid … is still alive 10s after SIGKILL")`;
  `test result: FAILED. 12 passed; 2 failed; … finished in 58.04s`;
  `make: *** [Makefile:88: rs-test] Error 101`.
- `make check` B: `ts-test` (Makefile:47), `(fail) opencode-slipfork live world (M2.18) [63388.05ms]`
  with `error: slipfork fork 'feat-live' failed: {"error":"cannot destroy the base seed world:
  engine protocol/execution error: world init pid 4042556 is still alive 10s after SIGKILL",
  "ok":false}`; `make: *** [Makefile:47: ts-test] Error 1`. The `rs-test` stage was never reached,
  so B has no `test result` blocks.

All four failing cases are the same defect: the pre-Btrfs engine's fixed
**10 s `WORLD_STOP_GRACE`** (`crates/slipfork-engine/src/reflink_netns.rs:78`) giving up while the
world's init survives, under load `32.58–44.91`. Owned by `M3.13`/`M3.14` and deleted by `M3.21`.
`M3.1`'s harness cannot fix or mask it; the runs are reported as red and were not retried.

### Frozen hashes — identical before and after the sequence

```
2bf3479d984d551c58a202b3eacb5e87269a6a71eba332384849881a62dd666e  Makefile
3156e3a41d86a2ec2a940ecf4a5618a92da18bd07eb939e2241a315fea8fcf92  Cargo.toml
f7377a7206fd8a5e0f3ab875ae3c40a3a9fb7790af645688be0c7fde48a939dd  Cargo.lock
63e510720a853098882ad7eb0be65362a150dc46607b7542f961c643e43d0fa5  crates/slipfork/Cargo.toml
2a89bbb40c827cb004c9a6ab87ec50d1cdf3d5ea55a5c52c2c525f5b1c7c5c9b  crates/slipfork-engine/Cargo.toml
8a6a6a57d8fc2014d210dd4b3063d5b9300537769fb70666e3b7859bb078e949  crates/slipfork/tests/support/mod.rs
d1ef81113ad3ddb07ace5b345764522c1fd7d86b98b47bf919fcd6e77bb1b2e0  crates/slipfork/tests/critical_slug.rs
feab48cc6e137fbc795099326c69efa333a202f73865fbcc79ede237ca6bca27  crates/slipfork/tests/critical_cli.rs
c34f7e00ebbb764b54513e28a873cb2fce737c416e2e5ceb27d30748b68d6a97  crates/slipfork-engine/tests/support/mod.rs
8d4e0d582c9378a9a9713b85c53cc0db0f3e235d0c8cc6882b14e7baf1994869  crates/slipfork-engine/tests/critical_oci_runtime.rs
637a635cd414f740cfcd4460a551a113d298d65ed1cc074571d685a1442ea4ab  crates/slipfork-edge/tests/critical_edge.rs
518b560792a5908d42e9426ab9bd4d7452ee10eec921292eca593ef8a9ecfde2  crates/slipfork-testkit/Cargo.toml
51b2614a941af5736aebdc208a965338fb664b0916a5254105cbd37b432d32ed  crates/slipfork-testkit/src/lib.rs
bfafcb1424a297e38c1d6c99b2707de6a11b8dc78873142b7d593036a22b23f0  tools/rs_test_list.py
9a91c426ac727b495568cad0c09dbb16d3306ce136d724cf246ec99a41a3f7b0  tools/tests/test_rs_test_list.py
```

The scoped `git status` was identical before and after; nothing was committed.

### Measured envelope (what these numbers do and do not prove)

- This node's harness is deterministic against its own race classes: `make` lists every binary,
  world-booting binaries run one at a time, scratch is on disk, `rustc` jobs are capped, and the
  edge handover retries its own ephemeral-port loss.
- **It is not unconditional determinism.** On this host, earlier attempt-7 code was green at
  `make check` loads `3.11 3.62 4.00`–`11.98 10.34 7.09` (a different, pre-settle hash set, so not
  cited as final evidence). The final run began inside the ≤ 10 envelope (`1.04 6.41 16.51`) and a
  foreign build/`rustc` storm from unrelated external processes drove it to `32.58 → 44.91`, where the pre-Btrfs fixed 10 s
  grace red-lines teardown. At that load the same fixed window would red-line any suite that stops a
  world; `M3.1` neither hides it nor retries it.
- **Consequence for acceptance:** the node's `# Acceptance` asks for two green back-to-back
  `make check` runs at load ≤ 10. That is **not met on the final hashes** in this session; the reds
  above are the measured result.

### Doctored failure proofs (`~/.cache/slipfork-tmp/a8-proofs.log`)

```
### real repository                       exit=0
### missing classification               exit=64  (--world-booting is required)
### missing                               exit=1   slipfork:critical_l0_wiring (missing + not serialized)
### duplicate                             exit=1   slipfork-snow:critical_snow
### unserialized                          exit=1   slipfork:critical_slug
### member outside crates/                exit=1   widget:critical_widget; then exit=0 when listed
### autotests = false orphan              exit=1   vcs:tests/orphan.rs;   then exit=0 when removed
### tests/nested/main.rs not listed       exit=1   alpha:nested;          then exit=0 when listed
```

Every mutation happened on a scratch copy under `~/.cache/slipfork-tmp/a8-proofs`; the repository
was never edited by the proof run.

## Leftover world processes

The final sequence left **24** processes from harness scratch
(`~/.cache/slipfork-cli-tests/**`): 12 `unshare … slipfork-world-init` supervisors (`ppid 1`) and
their 12 `slipfork-world-init` children. All 24 were reaped after the run (SIGTERM, then SIGKILL for
survivors): **12 worlds leaked by this run, 12 reaped, 0 remain**, and there is no `slipforkd`.
Permanent reaping stays `M3.24`'s scope.

## Session hygiene

Earlier in this attempt the tree was touched by superseded attempt-6 writers: two `a6c` runs and one
`i6z` driver were found alive against this working tree, one of them reverted
`crates/slipfork-testkit/src/lib.rs` mid-flight. Those artifacts are **not evidence**, their
confidential third-party name has been removed from this record, and the tree was re-settled,
re-preflighted (`cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D
warnings`, `cargo test -p slipfork-testkit --lib`, `cargo test -p slipfork-edge --test
critical_edge` all green) and measured once as recorded above.

## Files touched

- `Makefile`
- `Cargo.toml`, `Cargo.lock`
- `crates/slipfork/Cargo.toml`, `crates/slipfork-engine/Cargo.toml`
- `crates/slipfork/tests/support/mod.rs`
- `crates/slipfork/tests/critical_slug.rs`
- `crates/slipfork/tests/critical_cli.rs`
- `crates/slipfork-engine/tests/critical_oci_runtime.rs`
- `crates/slipfork-edge/tests/critical_edge.rs` (fresh-port retry, named handover bounds,
  kill-before-stdout-join)
- `crates/slipfork-testkit/Cargo.toml`, `crates/slipfork-testkit/src/lib.rs` (new; deadlines 40 s / 15 s)
- `tools/rs_test_list.py` (new)
- `tools/tests/test_rs_test_list.py` (new)

No mock, fallback, skipped test, `sleep()` padding, weakened assertion, new Cargo feature, or named
sibling/client/other project was introduced. No assertion or deadline was weakened; the caller-side
waits were made strictly larger than the product windows they race.
