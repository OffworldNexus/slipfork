# Log M3.2

## 2026-11-16T06:34 [strategist]

created (epic) Polyglot Parallel Snapshot-DAG Test Orchestrator (slipfork test, RunnerProtocol v1, warm world pool & CoverageDriver)

## 2026-11-16T16:59 [planner]

edited fields

## 2026-11-16T18:16 [lead]

OWNER HOLD (2026-11-16): M3.2 (M3.2.1..M3.2.18) is ON HOLD pending a conductor rewrite of this epic. Owner clarified Slipfork must NOT integrate deeply with languages nor ship a second test runner (no pytest-slipfork Zygote, no per-language coverage drivers) because external blastit owns test execution. The exact thin slipfork<->blastit boundary is being locked with the owner. No M3.2 child may be activated until the epic is rewritten; Phase 1 (M3.1, M3.7..M3.21, M3.19) continues at full speed.

## 2026-11-16T18:32 [lead]

OWNER RULING (2026-11-16): rewrite pending conductor/strategist/planner. Phase 2 = STP v1 (inherited SLIPFORK_PROTOCOL_FD AF_UNIX socketpair); framework-exposed DAG stage nodes incl. persistent fixture/migration stages frozen into RO Btrfs snapshots; coverage+mutants first-class in-protocol; language-blind crates/slipfork-test orchestrator; ad-hoc unpublished pytest/vitest/phpunit proofs; DELETE out-of-scope CommandTestAdapter/pytest-slipfork/per-language CoverageDriver from slipfork-dag, ADR-0006 and every M3.2 child. M3.2 stays ON HOLD and no child activates until Phase 1 is done and conductor has rewritten the epic.

## 2026-11-16T19:07 [lead]

PHASE 2 REWRITE MUST-DO (flagged by M3.7 legality review): this epic's NODE.md title/body still say 'RunnerProtocol v1', 'CoverageDriver', 'CommandTestAdapter', 'pytest-slipfork' and per-language adapters. The rewrite must replace them with STP v1 (inherited SLIPFORK_PROTOCOL_FD socketpair, framework-exposed DAG stage nodes incl. persistent fixture stages frozen to RO Btrfs snapshots, protocol-native coverage & mutants, language-blind crates/slipfork-test orchestrator, ad-hoc unpublished pytest/vitest/phpunit proofs) and delete every out-of-scope mention.

## 2026-11-16T19:07 [lead]

PHASE 2 REWRITE MUST-DO (flagged by M3.7 legality review): this epic NODE.md title/body still say RunnerProtocol v1, CoverageDriver, CommandTestAdapter, pytest-slipfork and per-language adapters. The rewrite must replace them with STP v1 (inherited SLIPFORK_PROTOCOL_FD socketpair, framework-exposed DAG stage nodes incl. persistent fixture stages frozen to RO Btrfs snapshots, protocol-native coverage and mutants, language-blind crates/slipfork-test orchestrator, ad-hoc unpublished pytest/vitest/phpunit proofs) and delete every out-of-scope mention.

## 2026-11-16T21:45 [lead]

REQUIREMENT (M3.9 legality): own the `slipfork test -j N` concurrency flag in the rewritten STP v1 epic — define its semantics (slot count) and document it; M3.9 target-dx references it as the Phase-2 concurrency knob.

## 2026-11-16T21:52 [lead]

OWNER WIRE-PROTOCOL DIRECTIVE: STP v1 frames are CBOR (RFC 8949) on the inherited SLIPFORK_PROTOCOL_FD socketpair — never NDJSON/JSON, never line-delimited framing. The Phase 2 replan must encode this in ADR-0006 §3 and the runner-protocol types, and delete every NDJSON mention.

## 2026-11-16T23:50 [lead]

OWNER REGROUP RULING (2026-11-16, binding): M3.2 (STP v1) and every M3.2.* child are PARKED for 0.2.0. Do not activate any of them in this run. The 0.1.0 release ships Btrfs Worlds (up, fork, list, shell, service, logs, forward, drop, prune, doctor, OSPP v1, .localhost edge) only; the owner tests 0.1.0 hands-on on the DO Btrfs VM first.

## 2026-11-17T14:26 [lead]

PARKED (owner ruling 2026-11-16, binding): STP v1 is 0.2.0. Do NOT activate M3.2 or any M3.2.* child in the 0.1.0 release; its ROADMAP row still says 0.3.0 and must be rewritten by the strategist before 0.2.0 starts.

## 2026-11-17T14:39 [lead]

status ready -> blocked

## 2026-11-17T14:39 [lead]

STATUS blocked = PARKED, not a missing input: owner ruling 2026-11-16 parks STP v1 to 0.2.0. Reactivating M3.2.* requires the strategist to rewrite the 0.3.0/Option-A ROADMAP row and a fresh milestone plan.
