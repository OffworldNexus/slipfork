# Result M3.22

Status: **code complete; the `test-server` Cargo feature is deleted and the OSPP v1 reference
server / `FakeTpmd` doubles now live in a real `publish = false` test crate,
`crates/slipfork-ospp-testkit`, consumed only through `[dev-dependencies]`.** Branch
`release/0.1.0`, base `ca5b191`. No commit.

## What changed

| File | Change |
|---|---|
| `crates/slipfork-ospp-testkit/` (new) | `publish = false` test crate. The four doubles (`memory_store.rs`, `reference_server.rs`, `signers.rs`, `tpmd_server.rs`) were **moved byte-identically** from `crates/slipfork-ospp/tests/support/` into `src/` (`memory_store`, `reference_server`, `signers` are byte-for-byte identical; `tpmd_server` differs only by the one dead `#[allow(dead_code)]` line deleted in the cruft round), declared as `pub mod` and re-exported exactly as the old `support/mod.rs` did, so consumers have **one access path**: `slipfork_ospp_testkit::{ReferenceOsppServer, FakeTpmd, DumpRecord, …}`. `#![forbid(unsafe_code)]`; the crate doc states it is test scaffolding, `publish = false`, never linked into a shipped binary. |
| `Cargo.toml` (root) | `crates/slipfork-ospp-testkit` added to `[workspace] members`. |
| `ARCHITECTURE.md` | §1's normative feature/test-scaffolding rule kept intact; its M2-residue sentence is now past tense and names the shipped location (`crates/slipfork-ospp-testkit`, `publish = false`, `[dev-dependencies]` of both protected suites) instead of the wrong `tests/support/` destination. |
| `crates/slipfork-ospp-testkit/src/tpmd_server.rs` | Removed the dead `#[allow(dead_code)]` (and its comment) on the public `start_at`; public items never trip `dead_code`, and the method is used by `critical_ospp_daemon.rs`. |
| `crates/slipfork-ospp/Cargo.toml` | The whole `[features]` table (`default`, `test-server = ["dep:tempfile", "dep:tpmd-proto"]`) and its comment are gone; the optional `tempfile` / `tpmd-proto` normal dependencies are gone. `[dev-dependencies]` now carries `slipfork-ospp-testkit` plus `tempfile` and `tpmd-proto`, both still used directly by `critical_ospp.rs` (scratch dirs, `RejectCode`). |
| `crates/slipfork-ospp/src/lib.rs` | Feature-gated `test_server` / `test_server_tpmd` `#[path]` modules, their comment, and the `extern crate self as slipfork_ospp;` alias deleted. |
| `crates/slipfork-ospp/tests/critical_ospp.rs` | `mod support;` deleted; imports the doubles from `slipfork_ospp_testkit::…`. `crates/slipfork-ospp/tests/support/` deleted entirely. |
| `crates/slipfork/Cargo.toml` | The OSPP `[dev-dependencies]` line (`slipfork-ospp = { …, features = ["test-server"] }`) is replaced by `slipfork-ospp-testkit = { path = "../slipfork-ospp-testkit" }`; the other two dev-deps (`slipfork-testkit`, `tempfile`) are unchanged, so the final list is exactly those three. Production `slipfork-ospp` dependency stays plain. |
| `crates/slipfork/tests/support/mod.rs` | **Unchanged from base** (byte-identical): it never declares an OSPP double, so the four other `mod support;` suites never link `tokio`/`tungstenite`/`tpmd-*`/`zstd`/`p256`. `#![allow(dead_code)]` pre-existed at base (`ca5b191` line 8). |
| `crates/slipfork/tests/critical_ospp_daemon.rs` | Imports `DumpRecord`, `FakeTpmd`, `HandleRecord`, `ReferenceOsppServer`, `ReferenceServerConfig`, `S3BucketRecord`, `TpmdBehavior` directly from `slipfork_ospp_testkit::…`; header no longer names a feature or a path. Every handshake, fragment-pairing, dump, vault, fork, and fail-closed assertion is unchanged. |
| `Cargo.lock` | New `slipfork-ospp-testkit` package; `slipfork`'s dev-dependency list is exactly `slipfork-ospp-testkit`, `slipfork-testkit`, `tempfile`. |

