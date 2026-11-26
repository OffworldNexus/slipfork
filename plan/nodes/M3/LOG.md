# Log M3

## 2026-11-14T22:03 [strategist]

created (milestone) Polyglot Parallel Snapshot-DAG Test Orchestrator (0.3.0)

## 2026-11-16T16:53 [conductor]

OWNER RULING (SCORCHED EARTH — OPTION A BTRFS-ONLY): (1) Option A (unified Btrfs subvolumes + Linux namespaces `userns+netns+ipcns+pidns+mntns` + `pivot_root` + `crun`) is the SOLE `WorldEngine` (`BtrfsEngine`). (2) Wipe EVERYTHING that isn't Option A from code, docs, ADRs, CLI, tools, and tests — delete `IncusEngine`, delete XFS `FICLONE` `ReflinkNetnsEngine`, delete ZFS/Incus from `do_vm.py` (retarget `do_vm.py` to Btrfs `-o user_subvol_rm_allowed,noatime` + `crun` + AppArmor `userns` + persistent SSH identity), zero backward compat, zero fallbacks. (3) Then deliver M3 (`0.3.0`) `slipfork test` polyglot orchestrator (`RunnerProtocol v1`, rolling warm Btrfs world pool, typed coverage, `pytest-slipfork` + `CommandTestAdapter`).

## 2026-11-16T17:25 [strategist]

edited fields

## 2026-11-16T18:11 [lead]

OWNER ARCHITECTURAL DIRECTIVE (2026-11-16): (1) ZERO test-scaffolding Cargo features anywhere; features belong strictly to language/platform modularity (python/php/js). SubvolumeDriver seam = pure constructor DI (BtrfsEngine::new + BtrfsEngine::with_driver(Arc<dyn SubvolumeDriver>)); test fakes live in #[cfg(test)] + tests/common/ or an unpublished publish=false testkit crate via [dev-dependencies]. Binding for M3.7, M3.12, M3.14, M3.15, M3.20. (2) HOLD M3.2.1 (@architect slipfork-test blueprint) until the owner says go; Phase 1 continues at full speed while researcher inspects external Python/PHP pain points for M3.2.1 input.

## 2026-11-16T18:15 [lead]

OWNER (2026-11-16): blastit post-mortem invariants are mandatory for M3.2.1/M3.2 (zero language tokens in core; AdapterRegistry only at the slipforkd Composition Root; language-only Cargo features; runner.sock/report-file-only structured results; PHP/Python/JS/Rust-ready WorldSlotContext; zero adapter vaporware). HOLD on M3.2.1 lifted; it still starts only after its DAG predecessors. Created M3.22 to delete the leftover slipfork-ospp test-server feature (anti-pattern residue).

## 2026-11-16T18:15 [lead]

OWNER (2026-11-16): blastit post-mortem invariants are mandatory for M3.2.1/M3.2 (zero language tokens in core; AdapterRegistry only at the slipforkd Composition Root; language-only Cargo features; runner.sock/report-file-only structured results; PHP/Python/JS/Rust-ready WorldSlotContext; zero adapter vaporware). HOLD on M3.2.1 lifted; it still starts only after its DAG predecessors. Created M3.22 to delete the leftover slipfork-ospp test-server feature.

## 2026-11-16T18:16 [lead]

OWNER (2026-11-16): M3.2 epic ON HOLD pending conductor rewrite (thin slipfork<->blastit boundary; blastit is the test runner; no deep per-language integration, no pytest-slipfork Zygote, no per-language coverage drivers). Drive Phase 1 (M3.1, M3.7..M3.22, M3.19) to gate M3-phase-1 completion at full speed.

## 2026-11-16T18:32 [lead]

