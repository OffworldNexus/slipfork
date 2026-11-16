# Log M3.2.1

## 2026-11-16T16:59 [planner]

created (task) 0.3.0 test-orchestrator architecture blueprint: TestAdapter/CoverageDriver DI tree, RunnerProtocol v1, WorldPool, daemon seam

## 2026-11-16T18:11 [lead]

OWNER DIRECTIVE: HOLD this node until explicitly released; the owner routes extra language/platform (Python/PHP) modularity lessons from the external blastit investigation into this blueprint before it starts.

## 2026-11-16T18:15 [lead]

HOLD RELEASED (owner, 2026-11-16): blueprint may start once its DAG predecessors (M3.1/M3.19 -> M3.2) complete. Mandatory blastit post-mortem invariants appended to NODE.md: (1) zero language tokens in slipfork-test core (no Executor::Php-style enums, no match language, no tool exit-code/DB creation assumptions), (2) adapters wired only via runtime AdapterRegistry at crates/slipfork/src/main.rs; Cargo features = language/platform only, never test scaffolding, (3) TestAdapter/WorldSlotContext designed for PHP+Python+JS+Rust day one: structured results only over runner.sock or slot.output_dir report files (never parse child stdout/stderr), explicit cwd/argv/world env, language-neutral CoverageSummary merge, (4) zero vaporware: no phpunit/vitest/playwright docs until a real passing support tier exists. New M3.22 deletes the leftover slipfork-ospp test-server feature.

## 2026-11-16T18:15 [lead]

HOLD RELEASED (owner, 2026-11-16): blueprint may start once DAG predecessors (M3.1/M3.19 -> M3.2) complete. Mandatory blastit post-mortem invariants appended to NODE.md: (1) zero language tokens in slipfork-test core (no Executor::Php-style enums, no match language, no tool exit-code/DB-creation assumptions), (2) adapters wired only via runtime AdapterRegistry at crates/slipfork/src/main.rs; Cargo features = language/platform only, never test scaffolding, (3) TestAdapter/WorldSlotContext designed for PHP+Python+JS+Rust day one: structured results only over runner.sock or slot.output_dir report files (never parse child stdout/stderr), explicit cwd/argv/world env, language-neutral CoverageSummary merge, (4) zero vaporware until a real passing support tier exists. M3.22 deletes the leftover slipfork-ospp test-server feature.

## 2026-11-16T18:16 [lead]

OWNER HOLD: do not start. M3.2 is being rewritten around a thin slipfork<->blastit boundary; Slipfork will not ship a second test runner (no pytest-slipfork Zygote, no per-language coverage drivers). The blastit post-mortem invariants already recorded here remain binding input for the rewrite.
