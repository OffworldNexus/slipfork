# Log M3.25

## 2026-11-16T21:00 [lead]

created (task) Close doctor host-probe DI gap and world-identity spelling split in the design of record

## 2026-11-16T21:00 [lead]

status ready -> active

## 2026-11-16T21:00 [lead]

attempt 1/3 started

## 2026-11-16T21:00 [lead]

base 4d5b02efd7444c3413a6d238271c2a700ed9fd5a

## 2026-11-16T21:01 [lead]

edited fields

## 2026-11-16T21:01 [lead]

SCOPE ADD (M3.8 legality BLOCKER): rule on ADR-0009 §3 rule 3 (unreadable non-world-accessible files) — today the doc promises a pre-boot diagnostic naming the path, but the only Rule 3 error variant (RootAtRuntimeEntrypoint) carries stage/entrypoint/uid/gid/component and cannot name a path, and the design doc contradicts itself (pre-boot check vs boot EROFS/EACCES). Decide: (a) add BaseImageError::UnreadablePath { path } to ADR-0009 §3 + assign the owning implementation node (M3.13/M3.14), or (b) delete the clause from the normative ADR and state the boot-time EROFS behaviour as the only guarantee. Update ADR-0009 accordingly and note the consumer.

## 2026-11-16T21:02 [lead]

SCOPE ADD #2 (M3.8/M3.9/M3.19 consistency): rule whether crun is a HOST binary (harness apt installs it; M3.19 runs crun --version on the host) or a WORLD-ROOTFS binary (ADR-0009 WorldRootfs.crun_bin = /.slipfork/bin/crun). The answer determines doctor's crun probe, the harness provisioning list, and M3.19 acceptance. State it once in the DI tree + image contract and remove the contradiction.

## 2026-11-16T21:34 [lead]

status active -> review

## 2026-11-16T21:45 [lead]

status review -> review

## 2026-11-16T21:45 [lead]

edited fields

## 2026-11-16T21:45 [lead]

attempt 2/4 started

## 2026-11-16T21:45 [lead]

CRUFT GATE attempt 2 FAIL: (1) base slot spelling drift — ADR-0010:22-23 + ADR-0009:23 say <project>-base-<l0_sha12> while the design of record keys <base_key_sha12> (rootfs + service images + seed dump); adopt ONE literal; (2) WorldEngine::prune_snapshots cited 3x but absent from the trait fence; (3) ADR-0008:53-54 still says uniform --world <fork>, contradicting ADR-0005 §3; (4) prune_cache contract stated 3x — one normative home; minors: crun provisioning paragraph restated 3x, BtrfsProbeReport field list re-listed, cache brace-set inconsistent, Phase-1 prune leaving snapshots empty must be user-visible.

## 2026-11-16T21:51 [lead]

ADD (M3.8 attempt 3 followup): ARCHITECTURE.md §3.3 WorldSpec must carry state_subdirs (single author slipforkd); ensure_base stays an inherent BtrfsEngine method, no trait change; ADR-0010/0009 base-slot literals -> <project>-base-<base_key_sha12> / seed-<base_key_sha12>.

## 2026-11-16T21:52 [lead]

ADD (M3.8 ioc): ARCHITECTURE.md §3.3 must carry WorldSpec.state_subdirs AND the BaseSeedSpec carrier (control_dir, state_subdirs, service_specs, databases, app_rootfs, rootfs/RootfsDigest, base_snapshot/SnapshotId) injected by slipforkd — the engine must never derive or guess those paths. ensure_base stays inherent on BtrfsEngine.

## 2026-11-16T21:53 [lead]

ADD (M3.8 final): ARCHITECTURE.md §3.2 SeedDumpStore excerpt must list discard + materialize (port has lookup/write_from/discard/materialize; DiskSeedDumpStore impl uses bounded decompress_bounded). Also carry WorldSpec.state_subdirs + the final BaseSeedSpec shape (project, base_key_sha12, rootfs, app_rootfs, control_dir, state_subdirs, service_specs, databases).

## 2026-11-16T22:26 [lead]

edited fields

## 2026-11-16T22:26 [lead]

attempt 3 done: base_key spelling, prune ports + CacheKeepSet, WorldId newtype, complete EngineError, DoctorReport flattening, CBOR §3. Re-running quad gate; ADR-0006 added to paths.

## 2026-11-16T22:27 [lead]

attempt 3/4 started

## 2026-11-16T22:27 [lead]

status review -> active

## 2026-11-16T22:27 [lead]

IOC FINAL FAIL: MAJOR one --world contract has two spellings — ADR-0005:52 gives `drop` a private positional-<fork> parse path while ADR-0008:53 says every world-scoped command selects exclusively via --world <world>; and DaemonControl::drop_fork(&DropForkRequest) has no selector carrier. Fix: DropForkRequest { cwd, world: Option<WorldId> } routed through the single resolve_world_selector/WorldId::parse_selector; delete ADR-0005:52 private parse. Plus WorldId serde must stay a JSON string (manual/serde(from=String,into=String)), parse_selector = trim -> FromStr -> project check, EngineError world fields typed WorldId, PruneReport declared once.

