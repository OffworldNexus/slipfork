# Result M3.21 — Atomic scorched-earth deletion of IncusEngine, ReflinkNetnsEngine, and FICLONE helpers

Base commit `053596e`. Attempt 1/3. No commit made.

## 1. Deletion inventory

| Path | Change |
|---|---|
| `crates/slipfork-engine/src/incus.rs` | **deleted** (506 lines, `IncusEngine` + `/1.0` REST client + `IncusClonePayload`/`IncusCloneSource`/`incus_disk_device`) |
| `crates/slipfork-engine/src/reflink_netns.rs` | **deleted** (1880 lines, `ReflinkNetnsEngine` + its container/registry machinery) |
| `crates/slipfork-engine/src/lib.rs` | removed `pub mod incus;`, `pub mod reflink_netns;`, `pub use incus::IncusEngine;`, `pub use reflink_netns::ReflinkNetnsEngine;`; narrowed the `linux_cow` re-export to `pub use linux_cow::current_uid_gid;`; deleted `EngineError::DaemonUnavailable` (only `incus.rs` constructed it) and `EngineError::ReflinkUnsupported` (only the deleted FICLONE helpers constructed it) |
| `crates/slipfork-engine/src/linux_cow.rs` | deleted `FICLONE_IOCTL`, `ReflinkStats`, `reflink_file`, `reflink_tree`, `reflink_dir_inner`, `probe_reflink_dir`; dropped the now-unused `fs`/`File`/`OpenOptions`/`PermissionsExt`/`symlink`/`Path`/`Instant`/`EngineError` imports; rewrote the module doc to say `unsafe` is confined to this module **and `crate::btrfs`'s kernel `ioctl` driver** and to list only `current_uid_gid`, `signal_process`, and `send_fd`/`recv_fd` |
| `crates/slipfork-engine/Cargo.toml` | description now: `WorldEngine IoC port with the BtrfsEngine adapter: one Btrfs subvolume per world through the injected SubvolumeDriver` |

Kept in `linux_cow.rs` (all still used): `current_uid_gid`, `ProcessSignal` + `signal_process`, `STATUS_OK`, `send_fd`, `recv_fd`.
`ProcessIdentity` / `kill_and_reap_child` already live in `crates/slipfork-engine/src/service.rs` and are consumed there by `btrfs_engine.rs`; no move was needed.

Net diff in scope: **-2570 / +25** lines across the five files.

## 2. Dependency prune

No dependency was removable — every declared dependency is still used after the deletion:

| Dependency | Still used by |
|---|---|
| `hex` | `world_init.rs`, `rootfs/{mod,input,pin,cache,conformance}.rs`, `btrfs_engine.rs`, tests |
| `libc` | `linux_cow.rs`, `btrfs.rs`, `btrfs_engine.rs`, `world_init.rs` |
| `serde`, `thiserror` | `lib.rs` |
| `serde_json` | `oci.rs`, `rootfs/{dockerfile,cache}.rs`, `btrfs_engine.rs`, `bin/slipfork-world-init.rs` |
| `sha2` | `rootfs/{mod,input}.rs` |
| `slipfork-world` | `lib.rs`, `conformance.rs`, `btrfs_engine.rs` |
| dev: `base64` | `tests/critical_btrfs_live.rs` |
| dev: `slipfork-testkit`, `tempfile` | `btrfs_engine.rs` + engine test suites |

`tungstenite`/`tls` were never declared by this crate (OCI/OSPP paths do not need them here). `Cargo.lock` is unchanged.

## 3. fd-count test race fix

`crates/slipfork-engine/src/linux_cow.rs` tests counted process-wide `/proc/self/fd` entries, so sibling lib tests opening/closing descriptors in parallel produced `12 -> 13` / `16 -> 18`.

