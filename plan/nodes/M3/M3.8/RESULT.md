# Result M3.8 — `docs/design/world-base-image.md` on unified Btrfs subvolumes (attempt 3/3)

> **Attempt 3 supersedes the record below** for every point it touches: the attempt-2 document was
> accepted by the functional reviewer and then failed the IOC gate (one MAJOR ownership conflict),
> the cruft gate (`C1`–`C8`) and the legality gate (`L1`–`L6`). Those three lists are dispositioned in
> [Attempt 3 disposition table](#attempt-3-disposition-table) at the end of this file; the attempt-2
> table is kept as history.

## Summary

Attempt 2 verified the consolidated fix pass on `docs/design/world-base-image.md` against the
attempt-1 Quad findings and the `M3.25` rulings, then applied exactly one correction: the
`doctor` surface no longer declares a `world_rootfs_crun` field. `doctor` renders
`unshare_userns` and `netns` from `HostProbe` plus the `BtrfsProbeReport` fields, and emits **no**
`crun` field at all, because static `crun` is a provisioned world-rootfs asset
(`crates/slipfork::runtime::runtime_assets` → `<state_dir>/cache/runtime/crun-…bin` →
`/.slipfork/bin/crun`), never a probe target (`ADR-0010` §4, `ADR-0009` §1). The earlier
"M3.15 owns that field / `ADR-0010` predates it" claim is gone from both the document and this
record.

Everything else was verified as already correct and left unchanged: the builder/engine boundary,
the base key and seed recipe, the three-stage host truth, staged prune, the error-string
catalogue, the mount/`list_children` owners, the node table, the benchmark citation, and the
terminology rule.

Scope: `docs/design/world-base-image.md` + this RESULT. No code, ADR, `ARCHITECTURE.md`,
`Makefile`, or `plan/` node was edited. No commit.

## Disposition Table (one row per required finding)

| # | Required disposition | Status | Evidence in `docs/design/world-base-image.md` |
|---|---|---|---|
| a | Builder stops at `cache/{rootfs,oci}`; seed + seal solely on the engine path | **Fixed** | §2.5 "The builder stops at plain directories" (`DockerfileStageBuilder` holds no `SubvolumeDriver`, produces only `cache/rootfs/<digest>/app` and `cache/oci/<sha256>/rootfs`, never creates/seeds/seals/destroys a subvolume); §3.1 "The builder's work ends here"; §2.3 seed row (`BtrfsEngine` owns seed → restore → `freeze_world`); §2.3.2 `WorldSpec.base_seed` carries every seed input; §7.2 #2 builder ends at the cache rename |
| b | Rule 3 static `RootAtRuntimeEntrypoint` only + boot-time `EROFS`/`EACCES`; no invented variant | **Fixed** | §5.2 rule 3 ("Static, pre-boot (the only `BaseImageError` check)" for `gosu`/`su-exec`/`sudo`/`chpst`; "Boot, no `BaseImageError`" for `EROFS`/`EACCES`; failure variant `RootAtRuntimeEntrypoint` (static only)); §4.1 #3 (no recursive `chmod`, no pre-boot path scan, `M3.25`); §6.7 |
| c | Staged prune GC owned by `M3.2.13`; Phase-1 prune reaps detached worlds only; `M3.19` asserts only its own subvolumes | **Fixed** | §7.2 #4 Phase 1 (`DaemonControl::prune` reaps detached world records only, `PruneReport.snapshots` empty, sealed bases survive, `M3.19` asserts only the scenario's own world subvolumes); §7.2 #4 Phase 2 (`M3.2.13` owns `prune.rs`/`PrunePlan`; `DaemonService::prune` drives `prune_snapshots`, `prune_artifacts`, `BaseImageBuilder` cache pruning); §8.4 #5 (scenario destroys its own snapshot, then worlds, then `prune --yes`; base presence asserted, not a globally empty list) |
| d | Base key = project + rootfs digest + service-image identity + seed-dump digest; `pg_restore`→`CHECKPOINT`→`freeze_world`; post-GC re-seed from `<state_dir>/dumps` | **Fixed** | §2.3.1 formula: `project`, `l0_merkle_sha256`, length-prefixed `(key, oci.digest)` pairs, length-prefixed `(db.id, "clean" \| dump_sha256_compressed)`; §2.3 seed row runs `pg_restore` then `CHECKPOINT;` then `freeze_world` on the seed world; §7.2 #4 "Re-seed after collection" re-materializes `.pgcustom` via `SeedDumpStore::materialize` and re-runs restore → `CHECKPOINT` → `freeze_world` |
| e | No fabricated `<path>: ` error prefix; TOML-parse vs daemon world-selection distinguished | **Fixed** | §6 preamble: CLI prints `slipfork: ` + `Display`; `BaseImageError` begins with its own `error:`; the catalogued `WorldConfigError` diagnostics have no prefix and no path. §6.1 separates `WorldConfigError::NoBuildableImage` on `up` from `BaseImageError::NoBuildableImage` in the builder. No transcript carries a `<path>: ` prefix; only daemon world-selection variants are catalogued, so no parse-time path rendering is asserted |
| f | `doctor` fields labelled `M3.15` target state with the `HostProbe`/`BtrfsProbeReport` split and no host-`crun` probe | **Fixed (corrected this pass)** | §2.1 #2 (`M3.15` target state; `BtrfsProbeReport` fields incl. `user_subvol_rm_allowed: Option<Result<(), i32>>`); §2.1 line 84 now: only `unshare_userns` + `netns` from `HostProbe`, and "`doctor` emits no `crun` field and performs no host `crun` probe" with `crun` named as a provisioned world-rootfs asset (`ADR-0010` §4, `ADR-0009` §1). `world_rootfs_crun` no longer appears anywhere |
| g | Three-stage host truth: boot magic-only → `doctor` scratch round-trip → destroy `EPERM` | **Fixed** | §2.1 #1 magic-only `ensure_supported` (no scratch round-trip; a mount lacking `user_subvol_rm_allowed` boots); #2 `doctor` live create → RO snapshot → clear-RO → destroy round-trip; #3 first unprivileged destroy fails closed with the same mount hint; §6.12 |
| h | Single owner for mount order and `list_children` filtering | **Fixed** | §4.1 #5 "One Ordered Mount Manifest — the Sole Owner of Post-Overlay Mount Order" (fixed prologue + one ordered manifest in `world-init.json`, no second table); §2.5 "One enumeration-filter owner" (`list_children` filters nothing; `BtrfsEngine` owns the exclusion policy and hands a world set to GC) |
| i | Node table accurate | **Verified, no change** | §9 rows checked against `plan/nodes/M3/M3.{7..25}/NODE.md` frontmatter: every `id`, `depends_on`, and `(done)` marker matches (`M3.7 []` done; `M3.8/M3.9/M3.10 [M3.7, M3.25]`; `M3.11 [M3.7]`; `M3.12 [M3.8, M3.9, M3.20]`; `M3.13 [M3.12]`; `M3.14 [M3.13]`; `M3.15 [M3.14]`; `M3.16 [M3.10]`; `M3.17 [M3.21]`; `M3.18 [M3.17]`; `M3.19 [M3.11, M3.16, M3.18]`; `M3.20 [M3.1, M3.7]`; `M3.21 [M3.14, M3.15]`; `M3.22 [M3.1, M3.15]`; `M3.23 [M3.11, M3.18, M3.19, M3.21, M3.22]`; `M3.24 [M3.1]`; `M3.25 [M3.7]`) |
| j | Benchmark figures are a citation of the research report, not copies | **Fixed** | §2.4 cites `docs/research/btrfs-crun-benchmarks.md` §2 and copies no value; §8 item 2 states the report owns the four figures; §8.2 trap 3 cites the report; grep for `19.5`, `6.3`, `39 ms`, `11–24` finds no measurement (only the `### 6.3` heading) |
| k | No `Option A/B` / `Spike A/B` framing | **Fixed** | Scoped owner grep `! grep -riE 'option [ab]\|spike [ab]' docs/design/world-base-image.md` exits 0 with no output; the document states what Slipfork is and does |

## Acceptance Evidence

| # | Command | Exit | Evidence |
|---|---|---|---|
| 1 | `! grep -rniE 'ficlone\|incus\|zfs\|reflink' docs/design/world-base-image.md` | 0 | nothing printed |
| 2 | `grep -n 'BTRFS_IOC_SNAP_CREATE_V2' docs/design/world-base-image.md` | 0 | 3 matches: L10 summary, L115 nested-subvolume rule, L560 live acceptance |
| 3 | `grep -n 'user_subvol_rm_allowed,noatime' docs/design/world-base-image.md` | 0 | 2 matches: L75 mount command, L442 §6.12 fix block |
| 4 | `grep -n '0x9123683e' docs/design/world-base-image.md` | 0 | 3 matches: L80 `ensure_supported`, L439 §6.12 transcript, L559 live acceptance |
| 5 | `make docs-check` | 0 | `uvx --offline zensical build --strict` → "No issues found"; `check_docs_honesty: OK (9 published doc(s) match real CLI subcommands)` |
| 6 | `! grep -riE 'option [ab]\|spike [ab]' docs/design/world-base-image.md` (owner rule, node path) | 0 | nothing printed |
| 7 | `uv run --no-project tools/guard.py` | 0 | `guard: OK (483 file(s) checked)` |
| 8 | `uv run --no-project tools/plan.py validate` | 0 | `plan OK` |
| 9 | `! grep -niE 'testcoordinator\|runnerprotocol\|coveragedriver\|missingposixshell\|no_new_priv\|root:base-v' docs/design/world-base-image.md` | 0 | nothing printed |
| 10 | `! grep -rn '\[derived\]\|\[guessed\]' docs/design/world-base-image.md` | 0 | nothing printed |
| 11 | `! grep -n 'world_rootfs_crun' docs/design/world-base-image.md` | 0 | nothing printed (field removed this pass) |

`docs/design/world-base-image.md` is 597 lines.

## Critic lenses

The five `critic` lenses (`ioc-upward`, `legality`, `cruft`, `implementability`, `testability`)
were **not** re-run in this pass: the mandatory Quad Review Gate on the finished node supersedes
them, and attempt 1 already ran two five-lens rounds to PASS. This attempt is a disposition audit
plus one correction, not a new design.

## Concurrency note

While this attempt was auditing, `docs/design/world-base-image.md` was being rewritten by a
concurrent editor (content changed repeatedly between 21:43 and 21:45). This session stopped
editing, waited for the file to reach a stable hash, re-read the final content, and applied only
the `doctor`/`world_rootfs_crun` correction on top of it. All acceptance evidence above was
re-run against the post-quiescence file (stable SHA-256 `6155ffe9…` before this session's single
edit). `lead` should confirm no second M3.8 attempt is still live.

## Files

- `docs/design/world-base-image.md` (one correction: `doctor` emits no `crun` field)
- `plan/nodes/M3/M3.8/RESULT.md` (this file)

## Followups

1. **`ADR-0010` §2–§3 / `ADR-0009` §3 base-slot literals** (outside this node's paths): the ADRs
   spell the base and seed slots `<project>-base-<l0_sha12>` / `seed-<l0_sha12>`, which names only
   the rootfs input. §2.3.1's base key also covers service-image digests and seed-dump identity;
   aligning the ADR literals to `<base_key_sha12>` is a `lead` edit. (Moved out of the normative
   document in attempt 3 — this record is now its only home.)
2. **`ARCHITECTURE.md` §3.3 carriers** (outside this node's paths): §2.3.2 introduces
   `WorldSpec.state_subdirs: Vec<PathBuf>` and the inherent
   `BtrfsEngine::ensure_base(&self, seed: &BaseSeedSpec)`; §3.3 needs the `WorldSpec` field so
   `M3.13` implements a specified struct. `ensure_base` deliberately does **not** land on
   `trait WorldEngine` (attempt-3 IOC ruling), so §3.3's trait block needs no change for it.
3. **`ARCHITECTURE.md` §3.2 `SeedDumpStore` excerpt is stale** (outside this node's paths): it
   lists only `lookup`/`write_from`, while the trait also declares `discard` and `materialize`
   (`crates/slipfork-ospp/src/dump.rs`; `DiskSeedDumpStore` implements all four). §7.2 of this
   document depends on `materialize`, so the §3.2 excerpt needs those two signatures.
4. **`docs/research/btrfs-crun-benchmarks.md` §5.1 anchors** — **closed for this document in
   attempt 3**: §3.2, §8.2 and §8.2 trap 3 now cite §5 #1. Any other file still citing §5.1 is a
   `lead` sweep.
5. **`plan/nodes/M3/M3.2/M3.2.13/NODE.md` still says "Option A makes this concrete"** in its
   Context. `plan/` is outside the owner terminology gate's file set, but the phrase is the stale
   framing the rule deletes; `lead` should edit that node's Context.

## Attempt 3 disposition table

Scope: `docs/design/world-base-image.md` + this RESULT. No commit. Every row below is a change to
the document; nothing was answered by prose outside it.

### IOC gate (one MAJOR, four MINOR)

| # | Required disposition | Status | Evidence |
|---|---|---|---|
| IOC-1 (MAJOR) | `BaseSeedSpec.control_dir` had two owners: the daemon injected it while `ensure_base` steps 3/5 created and removed `<state_dir>/ctl/seed-<base_key_sha12>/` | **Fixed** | §2.2 tree: `ctl/seed-<base_key_sha12>/` = "created and removed by slipforkd, never by the engine"; §2.2 rule 3: the control dir is "daemon-injected … which `slipforkd` creates and the engine only consumes (never re-derived and never created inside the engine)"; §2.3.2 step 3 boots `slipfork-world-init` "inside the daemon-created `seed.control_dir`" and no longer creates a `ctl/` path; step 5: "`ensure_base` destroys the seed subvolume. The seed control directory is `slipforkd`'s: it created … and removes it once the seed world is gone." |
| IOC-2 (MINOR) | `ensure_base` sat on the domain port while no caller used it there | **Fixed** | §2.3.2 #1: `BtrfsEngine::ensure_base(&self, seed: &BaseSeedSpec)` is "a public **inherent** method on the concrete engine, never a `trait WorldEngine` method"; `trait WorldEngine` (`ARCHITECTURE.md` §3.3) gains nothing. The "unless a real daemon caller exists" branch is the one taken — forced by IOC-3, since only the daemon holds the seed payload; §2.5's snippet keeps the concrete `Arc<BtrfsEngine>` for `ensure_base` and coerces `Arc<dyn WorldEngine>` for world lifecycle, so the composition root stays the only assembler |
| IOC-3 (MINOR) | `BaseSeedSpec` rode on every `WorldSpec` although only base preparation consumed `service_specs`/`databases` | **Fixed** | §2.3.2: `BaseSeedSpec` is "one carrier **argument** to the base-preparation call" and "never rides on `WorldSpec`"; `WorldSpec` keeps only `state_subdirs` (single author = `slipforkd`). Updated carriers: §2.3 seed row, §2.3 clone row (`WorldSpec.state_subdirs`), §2.5 zero-ambient-reads bullet (`WorldSpec.state_subdirs`, `BaseSeedSpec`), §7.2 re-seed (`slipforkd` calls `BtrfsEngine::ensure_base`) |
| IOC-4 (MINOR) | `list_children` exclusion claimed to exclude "the `<control_dir>` tree", but control dirs live under `<state_dir>/ctl/` | **Fixed** | §2.5 "One enumeration-filter owner": exclusion policy is `worlds/seed-*` + `worlds/.seed.lock` only, plus "Control directories are never candidates: they live under `<state_dir>/ctl/<id>`, not under `worlds/` or `snapshots/`" |
| IOC-5 (MINOR) | No single named production constructor | **Fixed** | §2.5: "`BtrfsEngine::new` is the sole production constructor and builds `KernelBtrfsDriver` internally (`ADR-0010` §5); `BtrfsEngine::with_driver` is the explicit-injection constructor the composition root uses when it supplies the driver itself"; snippet constructs `BtrfsEngine::new(...)`; §2.1 #1 boot gate now attributes `ensure_supported` to `BtrfsEngine::new` |

### Cruft gate (`C1`–`C8`)

| # | Required disposition | Status | Evidence |
|---|---|---|---|
| C1 (MAJOR) | `M3.16` row said `[M3.10]`; frontmatter is `[M3.10, M3.15]` | **Fixed** | §9 `M3.16` row is `[M3.10, M3.15]`. **Every** row re-verified programmatically against `plan/nodes/M3/<id>/NODE.md` frontmatter: 19/19 `depends_on` sets match, 0 mismatches |
| C2 (MAJOR) | Two normative spellings of the restore sequence (§2.3 `pg_restore` + `CHECKPOINT;` vs §3.2 three-call form) | **Fixed** | §3.2 is the single normative statement ("The seed restore sequence — stated once, here, and cross-referenced everywhere else (§2.3, §7.2)"), including the `psql` existence check + `createdb`; §2.3 seed row now says "runs the one restore sequence of §3.2"; §2.3 snapshot row and §7.2 #3/#4 point at §3.2. `grep pg_restore` shows one normative spelling only |
| C3 (MINOR) | Dead anchor `btrfs-crun-benchmarks.md` §5.1 at three sites | **Fixed** | §3.2, §8.2 preamble and trap 3 now cite `§5 #1` (the report's `## 5. Traps` item 1). No `§5.1` left in the document |
| C4 (MINOR) | Stale `(done)` markers: only `M3.7` marked while `M3.9`/`M3.11` are also `done` | **Fixed** | `M3.9` and `M3.11` rows now carry *(done)*; convention = *(done)* iff frontmatter `status: done`, *(this node)* for `M3.8`, bare otherwise (verified: 19/19 rows) |
| C5 (MINOR) | `mkfs.btrfs`/`mount` block duplicated verbatim in §2.1 and §6.12 | **Fixed** | §2.1 owns the only `mkfs.btrfs` + `mount -o user_subvol_rm_allowed,noatime` snippet (1 occurrence of `mkfs.btrfs` in the file); §6.12 renders the single-line `mount_hint` and states "The canonical `mkfs.btrfs` + `mount` snippet is §2.1" |
| C6 (MINOR) | Phase-1 GC scope stated normatively twice (§7.2 #4 and §8.4 #5) | **Fixed** | §8.4 #5 now only asserts the teardown sequence and the scoped `btrfs subvolume list` result, ending "§7.2 #4 owns exactly what Phase-1 `prune` may and may not collect" |
| C7 (MINOR) | Three spellings of one slot placeholder + a stale `<l0_sha12>` literal with a pointer to RESULT | **Fixed** | `<base_key_sha12>` everywhere (§2.2, §2.3, §2.3.1, §7.2); `<first 12 hex of base_key_sha256>`, `<new_base_key_sha12>` and the `l0_sha12` ADR bullet are gone. The ADR-mismatch note now lives only in followup 1 above |
| C8 (MINOR) | `crun-…bin` ellipsis vs the concrete pinned name | **Fixed** | §2.1 names `<state_dir>/cache/runtime/crun-1.20-x86_64-ff8407185ec4.bin`, matching §7's cache tree; no `crun-…` left |

### Legality gate (`L1`–`L6`)

| # | Required disposition | Status | Evidence |
|---|---|---|---|
| L1 (MAJOR) | Teardown `EPERM` stage never named `EngineError::SubvolumeDestroyDenied { path, errno, mount_hint }` | **Fixed** | §2.1 #3: "fails closed inside `SubvolumeDriver::destroy_subvolume` with `EngineError::SubvolumeDestroyDenied { path, errno, mount_hint }` … and **not** `EngineError::BtrfsRequired`, which would misreport `actual_fstype` on a host whose filesystem is Btrfs (`ADR-0010` §4–§5)"; §6.12 prose repeats the same variant with "never `BtrfsRequired`" |
| L2 (MAJOR) | §8.4's VM tooling list required host `crun` | **Fixed** | §8.4 closing paragraph: "`docker`, `btrfs-progs`, and the SHA-256-pinned `crun` asset under `<state_dir>/cache/runtime/`, staged into the world rootfs — never a host `crun` on `PATH`" |
| L3 (MINOR) | "`WorldConfigError` diagnostics have no prefix and no path" over-generalized | **Fixed** | §6 preamble scopes prefix/pathlessness to the daemon world-selection variants (`NoBuildableImage` on `up`, `AmbiguousWorldImage`, `UnknownWorldImage`) and states the parse-time case: `load_slipfork_config` renders `<path>: {err}` (and `cannot read <path>: {err}`), so a parse-time diagnostic always names `slipfork.toml` |
| L4 (MINOR) | Same dead `§5.1` anchor | **Fixed** | See C3 |
| L5 (MINOR) | `/.slipfork/bin/crun` staging misattributed to `runtime_assets` | **Fixed** | §2.1: "`crates/slipfork::runtime::runtime_assets` provisions the pinned binary at `<state_dir>/cache/runtime/…bin`, and `DockerfileStageBuilder` stages and SHA-256-verifies it at the guest path `WorldRootfs.crun_bin = /.slipfork/bin/crun` (`ADR-0009` §1)" |
| L6 (MINOR) | `control_dir` clause in the `worlds/` enumeration exclusion | **Fixed** | See IOC-4 |

## Attempt 3 acceptance evidence

Re-run after the final edit; all commands exit `0`.

| # | Command | Evidence |
|---|---|---|
| 1 | `! grep -rniE 'ficlone\|incus\|zfs\|reflink' docs/design/world-base-image.md` | nothing printed |
| 2 | `grep -n 'BTRFS_IOC_SNAP_CREATE_V2' docs/design/world-base-image.md` | 3 matches (summary, nested-subvolume rule, live acceptance) |
| 3 | `grep -n 'user_subvol_rm_allowed,noatime' docs/design/world-base-image.md` | 2 matches (§2.1 canonical mount, §6.12 `mount_hint`) |
| 4 | `grep -n '0x9123683e' docs/design/world-base-image.md` | 3 matches (`ensure_supported`, §6.12 transcript, live acceptance) |
| 5 | `make docs-check` | `zensical build --strict` → "No issues found"; `check_docs_honesty: OK (9 published doc(s) …)` |
| 6 | `! grep -riE 'option [ab]\|spike [ab]' docs/design/world-base-image.md` | nothing printed |
| 7 | `uv run --no-project tools/guard.py` | `guard: OK (483 file(s) checked)` |
| 8 | `uv run --no-project tools/plan.py validate` | `plan OK` |
| 9 | `! grep -niE 'testcoordinator\|runnerprotocol\|coveragedriver\|missingposixshell\|no_new_priv\|root:base-v' …` | nothing printed |
| 10 | `! grep -n '\[derived\]\|\[guessed\]' …` and `! grep -n 'world_rootfs_crun' …` | nothing printed |
| 11 | Node-table re-verification (script over §9 rows vs `plan/nodes/M3/<id>/NODE.md`) | 19 rows, 0 mismatches (deps + `(done)` markers) |
| 12 | `grep -c 'mkfs.btrfs' docs/design/world-base-image.md` / `grep -c 'pg_restore …'` | 1 canonical snippet; 1 normative restore sequence |

`docs/design/world-base-image.md` is 626 lines.

## Attempt 3 cleanup — legality/cruft minors and IoC confirm

Same attempt (no bump). Five MINORs from the legality/cruft PASS, then one MAJOR + three minors from
the IoC confirm.

| # | Required disposition | Status | Evidence |
|---|---|---|---|
| X1 (MINOR) | `:483` alleged fabricated `SeedDumpStore::materialize` | **Verified real — citation added, ARCHITECTURE.md followup filed** | The method exists on the port: `crates/slipfork-ospp/src/dump.rs` (trait `lookup` / `write_from` / `discard` / `materialize`) and `crates/slipfork-ospp/src/disk_dump.rs:142` (impl, bounded by `decompress_bounded` at `:297`). §7.2 re-seed now cites both files, so no reader can mistake it for a fabricated name. The stale carrier is `ARCHITECTURE.md` §3.2, whose `SeedDumpStore` excerpt lists only `lookup`/`write_from` — followup 3 below |
| X2 (MINOR) | `:144` formula hashed `db.source_tag`, which the carrier does not declare | **Fixed** | Formula: `db.source.tag()`; the carrier now declares `impl BaseSeedSource { pub fn tag(&self) -> &str }` (`Clean => "clean"`, `Dump { sha256, .. } => sha256`), and the prose names `BaseSeedSource::tag()` as the one accessor `M3.13` implements |
| X3 (MINOR) | §8.2 titled "(Recorded Probe Transcripts)" while trap 3 disclaims being a transcript | **Fixed** | §8.2 retitled "Kernel/Runtime Traps (Transcripts for Traps 1–2; Observed Kernel Behaviour for Trap 3)" |
| X4 (MINOR) | `SubvolumeDestroyDenied` vs `BtrfsRequired` `EPERM` rationale stated twice | **Fixed** | §6.12 owns the rationale ("never `BtrfsRequired`, which reports a filesystem type this host does not have"); §2.1 #3 keeps the variant name and adds "catalogued once in §6.12" |
| X5 (MINOR) | `ipc`/`shmget` key-`90000` rationale stated twice | **Fixed** | §8.2 trap 3 owns it (with the report `§5 #1` citation); §3.2 now says only "`ipc` is mandatory, not optional — the System V shared-memory collision it prevents is recorded once in §8.2 trap 3". `grep shmget` → one hit |
| Y1 (MAJOR) | `BaseSeedSpec` was the sole `ensure_base` argument yet carried no rootfs path/digest/project/base key, so the engine would have had to derive them | **Fixed** | §2.3.2 carrier now declares `project: ProjectSlug`, `base_key_sha12: String`, `rootfs: RootfsDigest`, `app_rootfs: PathBuf` (plus `control_dir`, `state_subdirs`, `service_specs`, `databases`). The engine formats `worlds/seed-{base_key_sha12}` and `snapshots/{project}-base-{base_key_sha12}` from injected fields and fails closed if `app_rootfs` is not `<state_dir>/cache/rootfs/<rootfs.hex()>/app`. The "`WorldSpec.snapshot` names the base" claim is deleted from the §2.3 seed row; §2.3.1 states the daemon passes `base_key_sha12`+`project` into `BaseSeedSpec` **and** `WorldSpec.snapshot` into the following clone |
| Y2 (MINOR) | §2.3.2 prose named `base_digest`/`dump_mounts`/`service_specs`, which the struct does not declare | **Fixed** | Prose now names the struct's own fields (`app_rootfs`, `project`/`base_key_sha12`, `control_dir`, `state_subdirs`, `service_specs`, `databases`); `grep base_digest\|dump_mounts` → 0 hits |
| Y3 (MINOR) | §2.5 snippet used `ensure_base`/`clone_world` before `DockerfileStageBuilder` existed | **Fixed** | Snippet is labelled "in wiring order" and reads `base_images` → `materialize(&rootfs_request)` → `BaseSeedSpec { … rootfs.digest, rootfs.app_rootfs … }` → `ensure_base` → `clone_world`, so every `BaseSeedSpec` field has a visible producer |
| Y4 (MINOR) | `ARCHITECTURE.md` §3.3 must carry the final carrier shape | **Recorded as followup 2** | Followup 2 now names `WorldSpec.state_subdirs` **and** the `BaseSeedSpec` carrier (`project`, `base_key_sha12`, `rootfs`, `app_rootfs`, `control_dir`, `state_subdirs`, `service_specs`, `databases`, `BaseSeedSource::tag`) with no `trait WorldEngine` change |

Re-run after the cleanup (all exit `0`): inverted `ficlone|incus|zfs|reflink` grep empty;
`BTRFS_IOC_SNAP_CREATE_V2` ×3; `user_subvol_rm_allowed,noatime` ×2; `0x9123683e` ×3;
`make docs-check` → "No issues found" + honesty OK; terminology / stale-vocab / `[derived]|[guessed]` /
`world_rootfs_crun` / `§5.1` greps empty; `guard.py` OK (483 files); `plan.py validate` → `plan OK`.
Node table re-verified after this pass: 19 rows, 0 mismatches — `M3.10` flipped to `done` upstream
during the pass, so its row is now marked *(done)* too.
