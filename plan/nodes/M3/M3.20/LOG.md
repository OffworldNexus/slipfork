# Log M3.20

## 2026-11-16T17:25 [strategist]

created (task) Workspace trait & test decoupling: drop SnapshotMode/StatefulRam and replace FakeIncus in slipfork-edge

## 2026-11-16T18:11 [lead]

OWNER DIRECTIVE: zero test-scaffolding Cargo features (no feature="test-support"). SubvolumeDriver seam uses pure constructor DI; test fakes only via #[cfg(test)] + tests/common/ or unpublished publish=false testkit crate in [dev-dependencies]. Features are reserved for language/platform modularity.

## 2026-11-16T18:43 [lead]

OWNER TERMINOLOGY RULE: no Option A/B or Spike A/B framing anywhere; describe the Btrfs+crun BtrfsEngine architecture directly. Gate before Phase 1 completes: ! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/. Delete alternatives rather than rewording them. See appended section in NODE.md.

## 2026-11-16T21:44 [lead]

REQUIREMENT (M3.9 cruft gate): crates/slipfork-edge/src/daemon.rs:82 documents ResourceSummary.id as pg_databases.main while diagnostics() emits bare ids; align the doc comment with the shipped bare-id contract.

## 2026-11-16T22:50 [lead]

M3.25 final ruling to consume: WorldId newtype (Display/FromStr/parse_selector, TestSlot never on wire), DropForkRequest { cwd, world: Option<WorldId> }, PruneReport declared once, WorldSpec.world, BaseKey fields promoted, DoctorReport.fixes order btrfs->unshare_userns->netns, EngineError world:String via WorldId::to_string().

## 2026-11-16T23:50 [lead]

status ready -> active

## 2026-11-16T23:51 [lead]

attempt 1/3 started

## 2026-11-16T23:51 [lead]

base ca5b19175d139a0b042b6477268c89d231a10a2a

## 2026-11-17T00:28 [lead]

worker report: SnapshotMode/StatefulRam gone, StubWorldEngine replaces FakeIncus, all 31 edge tests green, 20/20 bun at its run, workspace check/clippy/docs-check/guard/plan-validate/pytest green. Reds under load ~52: legacy fixed 10s WORLD_STOP_GRACE (11 critical_engine) and legacy readiness budgets (2 critical_oci_runtime); bun e2e daemon.sock 15s. Quad gate starting.

## 2026-11-17T00:28 [lead]

status active -> review

## 2026-11-17T03:36 [lead]

ARCHITECT RULING (escalation #21): provided Self:Sized mint_world_handle is the single sanctioned seam; WorldHandle is an in-process (engine,seq) lookup token with no cross-engine/IPC authority; ARCHITECTURE §3.3 amended (WorldHandle comment + trait listing). Code doc must use the architect's exact contract wording. Stub now passes assert_world_engine_conformance (33 edge tests).

## 2026-11-17T03:42 [lead]

edited fields

## 2026-11-17T03:42 [lead]

Paths widened: ARCHITECTURE.md (architect's escalation-#21 amendment lands with this node). Residuals: (a) pre-existing critical_engine r10_tmpfs assumes /tmp is tmpfs; this host's /tmp is XFS, deterministic red owned by M3.21's deletion; (b) bun e2e legacy daemon.sock 15s under load.

## 2026-11-17T03:46 [lead]

status review -> done
