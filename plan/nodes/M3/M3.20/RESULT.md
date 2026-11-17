# Result M3.20 — Workspace trait & test decoupling

Attempt 1/3 · base `ca5b191` · branch `release/0.1.0` · no commit made.
Fifth pass: the architect's minting-seam ruling is applied verbatim to both code docs (the ruling widened
this node's `paths` to include `ARCHITECTURE.md`, whose §3.3 `WorldHandle` comment and `mint_world_handle`
listing the architect amended), and the 502 case now re-checks its reserved port with a bounded retry.
Fourth pass: architect wording in the two code docs.
Third pass: Quad-Gate re-gate findings (ioc MAJOR on the stub/conformance claim, two minors) plus three
cheap cruft minors.

Shipped state: `cargo test -p slipfork-edge --all-targets` runs **33** `critical_edge` tests, including
`stub_world_engine_passes_the_shared_conformance_matrix` and `clone_world_refuses_an_unrecorded_snapshot`;
the stub carries `entry_command`, `record_snapshot` and `has_snapshot`.

## What changed (per file, all inside M3.20 `paths`)

| File | Change |
|---|---|
| `crates/slipfork-engine/src/lib.rs` | Deleted `SnapshotMode` and `EngineError::StatefulRamUnsupported`. `WorldEngine::create_snapshot(&self, source_dir, id)` / `freeze_world(&self, world, id)` lost the mode argument (distinct RO-snapshot vs writable-clone signatures, `ADR-0010` §3). Crate doc describes the port and its contract, not the legacy adapters. `WorldHandle::mint` stays `pub(crate)`; the trait carries the provided `fn mint_world_handle(&self, seq: u64) -> WorldHandle where Self: Sized` (default `WorldHandle::mint(self.name(), seq)`). Both docs now carry the architect's `ARCHITECTURE.md` §3.3 wording verbatim: `WorldHandle` is an in-process `(engine, seq)` lookup token owned inside `slipforkd` that an engine honours when the engine tag matches `self.name()` and the seq is live in its world table (`WorldGone` otherwise), and minting is "an in-process lookup-token constructor, not an unforgeable capability: minting a live seq exercises full authority on any engine with a matching `name()`, and grants nothing across distinct engine tags or over IPC", with `Self: Sized` excluding `dyn WorldEngine` so dynamic-port consumers get handles only from `clone_world` while out-of-crate adapters and test doubles can construct their return token. `WorldEntryCommand::program` doc names the contract ("the engine's entry program, for example `nsenter`"). |
| `crates/slipfork-engine/src/conformance.rs` | Dropped the StatefulRam step (the mode no longer exists) and renumbered 1–7; mode arguments removed; prose no longer names one adapter. |
| `crates/slipfork-engine/src/incus.rs` | `instance_snapshot_request(instance, id)` sends only `{"name": …}` (stale `"stateful": false` key deleted); signatures aligned; module doc no longer claims stateful RAM snapshots. Handle creation now goes through `self.mint_world_handle(seq)` — one canonical mint path. |
| `crates/slipfork-engine/src/reflink_netns.rs` | Removed both `SnapshotMode::StatefulRam` branches and the mode parameters; handle creation goes through `self.mint_world_handle(seq)`. |
| `crates/slipfork-engine/src/env.rs` | `iter()` doc: the engine's entry program (for example `nsenter`), not `nsenter` as the only program. |
| `crates/slipfork-engine/tests/critical_engine.rs` | Mode arguments dropped. The invented re-snapshot step is deleted (the trait never promises replace-on-duplicate, and the engine is deleted by `M3.21`); steps 1–4 untouched. |
| `crates/slipfork-engine/tests/critical_oci_runtime.rs` | Mode arguments dropped. |
| `crates/slipfork/src/daemon.rs` | `ensure_base_snapshot` calls `create_snapshot(&seed, &empty)` / `freeze_world(&handle, &snapshot)`. |
| `crates/slipfork-edge/src/daemon.rs` | `WorldExecTarget.entry` doc names the engine's entry program (for example `nsenter`). `ResourceSummary.id` doc aligned with the shipped bare-id contract (`main`, `media`, never `pg_databases.<id>`) per the M3.20 LOG requirement. |
| `crates/slipfork-edge/tests/critical_edge.rs` | `FakeIncus`/`IncusEngine` replaced by one `StubWorldEngine` that **passes the shared conformance matrix**: unknown snapshot → `SnapshotNotFound`; nested RW mounts → `OverlappingMounts` (via `WorldSpec::validate`); `create_snapshot` on the matrix's real scratch dir succeeds; clone → stable opaque `Debug` with no secrets; `dial_world_tcp` on closed port 1 → typed `DialFailed`; freeze-once → `WorldNotFreezable`; destroy → `WorldGone` on reuse; and an honest `entry_command` (`env --`, echoed cwd — a stub world has no namespaces to enter) instead of `Unsupported`. `assert_world_engine_conformance(&stub, &state, &test_rootfs())` runs once in `stub_world_engine_passes_the_shared_conformance_matrix`. `clone_world` fails closed `SnapshotNotFound` for an unrecorded snapshot, `clone_test_world` records `base` first, and `clone_world_refuses_an_unrecorded_snapshot` covers the negative case. `DialEngine`, `dial_engine()`, the `Arc<DialEngine>` tuple element and every pass-through are deleted; the real loopback dial lives on the stub. `ready_registry` returns `Arc<WorldRegistry>`. `registry_over(engine, Arc<dyn TunnelRegistrar>, forward_port)` is the one registry builder and `stub_registry(engine)` is its one-line `UnavailableTunnelRegistrar`/no-forward variant. Snapshot bookkeeping is `record_snapshot`/`has_snapshot`; the tautological `snapshot_ids()` assertion and accessor are gone (`destroyed() == 1` still proves the exact handle was destroyed). |
| `crates/slipfork-dag/src/support.rs` | `IntegrationCategory::WorldEngine` doc: "Copy-on-write world engine (e.g. `BtrfsEngine`)". |
| `packages/opencode-slipfork/tests/coordinator.critical.test.ts` | Fixture failure text is the node-prescribed `BtrfsRequired: state dir must be btrfs` (both sites). |

