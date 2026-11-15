# Result M1.6 (Gate 8 Defect Fix — Expert)

## Summary

### Gate 8 Defect Fix (`review-cruft` MAJOR)

- **Single `.localhost` host/URL renderer via `LocalHostRoute` (`crates/slipfork-snow/src/model.rs`):**
  - Removed the leftover single-caller `ResourceCoordinates::local_base` helper (which pre-rendered `format!("{project}.localhost")` and `format!("http://{project}.localhost")` only for `local_for_world` to overwrite both fields).
  - `ResourceCoordinates::local_for_world` now validates `route` via `LocalHostRoute::new(...)` and constructs `ResourceCoordinates` once with `app_domain: route.host()` and `app_url: route.url()`, leaving `LocalHostRoute` as the sole `.localhost` host/URL renderer and keeping the public `ResourceCoordinates` API unchanged.

### Prior Gate 6 Code/Test Repairs Preserved

1. **Deduplicated `.localhost` route-parse matrix (`crates/slipfork-snow/tests/critical_snow.rs`, `crates/slipfork-world/tests/critical_world.rs`):**
   - `critical_snow.rs` covers snow-owned wiring (`ResourceCoordinates::local_for_world` for `Canonical`/`UserFork`/`TestSlot`, `SnowError::World` propagation, `domains[].id` validation); `critical_world.rs` holds the single `.localhost` rule-table matrix.
2. **Shared DNS-label validator (`crates/slipfork-world/src/lib.rs`, `crates/slipfork-snow/src/model.rs`):**
   - Exported `DNS_LABEL_REASON` and `validate_unreserved_dns_label` in `slipfork-world` and reused across `slipfork-world` and `SnowFile::validate_structure`.
3. **Removed duplicate in-module slug test (`crates/slipfork-world/src/lib.rs`).**
4. **Unified CLI spawn helper (`crates/slipfork/tests/critical_slug.rs`):** `run_slipfork_with_env`.
5. **Enforced `--file` + `--repo` root consistency in `dag plan` (`crates/slipfork/src/main.rs`, `crates/slipfork/tests/critical_slug.rs`).**
6. **Honest `slipfork fork --help` copy (`crates/slipfork/src/main.rs`).**

## Verification (real commands & exit codes)

- `make check` -> **exit 0** (`plan.py validate` OK, `guard.py` OK 130 files, `ruff` check/format OK, `pytest` 6 passed, `bun test` 3 passed / 17 assertions, `cargo fmt --all -- --check` OK, `cargo clippy --workspace --all-targets -- -D warnings` OK, `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` OK, `cargo test --workspace --all-targets` & `--doc` OK, `zensical build --strict` `No issues found`, `check_docs_honesty.py` OK).
- `cargo test -p slipfork-snow -p slipfork-world -p slipfork` -> **exit 0** (`slipfork`: 6 critical; `slipfork-snow`: 8 critical; `slipfork-world`: 2 unit + 6 critical; 0 failures).
- `rg -n '"\{project\}\.localhost"|format!\("\{project\}' crates/slipfork-snow/src` -> **exit 0** (1 match only: `crates/slipfork-snow/src/model.rs:643: bucket: format!("{project}-{}", b.id),` — S3 bucket name; zero `.localhost` host/URL formatting in `slipfork-snow`, rendered exclusively via `LocalHostRoute::host()` / `LocalHostRoute::url()`).
- `uv run --no-project tools/critical_guard.py --base 7413b98caea8539187d151d32cbfcbc9e9567455 --node M1.6` -> **exit 0** (`critical-guard: OK: authorized by M1.6`).

## Files Touched in Gate 8 Defect Fix

- `crates/slipfork-snow/src/model.rs`
- `plan/nodes/M1/M1.6/RESULT.md`
