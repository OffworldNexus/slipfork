# Log M3.14

## 2026-11-16T16:58 [planner]

created (task) Engine conformance + critical suites on BtrfsEngine (driver-injected fast suites, live kernel suite for the VM)

## 2026-11-16T17:25 [strategist]

edited fields

## 2026-11-16T18:11 [lead]

OWNER DIRECTIVE: zero test-scaffolding Cargo features (no feature="test-support"). SubvolumeDriver seam uses pure constructor DI; test fakes only via #[cfg(test)] + tests/common/ or unpublished publish=false testkit crate in [dev-dependencies]. Features are reserved for language/platform modularity.

## 2026-11-16T18:43 [lead]

OWNER TERMINOLOGY RULE: no Option A/B or Spike A/B framing anywhere; describe the Btrfs+crun BtrfsEngine architecture directly. Gate before Phase 1 completes: ! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/. Delete alternatives rather than rewording them. See appended section in NODE.md.

## 2026-11-16T20:45 [lead]

REQUIREMENT (M3.1 legality gate): engine conformance/critical suites must be deterministic at high host load; assert bounded teardown elapsed time rather than fixed-grace waits, and do not reintroduce fixed sleep windows.

## 2026-11-16T21:21 [lead]

REQUIREMENT (M3.1 ioc gate): when engine-owned ReadinessProbe lands, delete the temporary caller-side readiness shim — crates/slipfork-engine/tests/critical_oci_runtime.rs `wait_postgres_ready` + `PG_READY_TIMEOUT` must be removed in favor of asserting ReadinessProbe::Exec semantics; add to acceptance.

## 2026-11-17T04:20 [lead]

LEAD AMENDMENT (M3.12 ioc finding): do NOT create crates/slipfork-engine/tests/support/test_tree_driver.rs. The single SubvolumeDriver double is crates/slipfork-testkit::btrfs::TestTreeDriver (publish=false dev-dependency); integration suites pass &TestTreeDriver as &dyn SubvolumeDriver. Delete the stale invariant-1 line in your run.

## 2026-11-17T05:32 [lead]

status ready -> active

## 2026-11-17T05:32 [lead]

attempt 1/3 started

## 2026-11-17T05:32 [lead]

base 3bc93e3ef6be13ce9d5c2e8e2326565960483686

## 2026-11-17T09:15 [lead]

status active -> review

## 2026-11-17T11:17 [lead]

FINAL fixes applied (3): post-stop container fail-closed assertions restored with a real client log line; base64 crate replaces the silent hand-rolled decoder; current_uid_gid replaces id -u/-g. 96 engine tests, rs-test 335 passed, support-check/docs-check green.

## 2026-11-17T11:37 [lead]

edited fields

## 2026-11-17T11:37 [lead]

status review -> done