`ARCHITECTURE.md` is now inside this node's widened `paths`: the architect amended §3.3 (the `WorldHandle`
comment and the `mint_world_handle` listing) and the code docs mirror that wording verbatim; this node wrote
no prose there. No `docs/`, `Makefile`, `tools/`, `plan/` (other than this RESULT), `crates/slipfork-testkit`,
`crates/slipfork-engine/tests/support/`, or any other node's file was touched.

Behavioural note carried from the previous pass: with a real loopback dial on the stub,
`unix_ingress_sock_is_0600_and_serves_identical_404` can no longer assume port 3000 is closed (a dev server
answered `308` there), so its unroutable route points at `closed_loopback_port()` (bind ephemeral, read port,
drop). Because a concurrent binder can take that port between the reservation and the dial, the 502 case now
re-checks it at dial time and re-attaches the world on a fresh port up to `REFUSED_DIAL_ATTEMPTS` (5) times —
the same reserved-port race `socket_activate_systemd_handover` re-runs — so the 502 assertion is never
weakened and a lost race fails loudly with the last response.

## Acceptance table

Host: 10 cores, load average **39.08 / 35.46 / 35.16** (unrelated foreign work); `/tmp` is XFS on this host.

Final pass (architect wording + bounded 502 retry):

| Command | Exit | Key output |
|---|---|---|
| `cargo test -p slipfork-edge --all-targets` | 0 | `3 passed` (lib) + **`33 passed; 0 failed`** (`critical_edge`, 0.55 s) |
| `cargo fmt --all -- --check` | 0 | clean |

Earlier passes (unchanged by the doc-only amendment):

