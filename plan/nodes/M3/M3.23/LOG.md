# Log M3.23

## 2026-11-16T18:43 [lead]

created (task) Terminology gate: purge Option A/B and Spike A/B framing from ARCHITECTURE.md, README.md, docs/, crates/, tools/

## 2026-11-16T18:59 [lead]

edited fields

## 2026-11-16T18:59 [lead]

SCOPE ADD (from M3.7 ioc final pass): docs/design/m1-core-and-poc.md still carries stale VolumeCloner/IncusEngine/TestAdapter rows after the Btrfs-only pivot. This gate also deletes/labels stale post-pivot design residue under docs/design/** (keeping the M3.8/M3.9/M3.10 rewrites), with zero banned engine names or dead engine seams left.

## 2026-11-16T19:07 [lead]

ADDITIONAL SCOPE (M3.7 legality final pass): (1) ADR-0010:9 and ADR-0006:13-14 must phrase docs/research/btrfs-crun-benchmarks.md as 'to be recorded by M3.11' (future tense), not 'as measured in the benchmark report' while the file is absent; (2) ARCHITECTURE.md:156  port-doc names crates/slipfork-test without the Phase 2 / M3.2 target-state qualifier. Verify both after M3.11 lands, and keep the repo-wide option/spike grep clean.

## 2026-11-16T19:07 [lead]

ADDITIONAL SCOPE (M3.7 legality final pass): (1) ADR-0010:9 and ADR-0006:13-14 must phrase docs/research/btrfs-crun-benchmarks.md as "to be recorded by M3.11" (future tense), not "as measured in the benchmark report" while the file is absent; (2) ARCHITECTURE.md:156 port-doc comment names crates/slipfork-test without the Phase 2 / M3.2 target-state qualifier. Verify both after M3.11 lands, and keep the repo-wide option/spike grep clean.

## 2026-11-16T19:19 [lead]

FINAL POLISH (from M3.7 cruft sign-off, optional): (1) ADR-0009 Composition Root mermaid redraws the ARCHITECTURE §1 tree -> collapse to a §1 pointer; (2) ADR-0002:19 restates the BtrfsEngine::new -> with_driver constructor-DI sentence also in ADR-0010 §5 -> keep one home; (3) ARCHITECTURE.md:11 test-server token should vanish once M3.22 lands.

## 2026-11-16T20:48 [lead]

FINAL POLISH from M3.7 functional review: ADR-0010 Consequences should state the operational consequence (every non-Btrfs host hard-fails at slipforkd boot and slipfork doctor with EngineError::BtrfsRequired {path, actual_fstype, mount_hint} naming mount -o user_subvol_rm_allowed,noatime; M3.21 deletes all previously shipped adapters/protocol/pools).
