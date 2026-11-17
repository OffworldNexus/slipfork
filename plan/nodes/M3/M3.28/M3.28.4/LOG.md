# Log M3.28.4

## 2026-11-17T16:32 [planner]

created (task) Restart vs up semantics, the port-less origin renderer, and the engine process-tree accessor

## 2026-11-17T19:39 [lead]

status ready -> active

## 2026-11-17T19:39 [lead]

attempt 1/3 started

## 2026-11-17T19:39 [lead]

base b49faefb2a2652c4d06b3785df88d611b32acbd8

## 2026-11-17T19:46 [lead]

attempt 1 hit the step cap (unverified WIP, workspace not compiling). SPLIT: .4 is now engine facts only (world_processes/world_lineage, BootLineage, injected clock, origin port-0 fail-closed, delete WorldLineage::unknown()); the restart/up semantics moved to new M3.28.17 (depends .4) and M3.28.6 now depends on .3+.4+.17. SCOPE: reverted the attempt-1 edits to crates/slipfork-snow/src/model.rs and the protected crates/slipfork/tests/critical_cli.rs (Phase C single-writer); the in-world loopback URL is not a .localhost origin, so the acceptance grep is scoped to WorldOrigin::local call sites. Added oci_store.rs + world/tests to paths (both structurally required).

## 2026-11-17T19:46 [lead]

attempt 2/3 started

## 2026-11-17T19:46 [lead]

base b49faefb2a2652c4d06b3785df88d611b32acbd8

## 2026-11-17T19:52 [lead]

edited fields

## 2026-11-17T19:52 [lead]

attempt 3/3 started

## 2026-11-17T19:59 [lead]

status active -> review

## 2026-11-17T20:01 [lead]

quad round 1 (cruft): FAIL — 2 BLOCKER second-clock paths (BtrfsEngine::with_driver injecting SystemClock; OciImageStore::new pinning SystemClock then overridden by .with_clock), MAJOR row_kind field that is a pure function of RuntimeKind, MAJOR one timestamp (spanning dump+salt+seed+destroy) reported as both base.millis and seed.millis (fabricated durations), MAJOR elapsed_millis duplicated 3x, plus minors (hand-written Debug, LineageCache vs ServiceCache, Default on unit SystemClock, a test of std::time).

## 2026-11-17T20:01 [lead]

edited fields

## 2026-11-17T20:01 [lead]

quad round 1 (ioc): FAIL — ServiceKind must be a caller fact on ServiceSpec (role), not derived from spawn mechanism (a resource spawning as WorldProcess would report Component); PostgresReady::production() self-constructs Instant::now (second time source); oci_store::new pins SystemClock then overridden; with_driver still self-wires SystemClock; base/seed share one measured window; anonymous (SnapshotId, BootLineage) pair should be a named SealedBase.

## 2026-11-17T20:01 [lead]

quad round 1 (legality): FAIL — BLOCKER: freeze_world returning SnapshotExists (base already on disk) is swallowed into Ok(()) yet the code still reports base.millis: Some(..) and seed.rebuilt: Some(true), i.e. a rebuild duration + 'rebuilt' for a pure cache hit. Must return reused facts (millis None, rebuilt Some(false)) on SnapshotExists. MINORS: poisoned service mutex rendered as pid None on a Running row (fail closed like the rest of the engine); seed.millis reusing the base-seal number; the shared WorldEngine conformance suite never exercises world_processes/world_lineage (add stopped-row/None-not-Some(0)/WorldGone assertions).

## 2026-11-17T20:02 [lead]

edited fields

## 2026-11-17T20:02 [lead]

attempt 4/4 started

## 2026-11-17T20:21 [lead]

attempt 4 (cap-out after the work): lead-verified acceptance on the tree — engine lib 66, critical_world 16, critical_edge 37, critical_engine 15 (101s), slipfork lib 35, no-placeholder-lineage, one-origin-renderer, workspace clippy clean, fmt-ok, critical_guard --node M3.28.4 OK (the 5 protected edits are authorized and additive), guard OK 552, plan OK. Count drops are deletions of tests for deleted things (fake-clock unit test, loopback_http_url case) per the gate rulings. RESULT.md is stale w.r.t. counts; this LOG is the evidence of record.

## 2026-11-17T20:22 [lead]

edited fields

## 2026-11-17T20:23 [lead]

edited fields

## 2026-11-17T21:00 [lead]

status review -> done

## 2026-11-17T21:00 [lead]

pre-commit: make check rc=0 (44 suites) once host load dropped from 19 to 6.5; the two earlier red runs were the same 'world init survived SIGTERM+SIGKILL for 15.0s' timeout on *different* tests, with a foreign cargo test in D state and load 16-19 — environmental, and the identical signature predates this node. Recorded as a robustness gap for M3.27 (teardown should not call a D-state pid 'survived', and 15s is too tight on a loaded host).
