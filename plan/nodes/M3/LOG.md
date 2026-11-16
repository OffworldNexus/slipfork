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
