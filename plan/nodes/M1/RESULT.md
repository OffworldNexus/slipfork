# Result M1 (`0.1.0` Final Milestone Gate)

STATUS: done (`RELEASE_READY: 0.1.0`)

## Independent Verification Summary

1. **Full Automated Gates (`make check` + honesty + banned-token sweep):**
   - `make check`: PASS (`plan.py validate`, `guard.py` on 150+ files, `ruff`, `pytest tools/tests` 6 passed, `bun test` 3 passed, `cargo fmt/clippy/doc/test` across all 5 crates with 37 Rust tests green, `zensical build --strict`, `check_docs_honesty.py` 8/8 published pages clean).
   - Banned-surface sweep (`rg -n "after_suite|SLIPFORK_ARTIFACTS_DIR|localtest\.me|BlastitObsCollector|--blastit-|weaved.*register_tunnel" docs crates README.md ARCHITECTURE.md CHANGELOG.md plan/ROADMAP.md`): `exit 1` (0 hits).
   - Live uncontended `make bench` re-run: PASS (`1.89 s` warm pool / `118 ms` per test / `3.24x` speedup vs `6.11 s` sequential baseline, `48.3 ms` 1000-file PG 18.7 `FICLONE` fork, `383 us` 64 KiB reflink, `10.41 ms` warm `dag plan`; restored committed `docs/research/m1-poc-benchmark.md`).

2. **Owner Release-Gate Corrections (`M1.8` + `M1.9`) & Accuracy Polish:**
   - **Single canonical origin (`ADR-0005` §1–§2, `H-008`):** `WorldOrigin = Local { url } | Shared { url }` normative, materialization-time assignment, `slipforkd` sole local + shared ingress (`shared_hosts` map + `*.localhost`-scoped `308`/`421` rules), and OSPP non-prerequisite for M2/M3.
   - **Origin-classified `per_world` fixtures (`ADR-0003` §1, `crates/slipfork-dag`):** `FixtureSpec.per_world` hashed with `"per-world"` vs `"shared"` discriminator, `OriginIndependentChildOfPerWorld` enforced, `DagPlanner::plan` short-circuits `DigestCache` to `CacheAction::Rebuild`, and `ADR-0003`/`ARCHITECTURE.md` updated to match `0.1.0` code + write-side cache non-insertion rule.
   - **Built-in typed coverage (`ADR-0006` §5, `crates/slipfork-dag`):** Byte-consistent `TestSpec { coverage: Option<CoverageConfig> }`, `CoverageReportFormat`, `CoverageThreshold` (`0..=100`, `MAX = 100`), `CoverageOptionsRequireEnabled`, and `EmptyCoverageReport`; zero shell hook fields or shims.
   - **Support registry (`ADR-0007`, `support/integrations.toml`):** All 4 entries (`reflink-netns-world-engine`, `file-vault-secrets`, `incus-engine`, `dag-config-cli`) verified offline against real files, `critical_*` symbols, and commit `d0b11e069ced52d0e03ebb3b251a1077d0c1bf39`. Fixed `ARCHITECTURE.md` (`main.rs` wires `slipfork support` in M2) and `ADR-0007` (`0.1.0` embedded status), and filed `H-009` (`direction-challenge` on `M2`) to settle pre-`declared` conformance-tested adapter tier vocabulary before `slipfork support` renders tiers in M2.