OWNER RULING (2026-11-16) — PHASE 2 = Slipfork Testing Protocol STP v1: standardized protocol over an inherited shared fd (SLIPFORK_PROTOCOL_FD, AF_UNIX socketpair). Frameworks expose nodes of the global DAG, including persistent setup/fixture stage nodes (migrations, seed factories, S3 state) that survive the setup process because Slipfork freezes each materialized stage into a read-only Btrfs subvolume snapshot, composing with build/artifact DAG nodes. Coverage and mutation ('mutants') are first-class protocol concepts, NOT per-language Rust drivers in Slipfork. No transparent magic/guessing (CommandTestAdapter), no premature published plugin packages: delete out-of-scope content from slipfork-dag/ADR-0006/M3.2; implement the language-blind Rust STP v1 DAG orchestrator (crates/slipfork-test) and prove STP v1 with ad-hoc pytest/vitest/phpunit plugins. PROCESS: drive Phase 1 (M3.1, M3.7..M3.22, M3.19) to done, then STOP and return to conductor BEFORE activating any M3.2 child (strategist+planner replace M3.2.1..M3.2.18).

## 2026-11-16T19:16 [lead]

PHASE 2 VOCABULARY (flagged by M3.7 ioc sign-off): this milestone NODE.md still describes TestCoordinator/RunnerProtocol v1/CoverageDriver/pytest-slipfork in its Phase 2 paragraph. The conductor/strategist M3.2 rewrite must update the M3 node text too, not only M3.2 children.

## 2026-11-16T21:47 [lead]

edited fields

## 2026-11-16T21:52 [lead]

OWNER WIRE-PROTOCOL DIRECTIVE (2026-11-16): STP v1 wire serialization is STRICTLY CBOR (RFC 8949) over the inherited SLIPFORK_PROTOCOL_FD socketpair — NEVER NDJSON/JSON. Binding for all Phase 2 design and docs; zero Phase 1 docs may say NDJSON for STP v1.

## 2026-11-16T23:50 [lead]

edited fields

## 2026-11-16T23:50 [lead]

OWNER REGROUP RULING (2026-11-16, binding): this subtree ships as 0.1.0 on release/0.1.0 (nothing merged to develop yet). M3.25 and M3.1 are committed done. M3.2 (STP v1) and all M3.2.* children are PARKED for 0.2.0 — do not activate. Chain: M3.20 -> M3.12 -> M3.13 -> M3.14/M3.15 -> M3.21 -> M3.16/M3.17/M3.18 -> M3.19 -> M3.22/M3.23/M3.24; hard stop before M3.2, return live 0.1.0 DO VM demo details to conductor. Version 0.1.0 in Cargo.toml/package.json/CHANGELOG/zensical.toml/docs.

## 2026-11-17T00:42 [lead]

BLOCKER (H-005): the tagged DO volume 131a2765 is ZFS (incuspool), not Btrfs; M3.16 is blocked pending owner authorization to wipe it (A) or create a fresh Btrfs volume (B). Droplet currently absent, volume unattached. M3.12+ code work continues independently; M3.19 live verification waits on H-005.

## 2026-11-17T04:44 [lead]

LEAD SCOPE RULING (0.1.0 shipping decision, 2026-11-17): M3.13 implements BtrfsEngine against the CURRENT trait surface (WorldSpec unchanged: target/rootfs/snapshot/mounts/base_env/local_ingress; engine derives <state_dir>/ctl/<slug>). The M3.25 target-state refinements — slipfork_world::WorldId + resolve_world_selector, WorldSpec.world/control_dir/state_subdirs injection, BaseKey::derive + BaseSeedSpec + WorldEngine::ensure_base sealing — are DEFERRED to 0.2.0 (M3.2/follow-up M3.26), not silently dropped: M3.23 must relabel ARCHITECTURE/ADR target-state references as 0.2.0 so 0.1.0 docs stay honest. Rationale: the daemon's existing ensure_base_snapshot flow already seals bases via create_snapshot, so Btrfs worlds work; shipping the owner-testable 0.1.0 takes priority over the internal trait refactor.

## 2026-11-17T14:01 [lead]