## 2026-11-16T22:49 [architect]

attempt 4 final pass: DropForkRequest { cwd, world: Option<WorldId> } + single resolve_world_selector/WorldId::parse_selector (delegating :/canonical to FromStr), manual JSON-string WorldId serde rejecting TestSlot on wire, PruneReport declared once, EngineError world fields via WorldId::to_string(), BaseKey fields promoted, SeedDumpStore write_from/materialize prose aligned, fixes order btrfs->userns->netns pinned, cruft deduplications folded; cross-node followup recorded for lead/M3.23 (M3.9 target-dx.md §1.6: <project>:<project> accepted as UserFork(<project>), <fork>==project rejected only on slipfork fork).

## 2026-11-16T22:50 [lead]

status active -> review

## 2026-11-16T22:52 [lead]

edited fields

## 2026-11-16T22:52 [lead]

attempt 4/5 started

## 2026-11-16T22:52 [lead]

status review -> active

## 2026-11-16T22:52 [lead]

CRUFT FINAL FAIL: MAJOR DropForkRequest { cwd, world: Option<WorldId> } is field-identical to WorldSelector (which already lists DropForkRequest as a consumer) — delete DropForkRequest and use WorldSelector (or alias). Minors: CBOR framing restated 3x in ARCHITECTURE (pointer only), fixes order restated beside ADR-0010 §4, state_subdirs declared on both BaseSeedSpec and WorldSpec, stale canonical world_id(project,target) comment + HookFailed field inventory, ADR-0008 restates write_from/MAX_DUMP_WINDOW_LOG, ADR-0008 rescue commands still say --world <fork>, ADR-0009 heading restates no-host-crun rule.

## 2026-11-16T23:19 [lead]

status active -> review

## 2026-11-16T23:20 [lead]

POST-STOP review results (recorded for regroup; no work started): ioc PASS, cruft PASS, legality PASS — all with MINORs only. Outstanding one-line fixes before M3.25 can be called clean: (1) ARCHITECTURE §3.3 WorldRef derives Debug and formats Handle(h) as {h:?} but WorldHandle derives only Clone/PartialEq/Eq — add Debug (or hand-write WorldRef Debug) or the enum cannot compile; (2) BaseSeedSpec carries project + base_key_sha12: String instead of a BaseKey, so ensure_base must re-spell <project>-base-<sha12> rather than calling BaseKey::snapshot_id() — carry BaseKey (or BaseSeedSpec::snapshot_id()) to prevent seal-vs-prune keep-set drift; (3) cruft prose dedupe: retention trio repeated in §3.3 vs ADR-0010 §3, ADR-0008:52 names a phantom DropFork request (method is drop_fork(WorldSelector)), ADR-0008:53 re-enumerates selector spellings, ARCHITECTURE:602-611 restates WorldId grammar. M3.1 remains uncommitted with its record mid-rewrite.

## 2026-11-16T23:21 [lead]

POST-STOP: functional sign-off PASS — all four lenses now PASS (ioc/legality/cruft/functional) with MINORs only. Diff is docs-only over exactly the 7 declared paths; zensical --strict, docs honesty, guard, plan validate green; CBOR pinned once (16 MiB cap); DI diagram single doctor::run composer; every symbol declared once. NOT COMMITTED (owner hard stop). When committing: stage ONLY ARCHITECTURE.md + ADR-0002/0005/0006/0008/0009/0010, never M3.1 files. Extra minor for later: ARCHITECTURE:612 references undeclared WorldTarget — add one enum line or pointer.

## 2026-11-16T23:48 [lead]

LEAD POST-STOP: quad gate 4/4 PASS (ioc/legality/cruft/functional) at 23:19-23:21 with MINORs only, docs-only diff over the 7 declared paths. Committing per owner regroup ruling (0.1.0 release). Carried MINORs -> M3.23 docs hygiene: (1) ARCHITECTURE §3.3 WorldRef formats Handle(h) as {h:?} but WorldHandle derives no Debug — add Debug; (2) BaseSeedSpec carries project+base_key_sha12 strings instead of BaseKey, so ensure_base must re-spell <project>-base-<sha12> (seal-vs-prune drift risk) — carry BaseKey; (3) cruft prose dedupe: retention trio repeated §3.3 vs ADR-0010 §3, ADR-0008:52 names phantom DropFork request (method is drop_fork(WorldSelector)), ADR-0008:53 re-enumerates selector spellings, ARCHITECTURE:602-611 restates WorldId grammar, ARCHITECTURE:612 references undeclared WorldTarget.

## 2026-11-16T23:48 [lead]

status review -> done

## 2026-11-16T23:48 [lead]

done: plan validate + guard.py + make docs-check (zensical --strict, check_docs_honesty OK) green; docs-only over exactly the 7 declared paths.