| Command | Exit | Key output |
|---|---|---|
| `cargo doc --workspace --no-deps` (`RUSTDOCFLAGS=-D warnings`) | 0 | `Finished` (both amended docs link cleanly) |
| `cargo clippy -p slipfork-engine -p slipfork-edge --all-targets -- -D warnings` | 0 | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | 0 | clean (the previous pass's foreign `M3.22` red is gone) |
| `cargo test -p slipfork-engine --lib` | 0 | `28 passed; 0 failed` |
| `! grep -rn 'SnapshotMode\|StatefulRam' crates/` | 0 | no matches |
| `! grep -rniE 'incus\|reflink\|ficlone\|zfs' crates/slipfork-edge/ crates/slipfork-dag/ crates/slipfork-engine/src/env.rs packages/opencode-slipfork/` | 0 | no matches |
| `! grep -riE 'option [ab]\|spike [ab]'` (this node's paths) | 0 | no matches (terminology gate) |
| `uv run --no-project tools/guard.py` | 0 | `guard: OK (487 file(s) checked)` |
| `uv run --no-project tools/plan.py validate` | 0 | `plan OK` |
| `uv run --no-project tools/critical_guard.py --base ca5b191 --node M3.20` | **1** | all four of this node's protected tests authorized; **two foreign violations** — below |

## Residuals (verbatim, not hidden)

**1. `cargo test -p slipfork-engine --test critical_engine r10_tmpfs` — deterministic host red, pre-existing:**

```
thread 'r10_tmpfs_fails_closed_without_silent_byte_copy_fallback' (859301) panicked at crates/slipfork-engine/tests/critical_engine.rs:203:43:
/tmp tmpfs must fail FICLONE closed: 625
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 13 filtered out; finished in 0.00s
```

`/tmp` is XFS on this host (`df -T /tmp` → `xfs`), so `probe_reflink_dir` succeeds (reflink count `625`) instead
of failing closed. The assertion is correct for a tmpfs runner and this node does not touch it: the whole
pre-`Btrfs` reflink engine and its tmpfs probe are deleted by `M3.21`, which owns this red.

**2. `bun test packages/opencode-slipfork/tests` (load ≈ 39) — sanctioned legacy-engine residual:**

```
(fail) opencode-slipfork live world (M2.18) > (unnamed) [45021.39ms]

 15 pass
 1 fail
 87 expect() calls
Ran 16 tests across 3 files. [86.04s]
```

The failing call is `run → forkWorld → coordinator.ts:204 → e2e.critical.test.ts:179`, i.e. the same
`slipforkd did not serve …/daemon.sock within 15s` boot residual recorded verbatim in the previous pass
(legacy reflink engine, fixed 15 s budget, owned by `M3.13`/`M3.14`/`M3.21`). Not retried to green.

**3. `critical_guard.py --base ca5b191 --node M3.20` — foreign violations:**

```
  M  crates/slipfork-edge/tests/critical_edge.rs  [authorized by crates/slipfork-edge/tests/critical_edge.rs]
  M  crates/slipfork-engine/tests/critical_engine.rs  [authorized by crates/slipfork-engine/tests/critical_engine.rs]
  M  crates/slipfork-engine/tests/critical_oci_runtime.rs  [authorized by crates/slipfork-engine/tests/critical_oci_runtime.rs]
  M  crates/slipfork-ospp/tests/critical_ospp.rs  [VIOLATION: not covered by any node path]
  M  crates/slipfork/tests/critical_ospp_daemon.rs  [VIOLATION: not covered by any node path]
  M  packages/opencode-slipfork/tests/coordinator.critical.test.ts  [authorized by packages/opencode-slipfork/tests/coordinator.critical.test.ts]
critical-guard: FAILED: 2 unauthorized protected test change(s) in M3.20.
```

Both violations are the concurrent `M3.22` test-server migration (moving the OSPP reference server out of
`slipfork_ospp::test_server` into `crates/slipfork/tests/support/`). Neither file is in this node's paths and
this node did not touch them; the guard scans the whole working tree against `ca5b191`.

**4. The remaining world-booting suites were not re-run.** `critical_engine` / `critical_oci_runtime` were
recorded verbatim in the first pass (`3 passed; 11 failed`, every failure `world init pid … is still alive 10s
after SIGKILL`; `1 passed; 2 failed`, readiness timeouts under load). This node only removes arguments and the
invented re-snapshot step there, so no new behaviour is introduced; `cargo clippy --all-targets` type-checks both.

## Working-tree note

`git status` also shows `tools/do_vm.py`, `tools/tests/test_do_vm.py`, `crates/slipfork-ospp/*`,
`crates/slipfork/tests/{support/mod.rs,critical_ospp_daemon.rs}`, `crates/slipfork-ospp/tests/critical_ospp.rs`,
`crates/slipfork/Cargo.toml`, `Cargo.lock` and several `plan/` files for other nodes modified by concurrent
agents. This node's footprint is exactly the 12 files above plus this RESULT.

## FOLLOWUPS

- `crates/slipfork-engine/tests/support/mod.rs` is not in this node's paths and needed no change.
- The minting seam is settled: the architect amended `ARCHITECTURE.md` §3.3 (the `WorldHandle` comment and
  the `mint_world_handle` listing) and both code docs now carry that wording verbatim; no further change is
  expected here.
- Re-run `critical_guard` once the concurrent `M3.22` migration lands.