MILESTONE GATE (lead-side): make check EXIT=0 (zero env overrides; every suite green incl. slipfork:critical_cli 14 @62s, critical_ospp_daemon 4 @111s, both critical_btrfs_live 1) and make docs-check EXIT=0 (zensical --strict 'No issues found'; check_docs_honesty OK 9 pages) on the committed 0.1.0 tree 2db6c8f. Live VM evidence pending M3.19 attempt 2. NOTE for conductor/strategist: plan/ROADMAP.md's M3 row is still 0.3.0/Option-A framed with the STP v1 checklist; it must be rewritten to 0.1.0 (Btrfs worlds shipped, STP v1 parked to 0.2.0) before this milestone can be marked done.

## 2026-11-17T15:05 [lead]

status ready -> active

## 2026-11-17T15:05 [lead]

ARG: eligible lead exception: M3.27 (test-only world-subvolume leak) remains ready by conductor decision; it does not change the shipped product and is recorded as a follow-up.

## 2026-11-17T15:05 [lead]

GATE PASSED (0.1.0, Btrfs Worlds): plain make check exit 0 (5m32s, 43 suites, 335+ tests, ts 16/16, both critical_btrfs_live 1/1, zensical --strict, docs-honesty OK) and make docs-check exit 0 on this non-Btrfs XFS host; LIVE DO VM 165.232.86.174 (droplet 620073485, Btrfs 9123683e, sysctl=1) ran engine critical_btrfs_live + slipfork critical_btrfs_live + critical_cli 14 + critical_slug 7 + critical_ospp_daemon 4 GREEN TWICE (warm 301s) after the M3.19.1 AppArmor attachment fix; sealed base RO + two sibling clones on :5432 in separate netns; 0.1.0 binaries installed for root+slipfork; demo up/curl/fork/logs/db/drop/down verified incl. a fork row; reboot-safe. Residuals accepted: (a) benchmark numbers not produced on the dev VM (no PG18 client/Chrome/uv installed; M3.17 harness fails closed and no numbers were faked); (b) M3.27 test-suite world-subvolume leak (test-only); (c) M3.2/STP v1 parked to 0.2.0; (d) WorldId/control_dir/ensure_base deferred to 0.2.0 and ARCHITECTURE relabeled; (e) sh getcwd cosmetic line in slipfork exec.

## 2026-11-17T17:40 [lead]

Surface shrink (M3.28.15): 0.1.0 drops the unimplemented tunnel-sharing surface entirely — 'slipfork share', DaemonControl::share/unshare, UnavailableTunnelRegistrar + slipfork-edge/src/tunnel.rs, TunnelRegistrar/TunnelLease/TunnelError/TunnelRequest + their doubles, WorldOrigin::Shared (and the 308 shared-host redirect / 421 Upgrade paths / shared_hosts), and IntegrationCategory::Tunnel. Theme: 0.1.0 ships only what can succeed; 'tunnel sharing is unavailable' is not a feature. Re-entry point: M4.1 (blocked on H-006).

## 2026-11-19T06:51 [strategist]

edited fields

## 2026-11-19T07:00 [strategist]

Milestone gate pass 1: ROADMAP.md + M3 updated; make check & docs-check & guard.py & plan.py validate all exit 0; Triad caught 0.2.0 DAG vaporware in cli.rs --help, stale doctor transcripts in getting-started.md/cli.md, and downstream snow delimiter scan in resolve.rs -> queued surgical M3.31 before final done sign-off

## 2026-11-19T07:15 [strategist]

Milestone gate PASS (HEAD e06375f): M3.31 verified, all Triad & automated gates green -> RELEASE_READY: 0.1.0

## 2026-11-19T07:15 [strategist]

status active -> done

## 2026-11-19T12:42 [conductor]

status done -> active: Reopening 0.1.0 for Snowfile DSL + per-service crun pod rootfs + clean-sheet CLI UX redesign

## 2026-11-21T00:43 [conductor]

edited fields

## 2026-11-22T09:42 [lead]

status active -> done

## 2026-11-22T10:16 [conductor]

status done -> active

## 2026-11-22T11:15 [conductor]

status active -> done

## 2026-11-22T12:10 [conductor]

status done -> active

## 2026-11-22T19:51 [conductor]

status active -> done

## 2026-11-22T20:59 [conductor]

status done -> active

## 2026-11-22T23:36 [lead]

