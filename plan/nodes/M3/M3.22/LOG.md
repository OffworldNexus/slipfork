# Log M3.22

## 2026-11-16T18:15 [lead]

created (task) Delete leftover slipfork-ospp test-server Cargo feature; move reference OSPP server doubles into tests/support

## 2026-11-16T18:35 [lead]

SCOPE ADD (lead): also replace the one-shot 60s S3 readiness wait in crates/slipfork/tests/critical_ospp_daemon.rs with bounded polling (the transient flake observed during M3.1 attempt 3), plus delete the test-server feature. Same file, same determinism goal.

## 2026-11-16T18:43 [lead]

OWNER TERMINOLOGY RULE: no Option A/B or Spike A/B framing anywhere; describe the Btrfs+crun BtrfsEngine architecture directly. Gate before Phase 1 completes: ! grep -riE 'option [ab]|spike [ab]' ARCHITECTURE.md README.md docs/ crates/ tools/. Delete alternatives rather than rewording them. See appended section in NODE.md.

## 2026-11-16T19:16 [lead]

edited fields

## 2026-11-16T19:16 [lead]

depends_on widened to M3.1 + M3.15: M3.22 edits crates/slipfork/Cargo.toml and crates/slipfork/tests/support/mod.rs, which M3.15 also writes, so the path lock must serialize them (M3.15 owns the daemon/CLI rewrite; M3.22 lands after).

## 2026-11-17T00:44 [lead]

status ready -> active

## 2026-11-17T00:44 [lead]

attempt 1/3 started

## 2026-11-17T00:44 [lead]

base ca5b19175d139a0b042b6477268c89d231a10a2a

## 2026-11-17T00:44 [lead]

LEAD: started ahead of M3.15 (paths disjoint from the active M3.20 fix and from M3.15's src/**). M3.15 will consume the moved tests/support doubles afterwards. The s3_buckets 60s readiness wait is replaced by slipfork_testkit::poll_until (M3.1 landed it as a publish=false dev-dependency) — do not duplicate a third poll_until in tests/support.

## 2026-11-17T01:29 [lead]

status active -> review

## 2026-11-17T01:29 [lead]

worker: feature table + optional deps deleted, doubles in slipfork/tests/support shared by #[path] with slipfork-ospp tests, owned suite 2/4 green (2 reds = legacy WORLD_STOP_GRACE at load 53.74), ospp suite 32 passed, clippy/check/fmt/docs-check/guard/plan/pytest/rs_test_list green.

## 2026-11-17T01:33 [lead]

edited fields

## 2026-11-17T01:33 [lead]

LEAD RULING after quad gate (ioc+cruft+legality MAJOR converge): the #path cross-package include is rejected. Create publish=false workspace crate crates/slipfork-ospp-testkit owning the four doubles (memory_store, reference_server, signers, tpmd_server) with their real deps; both crates/slipfork (only critical_ospp_daemon's imports) and crates/slipfork-ospp tests consume it via [dev-dependencies]; delete crates/slipfork-ospp/tests/support/** and the 6 dev-deps from crates/slipfork/Cargo.toml. Paths widened accordingly. Acceptance gate restated: cargo tree -p slipfork --edges normal must have no NEW direct edge to tempfile/tpmd-proto/tpmd-ipc/tungstenite/zstd/p256 — tpmd-proto legitimately remains via the production tpmd-client signer chain.

## 2026-11-17T02:44 [lead]

edited fields

## 2026-11-17T02:44 [lead]

Paths widened by lead: ARCHITECTURE.md (its M2 residue sentence names the wrong destination and must be corrected in this node).

## 2026-11-17T02:47 [lead]

LEAD record corrections from legality gate: base had the test-server selector in [dev-dependencies] (not production [dependencies]); invariants 1/4 rewritten to the shipped testkit design and the 'no new direct edge' gate. Assertion baseline: critical_ospp_daemon 4 tests/36 asserts, critical_ospp 32/170, unchanged.

## 2026-11-17T03:02 [lead]

Gate policy: ioc PASS, legality PASS (both with record MINORs only), cruft fixes in flight. If the next cruft/functional round has no BLOCKER/MAJOR, accept and commit; MINORs go to the log.

## 2026-11-17T03:20 [implementer]

CORRECTION to the 02:47 record (the entry itself stands, not rewritten): `critical_ospp` is 32 tests / **172** asserts, not "32/170" — base `ca5b191` and the working tree are identical there. `critical_ospp_daemon` is 4 tests / 36 asserts, unchanged. Owned-suite outcome: 1 passed / 3 failed at load 37–48, all three reds the legacy 10 s `WORLD_STOP_GRACE` / 60 s readiness windows; nothing retried.

## 2026-11-17T03:18 [lead]

FINAL: functional PASS, ioc PASS, legality PASS, cruft PASS (record fixes verified). Committing.

## 2026-11-17T03:18 [lead]

status review -> done
