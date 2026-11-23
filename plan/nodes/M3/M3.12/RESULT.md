# Result M3.12

Status: **code complete after the Quad Review gate (legality PASS with 2 minors, ioc FAIL, cruft FAIL
— all seven gate fixes applied in one pass) plus the four functional-reviewer follow-ups (path-agnostic
`BtrfsRequired` Display, name validation before any filesystem access, `non_btrfs` `list_children`
gate, and the `ARCHITECTURE.md` §1 double-compile rule).** The `SubvolumeDriver` seam exists with
exactly one production implementation (`KernelBtrfsDriver`) and one shared test driver
(`TestTreeDriver` in the `publish = false` `slipfork-testkit` crate). Branch `release/0.1.0`, base
`6827c51`. No commit.

## What changed

| File | Change |
|---|---|
| `crates/slipfork-engine/src/btrfs.rs` (new) | The sole Btrfs probe & CoW seam: `BTRFS_SUPER_MAGIC`, `BTRFS_MOUNT_HINT`, `BtrfsProbeReport`, `ProbeFacts`, `SubvolumeDriver` (`probe_facts` required + `probe` **provided**), `require_btrfs`, `probe_with`, and `KernelBtrfsDriver` — the only production implementation. All `unsafe` (`statfs`, `statvfs`, five `BTRFS_IOC_*` wrappers) lives here with a per-call `// SAFETY:` proof. |
| `crates/slipfork-engine/src/lib.rs` | `pub mod btrfs;` and the three `EngineError` variants exactly as `ARCHITECTURE.md` §3.3 declares them (`BtrfsRequired`, `SubvolumeDestroyDenied`, `Ioctl`). No duplicate root re-exports: btrfs items have one import path, `slipfork_engine::btrfs::*`. |
| `crates/slipfork-testkit/src/btrfs.rs` (new) | `TestTreeDriver` + `SubvolumeOp` journal: a directory-backed `SubvolumeDriver` with no kernel ioctl, on-disk subvolume identity, and its own lifecycle unit tests. |
| `crates/slipfork-testkit/src/lib.rs` | `pub mod btrfs;` and a crate-doc sentence naming the driver. |
| `crates/slipfork-testkit/Cargo.toml` | `libc` + `slipfork-engine` normal dependencies, `tempfile` dev-dependency. `publish = false` and the empty feature set are unchanged. |
| `ARCHITECTURE.md` §3.3 | Minimal listing amendment (lead-ruled refinement, one clause, no new section): `ProbeFacts`, `require_btrfs`, required `probe_facts`, and `probe` marked as a provided method. Every other signature, including `probe_with`, is unchanged. |
| `ARCHITECTURE.md` §1 | One normative sentence recording the dev-dependency double-compile rule (engine →dev→ testkit →normal→ engine compiles `slipfork_engine` twice; testkit doubles are consumed from integration suites, `--lib` unit tests use crate-internal fakes), stated once for the whole workspace. |
| `crates/slipfork-engine/Cargo.toml` | **Unchanged** — `libc`, `tempfile`, and the `slipfork-testkit` dev-dependency were already declared. |
| `Cargo.lock` | `slipfork-testkit` now lists `libc`, `slipfork-engine`, `tempfile`. |

`cargo tree -p slipfork --edges normal -i slipfork-testkit` → `warning: nothing to print.` — the test
driver never enters the shipped tree.

## Design

### Exactly one probe producer

`SubvolumeDriver` now carries the whole probe contract:

- `probe_facts(&self, path) -> Result<ProbeFacts, EngineError>` is **required** — a driver supplies
  only its own substrate's `statfs`/`statvfs` evidence (`probed_path`, `fstype`, `noatime`).
- `probe(&self, path) -> Result<BtrfsProbeReport, EngineError>` is **provided** and is the single
  code path that builds a `BtrfsProbeReport`: it rejects a non-absolute `path`
  (`EngineError::Io`, nothing is ever resolved against the ambient `current_dir()`), takes
  `probe_facts`, and only when `fstype` is Btrfs creates `state_dir` if needed and runs the scratch
  round-trip.

There is no public `probe_report` helper any more; `btrfs::probe_with(&dyn SubvolumeDriver,
state_dir)` is the injected seam and simply calls `driver.probe(state_dir)`.

`probe` semantics, exactly `ADR-0010` §4:

1. `probed_path` = `state_dir` when it exists as a directory, else its nearest existing ancestor.
2. `fstype` = raw `statfs(probed_path).f_type`; `btrfs_super_magic = is_btrfs(fstype)`.
3. `noatime` = `statvfs(probed_path).f_flag & ST_NOATIME` (advisory only).
4. Off Btrfs: `Ok(BtrfsProbeReport { btrfs_super_magic: false, user_subvol_rm_allowed: None, .. })`
   and **nothing is created** — the shared driver proves it by journalling zero subvolume calls.
5. On Btrfs: `<state_dir>/.doctor-probe-<pid>` create → read-only snapshot
   `.doctor-probe-<pid>-snap` → destroy (clearing `BTRFS_SUBVOL_RDONLY`) → destroy the probe
   subvolume. `Some(Ok(()))` on success, `Some(Err(errno))` with the first failing step's raw errno
   otherwise.
6. **Cleanup is never silently discarded.** Each name still marked created is destroyed again; if a
   cleanup destroy fails and the failure is not already the returned destroy error, the returned
   error names the residue paths (`EngineError::Io` listing them). A successful round-trip leaves
   zero residue, and a `state_dir` this call created is removed again when it is empty (a non-empty
   one holds the reported destroy residue or foreign content, and is never deleted).

### Exactly one magic gate

`require_btrfs(fstype, path) -> Result<(), EngineError>` owns the fail-closed
`BtrfsRequired { path, actual_fstype, mount_hint: BTRFS_MOUNT_HINT }` construction, and `is_btrfs`
owns the single `f_type == 0x9123683e` comparison (also used for
`BtrfsProbeReport::btrfs_super_magic`). `ensure_supported` (production and test driver) and
`list_children` call `require_btrfs`; no inline copy remains.

### `KernelBtrfsDriver` lifecycle (`ADR-0010` §3)

| Method | Kernel operation |
|---|---|
| `probe_facts` | `statfs`/`statvfs` on the nearest existing ancestor of an absolute path. |
| `ensure_supported` | `require_btrfs(statfs(path).f_type)` only; no scratch round-trip, so a mount without `user_subvol_rm_allowed` still boots. |
| `create_subvolume` | `BTRFS_IOC_SUBVOL_CREATE_V2`, `flags = 0`, on the parent directory fd. |
| `snapshot_ro` | `BTRFS_IOC_SNAP_CREATE_V2`, `flags = BTRFS_SUBVOL_RDONLY` (`1 << 1`), source subvolume fd. |
| `snapshot_writable` | `BTRFS_IOC_SNAP_CREATE_V2`, `flags = 0`. Distinct signature; no mode flag anywhere. |
| `destroy_subvolume` | `BTRFS_IOC_SUBVOL_GETFLAGS` → `BTRFS_IOC_SUBVOL_SETFLAGS(flags & !BTRFS_SUBVOL_RDONLY)` when read-only → `BTRFS_IOC_SNAP_DESTROY_V2`. |
| `list_children` | `require_btrfs` gate, then `readdir` + `lstat` inode classification (below). |

Hardening added in this pass:

- Every subvolume name is validated by `vol_args` **before** any `open_dir` in `create_subvolume`,
  `snapshot_ro`/`snapshot_writable`, and `destroy_subvolume`, so `""`, `.`, `..`, and absolute names
  never reach the filesystem — proven with a nonexistent parent, where an open would fail `ENOENT`
  but the returned error is the name error.
- `_IOC` encoding is asm-generic only: `#[cfg(not(any(target_arch = "x86_64", target_arch =
  "aarch64")))] compile_error!("M3.12 ioctl encoding is asm-generic only")`.
- `f_type` is read as `as u32 as i64`, so a magic such as `0x9123683e` is identical on 32- and
  64-bit targets instead of being sign-extended.
- A snapshot source descriptor of `0` (stdin closed, so `open` handed out fd 0) is rejected with
  `EBADF` instead of being re-read as the kernel's "no source subvolume" encoding.
- One `open_dir` helper opens every directory with
  `O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW`, so a symlink is never followed into a different
  subvolume and no descriptor leaks into children.
- Request numbers are computed by a `const fn ioc(dir, nr, size)` and pinned by a unit test to the
  values `cc` produced from `/usr/include/linux/btrfs.h` on this host (`SNAP_CREATE_V2 0x50009417`,
  `SUBVOL_CREATE_V2 0x50009418`, `SUBVOL_GETFLAGS 0x80089419`, `SUBVOL_SETFLAGS 0x4008941a`,
  `SNAP_DESTROY_V2 0x5000943f`); `struct btrfs_ioctl_vol_args_v2` is `#[repr(C)]` with a
  compile-time `assert!(size_of == 4096)` and a NUL-terminated name at offset 56, matching the
  header.