`open_descriptors()` is replaced by `descriptors_aliasing(fd: RawFd) -> usize`, which resolves the `/proc/self/fd/<fd>` link target (for a socket, `socket:[<inode>]`) and counts only descriptors whose link target is **identical**. The leak assertion in `recv_fd_rejects_extra_descriptors_without_leaking_them` now compares `(after_a, after_b)` with `(before_a, before_b)` by kernel socket identity, so unrelated descriptors opened by other tests can never perturb it, and a leaked `SCM_RIGHTS` duplicate is still caught (it would alias `fd_a`/`fd_b`'s inode). The other test (`recv_fd_accepts_exactly_one_descriptor`) never counted fds.

Determinism evidence — 20 consecutive runs at 16 test threads:

```
$ for i in $(seq 1 20); do cargo test -p slipfork-engine --lib linux_cow -- --test-threads=16; done
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 60 filtered out; finished in 0.00s   (x20)
```

## 4. Acceptance evidence (all commands run from the repo root)

`PATH` = `$HOME/.local/bin:$HOME/.cargo/bin:$HOME/.bun/bin:$PATH` (`uv`, `cargo`, `bun`).

| # | Command | Exit | Output |
|---|---|---|---|
| 1 | `test ! -e crates/slipfork-engine/src/incus.rs` | **0** | — |
| 2 | `test ! -e crates/slipfork-engine/src/reflink_netns.rs` | **0** | — |
| 3 | `! grep -rniE 'incus\|reflink\|ficlone\|statefulram\|snapshotmode\|zfs' crates/ packages/` | **1 (red)** | 3 hits, all outside this node's paths — see §5 |
| 4 | `cargo clippy --workspace --all-targets -- -D warnings` | **0** | `Finished dev profile` |
| 5 | `cargo test --workspace --lib --bins` | **0** | `slipfork-engine` lib `62 passed; 0 failed`; every other crate `ok` |
| 6 | `make check` | **0** | plan OK, guard OK (499 files), ruff OK, `113 passed`, check_support OK, `bun test` `16 pass 0 fail`, `cargo fmt --check` clean, zensical strict build, `check_docs_honesty: OK (9 published doc(s))` |
| 7 | `cargo fmt --all -- --check` | **0** | clean (also `rustfmt --check` on `lib.rs`/`linux_cow.rs`: 0) |
| 8 | `pytest -q tools/tests` | **0** | `113 passed in 13.54s` |
| 9 | `uv run --no-project tools/guard.py` | **0** | `guard: OK (499 file(s) checked)` |
| 10 | `uv run --no-project tools/critical_guard.py --base 053596e --node M3.21` | **1 (red)** | 6 unauthorized protected-test changes, all cross-node — see §5 |
| 11 | `uv run --no-project tools/plan.py validate` | **0** | `plan OK` |
| 12 | `cargo clippy -p slipfork-engine --all-targets -- -D warnings` | **0** | crate-scoped confirmation |

Symbol sweep (exit 1 = no matches):

```
$ grep -rn "IncusEngine\|ReflinkNetnsEngine\|reflink_tree\|probe_reflink_dir\|ReflinkStats\|reflink_file\|FICLONE_IOCTL\|DaemonUnavailable" crates/ packages/ | grep -v "TunnelError::DaemonUnavailable\|DaemonUnavailable(String)\|tunnel.rs:28"
refs_exit=1
```

The only surviving `DaemonUnavailable` occurrences belong to the unrelated `slipfork-world::TunnelError` / `slipfork-edge::tunnel` tunnel port, not to `EngineError`.

## 5. Residuals (cross-node red, recorded verbatim — not edited)

**(a) Acceptance grep #3 — 3 hits, all outside `M3.21`'s `paths`:**

```
crates/slipfork-engine/src/rootfs/mod.rs:82:    /// First 12 hex characters (`slipfork-l0-<digest12>` Incus image name, `ADR-0009` §3).
crates/slipfork/tests/support/mod.rs:307:        !diagnostic.to_lowercase().contains("reflink"),
packages/opencode-slipfork/tests/e2e.critical.test.ts:355:      expect(diagnostic.toLowerCase()).not.toContain("reflink");
```

- `rootfs/mod.rs:82` is a stale `Incus image name` doc phrase in a file owned by the docs-hygiene sweep (`M3.23`, whose `paths` include `crates/**`).
- The other two are *negative* assertions that a Btrfs refusal never advertises a reflink fallback; they are owned by `M3.22`/`M3.24` (`crates/slipfork/tests/support/mod.rs`) and `M3.23` (`packages/**`). Deleting the word there is their call, not a deletion of dead engine code.

**Correction (follow-up, 2026-11-17):** the workspace-wide acceptance grep is now **2** hits, both *negative* assertions that a Btrfs refusal never advertises a reflink fallback (`crates/slipfork/tests/support/mod.rs:307`, `packages/opencode-slipfork/tests/e2e.critical.test.ts:355`) — deleting the word there would weaken a contract, so that acceptance line remains a documented cross-node residual owned by `M3.22`/`M3.23`.

**(b) `critical_guard.py --base 053596e --node M3.21` — exit 1, 6 violations, none from this node:**

```
critical-guard: 053596e..working tree, 46 changed file(s)
  M  crates/slipfork/tests/critical_cli.rs  [VIOLATION: not covered by any node path]
  M  crates/slipfork/tests/critical_l0_wiring.rs  [VIOLATION: not covered by any node path]
  M  crates/slipfork/tests/critical_ospp_daemon.rs  [VIOLATION: not covered by any node path]
  M  crates/slipfork/tests/critical_slug.rs  [VIOLATION: not covered by any node path]
  M  crates/slipfork/tests/critical_vcs_remote.rs  [VIOLATION: not covered by any node path]
  M  packages/opencode-slipfork/tests/e2e.critical.test.ts  [VIOLATION: not covered by any node path]
  A  crates/slipfork/tests/critical_btrfs_live.rs  [added (always allowed)]
critical-guard: FAILED: 6 unauthorized protected test change(s) in M3.21.
```

These are the concurrent worker's in-flight `crates/slipfork/src/**` + tests re-homing (plus the `packages/**` e2e file); this node touched none of them. Its own files (`crates/slipfork-engine/{Cargo.toml,src/lib.rs,src/linux_cow.rs}` + two deletions) are not protected tests.

**(c) Transient cross-node red observed mid-run and since resolved by the other worker:** `cargo clippy --workspace` failed with `error[E0603]: struct DaemonPorts is private` / `DaemonConfig is private` / `error[E0624]: associated function new is private` (`crates/slipfork/tests/support/mod.rs:508` vs `crates/slipfork/src/daemon.rs:546`), and `cargo fmt --all -- --check` flagged `crates/slipfork/src/daemon.rs:464`. Both are green in the final run above.

## 6. Notes

- `EngineError::Unsupported` was kept: `conformance.rs` (2 sites) and `crates/slipfork-edge/tests/critical_edge.rs` still construct it.
- `BtrfsEngine` remains the sole `WorldEngine` implementation in the workspace; no fallback path, alias, or shim was introduced.
- No file outside the node's `paths` was modified.

## 7. Scope widening + Quad-gate fixes (lead-applied)

This section records the lead's scope widening and the Quad-gate fixes applied on top of §§1–6. Nothing above is rewritten; where a statement is superseded it is marked here.

1. **Scope widened — the diff is 9 files, not 5.** On `2026-11-17T12:17` the lead widened this node's `paths` (`plan/nodes/M3/M3.21/NODE.md`, `updated: "2026-11-17T12:17"`) to add `crates/slipfork-engine/src/rootfs/mod.rs`, `crates/slipfork-engine/src/rootfs/dockerfile.rs`, `crates/slipfork-engine/src/rootfs/input.rs`, and `crates/slipfork-engine/src/bin/slipfork-world-init.rs`, because this node's own acceptance grep (`! grep -rniE 'incus|reflink|ficlone|statefulram|snapshotmode|zfs' crates/ packages/`) is `crates/`-wide and therefore reaches those files. The node diff is consequently **9 files**: `git diff --stat 053596e -- crates/slipfork-engine` reports `9 files changed, 44 insertions(+), 2588 deletions(-)` (`Cargo.toml`, `src/{lib,linux_cow,incus,reflink_netns}.rs`, `src/rootfs/{mod,dockerfile,input}.rs`, `src/bin/slipfork-world-init.rs`). The "five files" and "-2570 / +25" figures in §1 describe only the original five-file core and are superseded by the 9-file diff above.

2. **`rootfs/mod.rs:82` "Incus image name" residue — fixed.** The doc comment now reads: *"First 12 hex characters of the digest — a compact label for diagnostics; the cache directory itself is named by the full [`Self::hex`] (`ADR-0009` §3)."* The cache directory is indeed named by the full `hex()` digest per `rootfs/cache.rs:44` (`self.root.join("rootfs").join(digest.hex())`). The residue row for `rootfs/mod.rs:82` in §5(a) is therefore **superseded**; it is kept above as history only.

3. **`src/lib.rs` crate rustdoc — singular engine.** The crate doc now names *"its single shipped implementation, [`BtrfsEngine`], over an injected `SubvolumeDriver`"*; the plural "adapters" wording is gone.

4. **`src/bin/slipfork-world-init.rs:14` — both audited `unsafe` modules named.** The doc now reads that `linux_cow.rs` **and** `btrfs.rs` *"are the two audited `unsafe` modules in the workspace"*.

5. **Dead test-only scaffolding removed.** In `rootfs/dockerfile.rs` the test-only `stage_names` helper and its `#[cfg_attr(not(test), allow(dead_code))]` are deleted; `parses_named_stages_and_continuations` now asserts stage names directly from `stages` (`dockerfile.stages.iter().map(|stage| stage.name.as_deref())`). In `rootfs/input.rs` the no-op `#[cfg_attr(not(test), allow(dead_code))]` above `#[cfg(test)] mod tests` is deleted.

6. **`src/linux_cow.rs` — SAFETY proof for the test-only `mem::zeroed()`.** A `// SAFETY:` proof was added above the `unsafe { mem::zeroed() }` in `mod tests` (the module doc promises one proof per `unsafe` block); the test logic itself is unchanged.

7. **§2 dependency-attribution rows corrected.**
   - `hex`: the `hex` crate is referenced only by `rootfs/mod.rs` and `rootfs/input.rs` (`hex::encode`). `rootfs/{pin,cache,conformance}.rs` and `btrfs_engine.rs` use the `RootfsDigest::hex()` accessor instead, and the old row's `world_init.rs` mention was a local `let hex` (a parsed digest string), not the crate. The dependency is still required by `rootfs/{mod,input}.rs`.
   - `libc`: used by `linux_cow.rs`, `btrfs.rs`, `btrfs_engine.rs`, **and** `src/bin/slipfork-world-init.rs` (the old row omitted the bin).
   - The conclusion stands: **no dependency was removable**; `Cargo.lock` is unchanged.

8. **Follow-up gate evidence (recorded verbatim for this node):**

   | Command | Exit | Result |
   |---|---|---|
   | `cargo test -p slipfork-engine --lib` | **0** | `62 passed; 0 failed` |
   | `cargo clippy -p slipfork-engine --all-targets -- -D warnings` | **0** | clean |
   | `cargo fmt --all -- --check` | **0** | clean |
   | `grep -rniE 'incus\|reflink\|ficlone\|zfs' crates/slipfork-engine/src/` | **1** | no hits |
   | `grep -rn 'allow(dead_code)' crates/slipfork-engine/src/` | **1** | no hits |

   The crate-scoped `Incus`/`Reflink`/`FICLONE`/`ZFS` sweep over `crates/slipfork-engine/src/` is now empty, so the only remaining hits for the node's acceptance grep are the two cross-node negative assertions recorded in the §5(a) correction above.