status active -> done

## 2026-11-22T23:36 [lead]

GATE PASSED: live /home/slipfork/demo runs real uv+FastAPI + pnpm+SvelteKit with fork isolation, run-id logs, parallel teardown and cross-domain ps

## 2026-11-22T23:44 [conductor]

status done -> active

## 2026-11-22T23:49 [conductor]

status active -> done

## 2026-11-22T23:54 [conductor]

status done -> active

## 2026-11-23T02:34 [conductor]

status active -> done

## 2026-11-23T02:38 [conductor]

status done -> active

## 2026-11-23T02:41 [conductor]

status active -> done

## 2026-11-23T13:07 [conductor]

status done -> active

## 2026-11-23T15:42 [conductor]

status active -> done

## 2026-11-23T15:50 [conductor]

status done -> active

## 2026-11-23T15:54 [conductor]

status active -> done

## 2026-11-23T19:02 [conductor]

status done -> active

## 2026-11-23T19:28 [conductor]

status active -> done

## 2026-11-23T20:16 [conductor]

status done -> active

## 2026-11-23T20:47 [conductor]

status active -> done

## 2026-11-23T21:13 [conductor]

status done -> active

## 2026-11-23T21:29 [conductor]

status active -> done

## 2026-11-23T22:36 [conductor]

status done -> active

## 2026-11-23T23:13 [conductor]

status active -> done

## 2026-11-23T23:39 [conductor]

status done -> active

## 2026-11-23T23:51 [conductor]

status active -> active

## 2026-11-24T00:00 [conductor]

status active -> done

## 2026-11-24T00:04 [conductor]

status done -> active

## 2026-11-24T03:14 [conductor]

status active -> done

## 2026-11-24T11:34 [conductor]

status done -> active

## 2026-11-25T19:21 [strategist]

edited fields

## 2026-11-25T19:22 [strategist]

status active -> active

## 2026-11-25T19:22 [strategist]

M3 gate FAIL at 4a20e55: GHA CI 45670641396 red (critical_world_journey cached-image inner rows + Windows CRLF format test), 2 live suites missing from LIVE_SUITES, Law 1 >500 LOC/Palette::tint/pty AtomicBool gaps, and specs/02-04 + CHANGELOG drift; filed M3.180-M3.183, superseded M4.1, refreshed ROADMAP (M4=0.2.0, M5=0.3.0); docs/ untouched

## 2026-11-25T23:38 [strategist]

M3 gate pass 2 FAIL at a7c9cb6: all 16 live suites pass individually on cold Btrfs (53/53), but unmodified make live-test exits 2 before suite 11/16 (purge_store cannot delete BTRFS_SUBVOL_RDONLY subvolumes); 2 prod files >500 non-test LOC (progress.rs 619, builder/mod.rs 524); version/spec/ARCHITECTURE/.SRCINFO drift + doctor copy defect remain

## 2026-11-26T03:28 [strategist]

M3 milestone gate third pass @ 3aef2ca PASS: make check green, single cold make live-test 16/16 (54 tests, 0 EROFS, warm refusal rc=2, purge-store 77 subvols -> absent), Five Laws verified (max src 500 LOC, tests/** exemption documented in specs/03), guard.py + plan.py validate OK

## 2026-11-26T03:28 [strategist]

status active -> done

## 2026-11-26T07:19 [conductor]

status done -> active

## 2026-11-26T07:19 [conductor]

Post-gate docs pass requested by the owner: tutorial-first docs rewrite (docs/**, zensical.toml, README). M3 reopened only to host this child; gate evidence unchanged.

## 2026-11-26T07:26 [conductor]

status active -> done

## 2026-11-26T07:30 [conductor]

status done -> active

## 2026-11-26T07:30 [conductor]

status active -> done

## 2026-11-26T07:35 [conductor]

status done -> active

## 2026-11-26T07:38 [conductor]

status active -> done

## 2026-11-26T07:39 [conductor]

status done -> active

## 2026-11-26T07:39 [conductor]

status active -> done

## 2026-11-26T07:40 [conductor]

status done -> active