Subvolume names are validated before any syscall (`vol_args`): 1..=4039 bytes, no interior NUL, not
absolute, no empty/`.`/`..` component — an invalid name is an `EngineError::Io`, never a kernel call.

### Error mapping (`ADR-0010` §4)

| Condition | Error |
|---|---|
| `statfs`/`statvfs`/`open`/`BTRFS_IOC_*` fails outside the destroy path (`ENAMETOOLONG`, `EROFS`, `ENOTEMPTY`, `ENOSPC`, `EIO`, `EBUSY`, …) | `EngineError::Ioctl { op, path, errno }` |
| `f_type != BTRFS_SUPER_MAGIC` in `ensure_supported` / `list_children` | `EngineError::BtrfsRequired { path, actual_fstype, mount_hint: BTRFS_MOUNT_HINT }` |
| `EPERM` anywhere inside `destroy_subvolume` (open / getflags / setflags / snap_destroy) | `EngineError::SubvolumeDestroyDenied { path, errno, mount_hint: BTRFS_MOUNT_HINT }` |
| Any other errno inside `destroy_subvolume` | `EngineError::Ioctl { op, path, errno }` |
| Invalid subvolume name, non-absolute probe path, unremovable probe state dir, scratch residue | `EngineError::Io { path, message }` (no syscall attempted for a bad name) |

