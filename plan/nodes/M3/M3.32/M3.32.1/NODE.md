---
id: M3.32.1
title: "slipfork-forkfile: purpose-made Forkfile parser, whole-file caret checker, evaluator & JSON IR"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: []
paths: ["docs/decisions/ADR-0009*"]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-19T13:02"
updated: "2026-11-19T13:02"
---

# Goal

Create `crates/slipfork-forkfile` (`#![forbid(unsafe_code)]`) implementing **100%** of `docs/design/forkfile.md` as the single canonical manifest owner, with zero `snow.yml` YAML concepts leaking through.

# Context

- New crate member in `Cargo.toml` (`crates/slipfork-forkfile`), deps from `[workspace.dependencies]` only (`serde`, `serde_json`, `thiserror`, `getrandom`, `hex`, `base64`, `sha2`); no new third-party parser crate.
- Reference behaviour to port, not copy wholesale: `crates/slipfork-snow/src/model.rs:1-985` (`SnowFile`/modes/ingress), `crates/slipfork-snow/src/faceless.rs:1-432` (expression/generator grammar), `crates/slipfork-snow/src/provider.rs:1-456` (`SecretProvider`, `SecretValue`, `OsCsprngEntropy`), re-exports at `crates/slipfork-snow/src/lib.rs:11-18`.
- `FileVaultProvider` currently lives in `crates/slipfork/src/vaults.rs:16` (imports `slipfork_snow::{...}`) — move it into `slipfork-forkfile` as the canonical local `0600` vault provider and re-export it from the crate root.
- Spec sections to implement literally: `forkfile.md` §1 (Cargo-style `features` + `on <expr>` with `!` > `&` > `|`), §2 (postgres `version` default `"18"` + 8 props, redis `version` default `"7"` + 6 props, `s3`, `domain`, `app.*`, `deployment.*`), §3 (`./dir` vs `from <image>`, `stage`/`dockerfile`/`size`/`port`/`internal_ports`/`route`/`volume`, `env <group>` + `use env`, `build_env` vs `env` collision, `run`/`setup`/`pre-deploy`/`post-deploy`/`failed-deploy`/`task`, direct `argv` with `$port`/`$env.KEY`/`$<entity>.<prop>` and `FOO=bar` prefixes), §4 (multi-error `Span { offset, len, line, col }`, 1-based, rustc-style caret snippet), §5 (`parse_forkfile`, `validate(&dyn ExternalCheck)`, `evaluate(&FeatureSet)`, `find_*`, `EvaluatedFork::resolve_env`, `Spec.spec` JSON).
- Public API must match `forkfile.md` §5 exactly so `M3.32.3` can swap `slipforkd`/OSPP types mechanically.
- Do **not** delete `crates/slipfork-snow` in this slice (its consumers still compile); `slipfork-snow` is deleted in `M3.32.3` when `slipforkd` and `slipfork-ospp` cut over. `crates/slipfork-snow/**` is in this node's `paths` only so newly discovered `Forkfile` grammar/regex helpers can be lifted from the canonical owner.
- Tests: keep new unit tests inside `crates/slipfork-forkfile/src/**` (`#[cfg(test)]`) for pure grammar/span/taint algorithms, plus `crates/slipfork-forkfile/tests/critical_forkfile.rs` for the parse→validate→evaluate→JSON contract. Migrating `crates/slipfork-snow/tests/{critical_snow.rs,claimed_eval.rs}` is deferred to `M3.32.3` with the crate deletion; do not duplicate them here.

# Acceptance

- `cargo test -p slipfork-forkfile --all-targets` exits `0`.
- `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all -- --check` exit `0` (workspace still builds with `slipfork-snow` present).
- `crates/slipfork-forkfile/tests/critical_forkfile.rs` asserts the exact §4 snippet: parsing `DATABASE_URL = pg.url` yields `error: ...\n  --> Forkfile:14:20` plus a caret line and a ``did you mean database `app_db.url`?`` suggestion, and that **all** errors across active *and* inactive `on` branches are collected in one pass.
- A round-trip test proves `parse -> serde_json -> parse` is lossless, and `resolve_env` marks `secret(...)`-derived values `TaintedString::tainted` while `--reveal`-less rendering yields `[REDACTED]`.
- Write NO `RESULT.md`.