### No cross-package include, no feature, no duplicate

The doubles are physically inside `crates/slipfork-ospp-testkit/src/`; there is no `#[path]` into
another package's `tests/` tree, no `test-server` feature, and no second copy. Both protected
suites (`crates/slipfork-ospp/tests/critical_ospp.rs` and
`crates/slipfork/tests/critical_ospp_daemon.rs`) import the same crate through `[dev-dependencies]`.

### Testkit dependencies — what the code actually imports

`hex`, `p256`, `serde`, `serde_json`, `sha2`, `slipfork-ospp`, `slipfork-snow`, `tempfile`,
`tokio`, `tpmd-ipc`, `tpmd-proto`, `tungstenite`, `zstd` (all workspace pins). Verified by
`cargo check -p slipfork-ospp-testkit` and
`cargo clippy -p slipfork-ospp-testkit --all-targets -- -D warnings` (both green; `hex` and `tokio`
are used fully-qualified, so they were added after the first check caught them).

### `s3_buckets` 60 s readiness wait — no-op, with evidence

The one-shot wait this node was asked to replace does not exist in
`crates/slipfork/tests/critical_ospp_daemon.rs` at base `ca5b191`:

```
$ git show ca5b191:crates/slipfork/tests/critical_ospp_daemon.rs | grep -nE 'Duration|Instant|sleep|readiness|wait'
39:    run_command: "exec sleep 300"
79:    run_command: "exec sleep 300"
```

Those two hits are the fixture manifest's `run_command`, not a wait. The only 60 s readiness budget
in the tree is `slipfork-engine/src/reflink_netns.rs::OCI_SERVICE_READY_TIMEOUT`
(`Duration::from_secs(60)`), owned by the engine worker rewriting `crates/slipfork-engine/**`; the
suite's own daemon spawn already polls through
`slipfork_testkit::poll_until(DAEMON_STARTUP_DEADLINE, …)`. No third `poll_until` copy was added and
no `sleep()` padding exists.

## Acceptance (exact commands, exact exits)