`BTRFS_MOUNT_HINT` is exactly `sudo mount -o user_subvol_rm_allowed,noatime /dev/<device> <state_dir>`
and is the single text both variants carry. `BtrfsRequired`'s `Display` is path-agnostic —
``{path} is not on Btrfs: statfs reports f_type {actual_fstype:#x}, not BTRFS_SUPER_MAGIC
0x9123683e; fix: {mount_hint}`` — because `{path}` can be `worlds/`, `snapshots/`, or a probe path,
not only the state directory.

### What `list_children` honestly returns

Direct child **subvolume roots** of `parent`, sorted by path, and nothing else. The classification is
`readdir` + `lstat` (`DirEntry::metadata`, no symlink traversal): a directory whose inode number is
`BTRFS_FIRST_FREE_OBJECTID` (`256`) is a subvolume root, because every Btrfs subvolume root has
objectid 256 — the first objectid the filesystem hands to user-visible inodes — so no plain directory
inside `parent` can carry it. The parent must be on Btrfs or the call fails closed with
`BtrfsRequired`. Plain directories, files (`.seed.lock`), and symlinks are skipped; nested
subvolumes are not descended into. There is no shell-out and no privileged tree search
(`BTRFS_IOC_TREE_SEARCH_V2` needs `CAP_SYS_ADMIN`), so the method works as the unprivileged
`$UID:$GID` daemon. Documented consumers: the `M3.14` engine suite (sealed-base names) and the Phase 2
`M3.2.13` GC (snapshot lineage). Live confirmation on a real Btrfs mount is `M3.19`'s.

### Test driver (`crates/slipfork-testkit::btrfs`)

`TestTreeDriver` emulates the same observable contract with real directories and zero ioctl:

- **Identity is on disk.** Creating or cloning a subvolume writes the sibling marker
  `.<name>.slipfork-subvol` in its parent. `list_children` reports exactly the directories carrying
  that marker (plain directories, files, and symlinks are ignored) and `destroy_subvolume` resolves
  identity from it, so a **second driver instance over the same state directory** lists and destroys
  what the first one created — exactly like the kernel.
- **Read-only seal.** `snapshot_ro` copies the tree and chmods the snapshot **root** to `0o555`;
  `snapshot_writable` uses `0o755`. The kernel seals the whole subvolume and this double seals the
  root, so nested files are not individually sealed: the contract `M3.14` asserts is that a write
  **at the snapshot root** fails (`EACCES`) while the sealed payload stays readable. Cloning onto an
  existing path fails with `EEXIST`.
- `destroy_subvolume` clears the owner-write bit (`SubvolumeOp::ClearReadonly`) **before** removing
  the tree (`SubvolumeOp::DestroySubvolume`), so `ADR-0010` §3's ordering is asserted through the
  injected driver.
- `probe_facts` reports the emulated mount facts; the provided `probe` then runs the shared scratch
  round-trip, so `doctor`'s field mapping is exercised end to end.
- **The same fail-closed gates as the kernel driver.** `non_btrfs(..)` returns `BtrfsRequired` from
  `ensure_supported` **and** from `list_children` (never an empty list), so a suite can assert that
  contract path with the shared double instead of a bespoke fake.
- `btrfs()` / `non_btrfs(fstype)` / `with_destroy_errno(errno)` are typed constructors (no boolean
  bag, no Cargo feature); every call is journalled for ordering assertions.

## Unit-test evidence

`cargo test -p slipfork-engine --lib btrfs` — 12 passed, 0 failed, 28 filtered out:

```
btrfs::tests::ioctl_requests_match_the_linux_header_encoding ... ok
btrfs::tests::btrfs_magic_and_mount_hint_are_the_documented_constants ... ok
btrfs::tests::require_btrfs_accepts_only_the_btrfs_super_magic ... ok
btrfs::tests::non_absolute_probe_paths_are_rejected_without_touching_the_cwd ... ok
btrfs::tests::kernel_probe_reports_the_host_contract_without_leaving_residue ... ok
btrfs::tests::ensure_supported_agrees_with_probe_on_this_host ... ok
btrfs::tests::ioctl_errnos_map_to_typed_ioctl_errors ... ok
btrfs::tests::destroy_eperm_maps_to_subvolume_destroy_denied ... ok
btrfs::tests::error_display_carries_the_mount_remediation ... ok
btrfs::tests::invalid_subvolume_names_are_rejected_before_any_filesystem_access ... ok
btrfs::tests::subvolume_names_must_be_relative_components ... ok
btrfs::tests::nearest_existing_ancestor_walks_up_to_the_first_existing_directory ... ok
```

`cargo test -p slipfork-testkit` — 14 passed (12 driver tests + the 2 existing `poll_until` tests):
`non_btrfs_driver_reports_its_emulated_fstype_and_touches_no_subvolume`,
`non_btrfs_driver_fails_closed_from_list_children`,
`btrfs_driver_probe_runs_the_scratch_round_trip_and_leaves_no_residue`,
`probe_records_the_raw_errno_of_a_failing_scratch_round_trip`, `probe_rejects_a_relative_path`,
`ensure_supported_is_magic_only_and_ignores_the_scratch_round_trip`,
`read_only_snapshot_is_cleared_before_destroy`,
`driver_destroy_denied_by_the_mount_maps_to_subvolume_destroy_denied`,
`destroying_an_unknown_subvolume_fails_closed`,
`writable_clone_of_a_read_only_snapshot_is_writable`,
`list_children_returns_only_subvolume_roots_sorted`,
`a_second_driver_instance_lists_and_destroys_what_the_first_created`.

Host evidence: `stat -f -c 'fstype=0x%t'` on both `$HOME/.cache/slipfork-tmp` and the repository
reports `0x58465342` (XFS), so `probe` takes the off-Btrfs branch here: `btrfs_super_magic == false`,
`user_subvol_rm_allowed == None`, and the state directory is never created.

## Acceptance (exact commands, exact exits)

| Command | Exit | Result |
|---|---|---|
| `cargo check -p slipfork-engine` | 0 | clean |
| `cargo test -p slipfork-engine --lib btrfs` | 0 | `12 passed; 0 failed; 28 filtered out` |
| `cargo test -p slipfork-testkit` | 0 | `14 passed; 0 failed` |
| `cargo clippy -p slipfork-engine --all-targets -- -D warnings` | 0 | clean |
| `grep -n 'BTRFS_SUPER_MAGIC' crates/slipfork-engine/src/btrfs.rs` | 0 | 9 matches |
| `grep -n 'BTRFS_IOC_SNAP_CREATE_V2' …` | 0 | 5 matches |
| `grep -n 'BTRFS_IOC_SUBVOL_SETFLAGS' …` | 0 | 4 matches |
| `grep -n 'BTRFS_IOC_SNAP_DESTROY_V2' …` | 0 | 4 matches |
| `grep -n 'BtrfsRequired' …` | 0 | 9 matches |
| `grep -n 'user_subvol_rm_allowed,noatime' …` | 0 | 4 matches |
| `grep -c 'SAFETY:' crates/slipfork-engine/src/btrfs.rs` | 0 | **10** ≥ 9 `unsafe {` blocks |
| `cargo check --workspace --all-targets` | 0 | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | 0 | clean |
| `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` | 0 | clean |
| `cargo fmt --all -- --check` | 0 | clean |
| `cargo test --workspace --lib --bins` | 0 | every suite `ok` (engine lib `40 passed`, testkit `14 passed`) |
| `cargo test --workspace --doc` | 0 | all `0 passed` |
| `make docs-check` | 0 | `No issues found`; `check_docs_honesty: OK (9 published doc(s) …)` |
| `uv run --no-project --with pytest pytest -q tools/tests` | 0 | `112 passed` |
| `uv run --no-project tools/guard.py` | 0 | `guard: OK (490 file(s) checked)` |
| `uv run --no-project tools/plan.py validate` | 0 | `plan OK` |
| `uv run --no-project tools/critical_guard.py --base 6827c51 --node M3.12` | 0 | `no change to a protected critical test` |
| `! grep -riE 'option [ab]\|spike [ab]'` (this node's files) | 0 | no matches |

`make check` was run component-wise (every non-world-booting phase above) rather than as one sweep:
its `rs-test` phase boots real worlds through the pre-`Btrfs` engines still present until `M3.21`,
which this node's load envelope and the dispatch brief exclude. `bun` is not installed on this host,
so `make ts-test` could not run; no TypeScript file was touched.

## Honest reds and residuals

1. **No live Btrfs on this host.** The kernel ioctl paths (`SUBVOL_CREATE_V2`, `SNAP_CREATE_V2`
   with/without `BTRFS_SUBVOL_RDONLY`, `GETFLAGS`/`SETFLAGS`, `SNAP_DESTROY_V2`) and the inode-256
   `list_children` classification are compiled, encoded against the real header, and unit-tested at
   the mapping/encoding level, but are **not executed** here: `/` is XFS and there is no
   `/dev/btrfs-control`. `M3.19`'s `critical_btrfs_live` is the live proof.
2. **Engine `--lib` unit tests cannot consume the shared testkit driver.** The dev-edge cycle
   (`slipfork-engine` →dev→ `slipfork-testkit` →normal→ `slipfork-engine`) compiles the engine
   **twice** for `cargo test -p slipfork-engine --lib`, so `TestTreeDriver` implements a *different*
   `SubvolumeDriver` instance than the test crate sees. Engine `--lib` tests therefore own the pure
   mapping/gate/encoding assertions, and the full `TestTreeDriver` lifecycle is unit-tested inside
   `slipfork-testkit`. **Verified by a throwaway integration probe (created, run green, deleted):**
   an engine integration test *can* pass `&TestTreeDriver as &dyn SubvolumeDriver`, so
   `M3.14`/`M3.15` can use the shared driver as planned. The rule is now stated once for the whole
   workspace in `ARCHITECTURE.md` §1; the `NODE.md` wording "used by engine lib unit tests" is the
   one thing that does not hold.
3. **`crates/slipfork-engine/src/linux_cow.rs` module doc is stale.** It still says it is "the
   **only** module in the workspace permitted to contain `unsafe` code"; `ARCHITECTURE.md` §2 already
   names two (`btrfs.rs` + `linux_cow.rs`). That file is outside this node's paths (`M3.21` owns it).
4. **The test double seals only the snapshot root.** The kernel seals the whole subvolume; the double
   chmods the root, so nested files stay writable. Documented in the driver and in `RESULT.md`; the
   `M3.14` assertion is "a write at the snapshot root fails", and real whole-subvolume sealing is
   `M3.19`'s live proof.
5. **`EngineError::SubvolumeDestroyDenied` covers the whole destroy path.** `ADR-0010` §4 names the
   `SETFLAGS`/`SNAP_DESTROY_V2` failures; an `EPERM` from `open`/`GETFLAGS` inside
   `destroy_subvolume` maps to the same variant because the remediation is identical.
6. **Probe scratch names use the process id.** Two concurrent probes in the *same* `state_dir` would
   collide on `.doctor-probe-<pid>`; `doctor` is a single pre-daemon CLI probe, and a second
   concurrent probe fails closed with `EEXIST` rather than corrupting state.
7. **`list_children` and `ensure_supported` are fail-closed on `statfs` errors** (`Ioctl`), not on a
   guessed filesystem; `ensure_supported` deliberately does not walk ancestors (the daemon ensures
   `<state_dir>` exists first), while `probe` does.
8. **Two `ARCHITECTURE.md` edits, both lead-ruled and minimal.** §3.3 records the probe refinement
   (`ProbeFacts`, `require_btrfs`, required `probe_facts`, provided `probe`) and §1 records the
   dev-dependency double-compile rule once for the workspace. `ADR-0010` §4's report fields and the
   `probe`/`ensure_supported` split are unchanged.
