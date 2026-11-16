# Log M3.7

## 2026-11-16T16:58 [planner]

created (task) Option A design of record: ADR-0010, ARCHITECTURE.md DI tree, and ADR-0001..0009 scorched-earth wipe

## 2026-11-16T17:28 [lead]

status ready -> active

## 2026-11-16T17:28 [lead]

attempt 1/3 started

## 2026-11-16T17:28 [lead]

base 6736e08c62c5e58f89792711d8b20a6485c3c05c

## 2026-11-16T17:54 [lead]

status active -> review

## 2026-11-16T17:57 [lead]

status review -> active

## 2026-11-16T17:57 [lead]

attempt 2/3 started

## 2026-11-16T18:11 [lead]

ADDITIONAL FINDING M from sibling M3.1 quad gate: SPEC_BLIND_SPOT ServiceSpec::readiness_port (TCP accept != usable service); architect must encode one readiness owner (engine ReadyProbe::Exec vs canonical caller probe seam) in ARCHITECTURE.md §3.3 + ADR-0010/0002, consumed by M3.1/M3.13/M3.14/M3.2.1.

## 2026-11-16T18:11 [lead]

OWNER DIRECTIVE: zero test-scaffolding Cargo features (no feature="test-support"). SubvolumeDriver seam uses pure constructor DI; test fakes only via #[cfg(test)] + tests/common/ or unpublished publish=false testkit crate in [dev-dependencies]. Features are reserved for language/platform modularity.

## 2026-11-16T18:32 [lead]

OWNER RULING while attempt 2 runs: Phase 2 becomes STP v1 (inherited SLIPFORK_PROTOCOL_FD AF_UNIX socketpair; framework-exposed DAG stage nodes incl. persistent fixture/migration stages frozen into RO Btrfs snapshots; coverage+mutants in-protocol; language-blind Rust orchestrator; ad-hoc unpublished pytest/vitest/phpunit proofs; NO CommandTestAdapter/guessing, NO published plugin packages, NO per-language Rust coverage drivers). ADR-0006 in your paths must not keep a normative RunnerProtocol v1/NDJSON-runner.sock/per-language-Adapter contract that 0.3.0 will not ship: do a bounded truthfulness pass using ONLY these owner-supplied facts, mark framework adapters as ad-hoc unpublished proofs, and point to the Phase 2 rewrite for the full STP v1 spec. Do not invent details.

## 2026-11-16T18:38 [lead]

status active -> review

## 2026-11-16T18:39 [lead]

QUAD attempt 2 findings (legality FAIL + ioc FAIL, no blind spot): (L1 BLOCKER) ADR-0006:32 and (L2 BLOCKER) ARCHITECTURE.md:578 claim crates/slipfork-test 'ships/proven in 0.3.0' -> must be labeled Phase-2 target state (owning M3.2); (L3 MAJOR) ARCHITECTURE.md:15 composition-root paragraph mixes shipped 0.2.0 with M3 STP orchestrator/BtrfsArtifactStore present tense; (L4/L5 MINOR) mermaid label + VISION perfect tense; (L6 MINOR) ADR-0010:9 provenance cites plan/nodes/M3/LOG.md -> cite M3.11/NODE.md owner-supply. (I1 MAJOR) ADR-0008:53 second readiness mechanism contradicts sole ServiceSpec.readiness owner -> rewrite; (I2 MAJOR) ADR-0002:22 says ReadinessProbe derives from ADR-0009 §2 image contract that carries no readiness data -> put readiness argv on OciRootfs in ADR-0009 §2; (I3 MINOR) <runtime_dir> defaulting owner ambiguous vs ADR-0005/0008 explicit --runtime-dir; (I4 MINOR) btrfs::probe bypasses driver seam -> probe on trait; (I5 MINOR) pytest/vitest/phpunit tokens in core crate row; (I6 MINOR) WorldEngine::name vestigial.

## 2026-11-16T18:39 [lead]

status review -> active

## 2026-11-16T18:39 [lead]

attempt 3/3 started

## 2026-11-16T18:58 [lead]

edited fields

## 2026-11-16T18:58 [lead]

status active -> review

## 2026-11-16T18:59 [lead]

legality final PASS with 2 MINORs to fix in the follow-up: (1) ADR-0010:52 unlabeled VM-harness '/usr/bin/unshare in VM profile' claim -> label M3.16 target state (requirement also logged on M3.10/M3.16); (2) ADR-0002:12 names crates/slipfork-test in present tense -> append (M3.2 target state).

## 2026-11-16T18:59 [lead]

edited fields

## 2026-11-16T18:59 [lead]

attempt 4/4 started

## 2026-11-16T19:00 [lead]

cruft final pass findings forwarded to the attempt-4 pass: X1 ADR-0003 dangling zygote clause + title, X2 VISION:16 zygote misattribution of subvolume CoW, X3 ARCHITECTURE:580 duplicate of ADR-0006 normative STP promise, X4 drift layout string, X5 ADR-0009 duplicate Rust port structs, X6 ADR-0008 unpointed --ipc banner, X7 metrics restated 4x. Stale deleted-file entries pruned from M3.7 paths by lead (deletions were authorized by the attempt-1 path set).

## 2026-11-16T19:00 [lead]

edited fields

## 2026-11-16T19:08 [lead]

ioc final pass FAIL with 2 MAJORs forwarded to the architect follow-up: E1 ADR-0010 must state the deletion ruling (all previously shipped engine adapters/protocol/storage pools/FICLONE-class helpers deleted by M3.20/M3.21, no fallback engine/runtime selection); E2 ARCHITECTURE.md:11 feature rule must name M3.22 as the owner deleting the existing slipfork-ospp test-server residue; E3 name the vehicle that reaches TestTreeDriver from crates/slipfork tests; E4 mermaid CLI->KernelBtrfsDriver construction edge. Readiness/Layout/STP checks otherwise clean.

## 2026-11-16T20:48 [lead]

edited fields