| Command | Exit | Result |
|---|---|---|
| `! grep -rn 'test-server' crates/ Cargo.toml` | 0 | no matches |
| `! grep -rn 'features = \["test' crates/*/Cargo.toml` | 0 | no matches |
| `! grep -rn '#\[path = "\.\./\.\./\.\./' crates/` | 0 | no cross-package include |
| `! grep -riE 'option [ab]\|spike [ab]'` (this node's files) | 0 | no matches |
| `cargo check -p slipfork-ospp-testkit` | 0 | clean |
| `cargo clippy -p slipfork-ospp-testkit --all-targets -- -D warnings` | 0 | clean |
| `cargo test -p slipfork-ospp --no-run` | 0 | lib + `critical_ospp` build |
| `cargo test -p slipfork-ospp` | 0 | `1 passed` (lib), `32 passed` (`critical_ospp`), `0` doctests |
| `cargo test -p slipfork --test critical_ospp_daemon -- --test-threads=1` | see below | owned suite |
| `cargo check --workspace --all-targets` | 0 | clean, zero warnings |
| `cargo clippy --workspace --all-targets -- -D warnings` | 0 | clean |
| `cargo fmt --all -- --check` | 0 | clean |
| `make docs-check` | 0 | `No issues found`; `check_docs_honesty: OK (9 published doc(s) …)` |
| `uv run --no-project tools/guard.py` | 0 | `guard: OK (487 file(s) checked)` |
| `uv run --no-project tools/critical_guard.py --base ca5b191 --node M3.22` | **1** | both edited protected tests `[authorized …]`; 4 violations are other workers' files — see reds |
| `uv run --no-project tools/plan.py validate` | 0 | `plan OK` |
| `uv run --no-project --with pytest pytest -q tools/tests` | 0 | `105 passed` |

## Restated shipped-tree gate (lead ruling)

`cargo tree -p slipfork --edges normal --depth 1` shows only the seven direct normal dependencies
(`clap`, `hex`, `serde`, `serde_json`, `sha2`, `slipfork-{dag,edge,engine,ospp,snow,vcs,world}`) —
**no `slipfork-ospp-testkit`** (it is a dev-dependency) and **no new direct edge** to `tempfile`,
`tpmd-proto`, `tpmd-ipc`, `tungstenite`, `zstd`, or `p256`.

Exact inverse chains (`cargo tree -p slipfork --edges normal -i <crate>`):

```
tempfile      → warning: nothing to print.        (absent from the shipped tree)
tpmd-proto    → tpmd-proto ← tpmd-client ← slipfork-ospp ← slipfork
                tpmd-proto ← tpmd-ipc    ← slipfork-ospp ← slipfork
tpmd-ipc      → tpmd-ipc ← slipfork-ospp ← slipfork
tungstenite   → tungstenite ← slipfork-ospp ← slipfork
zstd          → zstd ← slipfork-ospp ← slipfork
p256          → p256 ← slipfork-ospp ← slipfork
```

`tpmd-proto` legitimately remains through the production `tpmd-client` / `tpmd-ipc` chain: it is the
wire contract the shipped `TpmdDeviceKeySigner` links. Every other listed crate was already a
normal `slipfork-ospp` production dependency at base; this node added none of them to `slipfork`,
and `tempfile` now has no normal edge at all.

## Honest reds

1. **`critical_guard.py` — 4 cross-node violations.** The tool reports
   `crates/slipfork-edge/tests/critical_edge.rs`, `crates/slipfork-engine/tests/critical_engine.rs`,
   `crates/slipfork-engine/tests/critical_oci_runtime.rs`, and
   `packages/opencode-slipfork/tests/coordinator.critical.test.ts` as `[VIOLATION: not covered by
   any node path]`. All four are concurrent workers' edits in the shared working tree; this node
   touched none of them. Both protected tests this node edits are reported authorized
   (`crates/slipfork-ospp/tests/**`, `crates/slipfork/tests/critical_ospp_daemon.rs`).
2. **`make check` is not swept end to end.** Under the node's load envelope its `rs-test` phase is
   the pre-`Btrfs` engine's fixed `WORLD_STOP_GRACE` residual; every deterministic phase was run
   individually instead (table above). The owned suite's own outcome is recorded verbatim below.

## Owned suite — exact outcome

`cargo test -p slipfork --test critical_ospp_daemon -- --test-threads=1`, load range **37–48**,
nothing retried:

```
test result: FAILED. 1 passed; 3 failed; 0 ignored; 0 measured; 0 filtered out
```

| Case | Outcome | Captured diagnostic |
|---|---|---|
| `dot_jj_repository_routes_to_jujutsu_and_never_falls_back_to_git` | FAILED | `slipfork: cannot destroy the base seed world: engine protocol/execution error: world init pid … is still alive 10s after SIGKILL` |
| `optional_slipfork_toml_boots_and_malformed_config_fails_closed` | FAILED | same `world init pid … is still alive 10s after SIGKILL` |
| `paired_ospp_resolves_remote_secrets_restores_a_real_dump_and_forks_rows` | FAILED | `slipfork: cannot start backing service 'pg_clusters.main' in the base seed world: engine protocol/execution error: service 'pg_clusters.main' did not accept connections on port 5432 within 60s; the process was stopped (captured logs retained)` |
| `unpaired_remote_strategy_and_refresh_dump_fail_closed` | passed | — |

All three reds are the pre-`Btrfs` engine's fixed windows — the 10 s `WORLD_STOP_GRACE` teardown
and the 60 s `OCI_SERVICE_READY_TIMEOUT` readiness budget — owned by `M3.13`/`M3.14`/`M3.21` and
covered by this node's load envelope. A separate run of the same command on the same tree at load
26–64 recorded `2 passed; 2 failed` (`optional_slipfork_toml…` passed) with the same two engine
windows; the variance is the foreign load, not the harness, and neither run was retried.

## Record fixes

- The base line numbers for the `run_command: "exec sleep 300"` hits are `39:` and `79:`, not
  `37:`/`77:` (the earlier record quoted the working-tree file after this node's header edit).
- `#![allow(dead_code)]` in `crates/slipfork/tests/support/mod.rs` pre-existed at base; this node
  neither added nor removed it.
