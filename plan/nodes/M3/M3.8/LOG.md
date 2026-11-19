# Log M3.8

## 2026-11-16T16:58 [planner]

created (task) Rewrite docs/design/world-base-image.md on unified Btrfs subvolumes

## 2026-11-16T18:11 [lead]

OWNER DIRECTIVE: zero test-scaffolding Cargo features (no feature="test-support"). SubvolumeDriver seam uses pure constructor DI; test fakes only via #[cfg(test)] + tests/common/ or unpublished publish=false testkit crate in [dev-dependencies]. Features are reserved for language/platform modularity.

## 2026-11-16T18:43 [lead]

OWNER TERMINOLOGY RULE: no Option A/B or Spike A/B framing anywhere; describe the Btrfs+crun BtrfsEngine architecture directly. Gate before Phase 1 completes: ! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/. Delete alternatives rather than rewording them. See appended section in NODE.md.

## 2026-11-16T19:19 [lead]

STALE VOCABULARY (M3.7 cruft sign-off): docs/design/world-base-image.md:364 still carries TestCoordinator/RunnerProtocol v1 wording; the rewrite must drop the dead protocol names and describe only the Btrfs/crun substrate plus the STP v1 pointer (Phase 2 M3.2).

## 2026-11-16T20:48 [lead]

status ready -> active

## 2026-11-16T20:48 [lead]

attempt 1/3 started

## 2026-11-16T20:48 [lead]

base 4d5b02efd7444c3413a6d238271c2a700ed9fd5a

## 2026-11-16T20:59 [lead]

status active -> review

## 2026-11-16T21:01 [lead]

edited fields

## 2026-11-16T21:01 [lead]

QUAD GATE attempt 1 (ioc FAIL, SPEC_BLIND_SPOT escalated to M3.25): BLOCKER builder credited with seed+seal (must stop at cache/rootfs/<digest>/app; seed+seal solely BtrfsEngine §2.3); MAJOR prune has no owning GC port/entry point (engine GC method or dedicated injected port + named reference owner); MAJOR user_subvol_rm_allowed admission gap (see M3.25 ruling); MINOR single owner for mount order + list_children enumeration filtering; MINOR inline target-state labels for OciRootfs.readiness/spawn_service.

## 2026-11-16T21:03 [lead]

ADD (M3.11 cruft gate): docs/design/world-base-image.md becomes the CITATION of docs/research/btrfs-crun-benchmarks.md for the four numbers, not a second copy of them; the report owns the measurements.

## 2026-11-16T21:35 [lead]

status review -> active

## 2026-11-16T21:35 [lead]

attempt 2/3 started

## 2026-11-16T21:35 [lead]

base 7cf08b5202c58593b3f6c4031bebf200a8e24f97 (attempt 2)

## 2026-11-16T21:45 [lead]

status active -> review

## 2026-11-16T21:48 [lead]

attempt 3/3 started

## 2026-11-16T21:48 [lead]

status review -> active

## 2026-11-16T21:48 [lead]

IOC GATE attempt 2 FAIL: MAJOR BaseSeedSpec.control_dir has two owners — doc says injected by slipforkd (:162) yet ensure_base creates/removes <state_dir>/ctl/seed-<base_key>/ (:186, steps 3,5), contradicting "never re-derived inside the engine" (:117/:209). Ruling: daemon injects control_dir, engine never derives/creates it. Minors: make ensure_base a private inherent BtrfsEngine method (clone_world is the only caller) or declare it with its caller; keep seed-only payload out of WorldSpec by passing &BaseSeedSpec to ensure_base and leaving state_subdirs; drop the control-dir clause from the list_children exclusion (control dirs live under ctl/, ADR-0010 §3 excludes only seed-*/.seed.lock); name BtrfsEngine::new as the sole production constructor.

## 2026-11-16T21:51 [lead]

status active -> review

## 2026-11-16T21:54 [lead]

status review -> done
