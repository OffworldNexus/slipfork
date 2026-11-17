# Log M3.12

## 2026-11-16T16:58 [planner]

created (task) SubvolumeDriver seam: KernelBtrfsDriver (statfs + BTRFS_IOC_*) and test-only tree driver

## 2026-11-16T17:25 [strategist]

edited fields

## 2026-11-16T18:11 [lead]

OWNER DIRECTIVE: zero test-scaffolding Cargo features (no feature="test-support"). SubvolumeDriver seam uses pure constructor DI; test fakes only via #[cfg(test)] + tests/common/ or unpublished publish=false testkit crate in [dev-dependencies]. Features are reserved for language/platform modularity.

## 2026-11-16T18:43 [lead]

OWNER TERMINOLOGY RULE: no Option A/B or Spike A/B framing anywhere; describe the Btrfs+crun BtrfsEngine architecture directly. Gate before Phase 1 completes: ! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/. Delete alternatives rather than rewording them. See appended section in NODE.md.

## 2026-11-16T21:45 [lead]

M3.25 RULING: implement EngineError::SubvolumeDestroyDenied { path, errno, mount_hint } for the destroy-stage EPERM path alongside BtrfsRequired and BtrfsProbeReport (ADR-0010 §4).

## 2026-11-16T22:50 [lead]

M3.25 final ruling to consume: WorldId newtype (Display/FromStr/parse_selector, TestSlot never on wire), DropForkRequest { cwd, world: Option<WorldId> }, PruneReport declared once, WorldSpec.world, BaseKey fields promoted, DoctorReport.fixes order btrfs->unshare_userns->netns, EngineError world:String via WorldId::to_string().

## 2026-11-16T23:20 [lead]

ADD (M3.25 ioc minor): type EngineError::SnapshotNotFound(String) -> SnapshotId so snapshot identity is not stringly (ARCHITECTURE §3.3).

## 2026-11-16T23:52 [lead]

edited fields

## 2026-11-16T23:52 [lead]

LEAD RULING on the test-driver seam (verified by experiment): the shared TestTreeDriver lives in the publish=false crates/slipfork-testkit crate (absolute dev-dependency of slipfork-engine and slipfork; engine->testkit dev edge + testkit->engine normal edge is a legal Cargo cycle,  proven green). KernelBtrfsDriver is the only production implementation in src/btrfs.rs. Engine lib unit tests, engine integration tests (M3.14) and slipfork integration tests (M3.15) all consume the one testkit tree driver — no duplicated fake driver. Paths widened to crates/slipfork-testkit/** for this reason.

## 2026-11-17T03:47 [lead]

status ready -> active

## 2026-11-17T03:47 [lead]

attempt 1/3 started

## 2026-11-17T03:47 [lead]

base 6827c5117e4c8b12a10ab546f997105cc5e527c5

## 2026-11-17T04:11 [lead]

status active -> review

## 2026-11-17T04:11 [lead]

worker done: btrfs.rs + TestTreeDriver in testkit, 11 lib tests, SAFETY 10>=9, clippy/check/fmt/workspace green. Quad gate starting. Notes: dev-dep cycle makes engine lib tests unable to consume the testkit driver; integration suites can.

## 2026-11-17T04:20 [lead]

edited fields

## 2026-11-17T04:20 [lead]

LEAD RULING (ioc/cruft MAJORs): (1) one require_btrfs gate; (2) ProbeFacts becomes a required trait method and probe a provided trait method (single probe body, no public probe_report helper) with a one-line ARCHITECTURE §3.3 amendment — paths widened; (3) TestTreeDriver derives identity from a sibling .<name>.slipfork-subvol marker (survives driver re-creation) and documents that RO seals the subvolume root; (4) remove the main.rs vaporware doc.

## 2026-11-17T04:43 [lead]

FINAL: legality PASS, functional PASS, cruft PASS, ioc PASS after lead plan amendments (M3.14 invariant 1 now consumes the testkit TestTreeDriver; M3.13 invariant 3 moves driver-injected suites to tests/ per the ARCHITECTURE §1 double-compile rule). Committing.

## 2026-11-17T04:43 [lead]

status review -> done
