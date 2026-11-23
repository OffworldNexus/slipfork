# Result M2.26 — `slipfork-dag`: `[world]` config (`world.image` + `world.stage` only, fail-closed)

## Summary

- **`crates/slipfork-dag/src/world.rs` (new):** the sole `[world]` schema from `ADR-0009`
  §`Decision` 1 — `WorldImageConfig { image: Option<String>, stage: Option<String> }`, two keys,
  `#[derive(Debug, Default, Clone, PartialEq, Eq)]`, zero fallbacks, zero precedence ladder, zero
  boolean mode flags. `WorldImageConfig::from_toml` is the **one** parse path and owns the whole
  accepted-key table (unknown key / non-string value → typed error), so there is no second serde
  schema to drift from it.
- **Typed `WorldConfigError` (never stringly `DagError::Toml`):** `NotATable`, `UnknownKey`,
  `NonStringValue`, `NoBuildableImage`, `AmbiguousWorldImage`, `UnknownWorldImage`,
  `EmptyWorldImage`, `EmptyWorldStage`, `UnknownWorldStage`, `AmbiguousWorldStage`. Each carries
  the exact actionable fix; `NoBuildableImage` prints the documented four-line `images:` snippet
  and the "a project that cannot be built into an image is not a project Slipfork can fork" line
  (`ADR-0009` §5.1).
- **`SlipforkConfig`:** new `pub world: WorldImageConfig` field, `RawSlipforkConfig.world:
  Option<toml::Value>` routed through `WorldImageConfig::from_toml` so unknown keys
  (`base`, `rootfs`, `dockerfile`, typos) and non-string values fail closed as typed variants
  instead of an opaque serde string. `DagError::WorldConfig(#[from] WorldConfigError)` is
  `#[error(transparent)]`.
- **`crates/slipfork-dag/tests/critical_world_config.rs` (new):** 13 protected tests covering the
  full acceptance matrix plus adversarial refusal paths; the one inline unit test that is not in the
  critical file (`non_table_world_fails_closed`) stays next to the parser.
- **Shared internals (cleanup pass):** `select_image` / `select_stage` share one private
  `exact_match` helper (find → exact match → `Unknown…{declared}`), and both `Dockerfile`
  target-stage diagnostics share the `CONTRACT_RULE_1` ("World Image Contract #1 violated:") const.

### Resolution design (stated choice: no `snow.yml`/`Dockerfile` parsing, no new dependency)

`slipfork-dag` gains no `slipfork-snow` dependency and performs zero ambient I/O. Validation is a
pure function over `&WorldImageConfig` plus the plain facts the Composition Root already owns,
split into **two phases** because `world.image` selects the `BuildImage`, and only that
`BuildImage`'s `Dockerfile` declares the named stages:

1. `WorldImageConfig::select_image(&[String] /* buildable snow.yml images[].id */)` — explicit
   `world.image` must name one; exactly one `BuildImage` is inferred; `>= 2` fail closed with the
   candidate list; zero `BuildImage` entries fail closed with the four-line snippet.
2. `WorldImageConfig::select_stage(&[String] /* named FROM ... AS <name> */, Option<&str> /* final
   stage name */)` — explicit `world.stage` must name a declared stage; otherwise the final stage's
   name is used and an unnamed final stage fails closed with the one-line `Dockerfile` fix
   (`world.stage` defaults to the stage the `BuildImage` builds). Blank `world.image` /
   `world.stage` values are rejected (`EmptyWorldImage` / `EmptyWorldStage`).

`M2.27`'s pure `Dockerfile` stage parser supplies the stage facts; `slipforkd` (`M2.29`) is the only
place that reads `snow.yml`/`Dockerfile`/env and calls these two pure functions.

## Verification (real commands & exit codes)

- `cargo test -p slipfork-dag --test critical_world_config` -> **exit 0**
  (`running 13 tests` / `test result: ok. 13 passed; 0 failed`).
- `rg -n "world\.base|slipfork_base|CuratedBase|prebuilt" crates/slipfork-dag` -> **exit 1**
  (no match; the superseded config shapes are not aliased anywhere).
- `cargo test -p slipfork-dag --all-targets` -> **exit 0**
  (1 unit + 11 `critical_dag` + 4 `critical_support` + 13 `critical_world_config`, 0 failures).
- `cargo test -p slipfork-dag --all-targets --no-default-features` -> **exit 0**
  (same 29 tests, 0 failures).
- `cargo clippy -p slipfork-dag --all-targets -- -D warnings` -> **exit 0**;
  `RUSTDOCFLAGS="-D warnings" cargo doc -p slipfork-dag --no-deps` -> **exit 0**.
- `cargo fmt --all -- --check` -> **exit 0**.
- `cargo clippy --workspace --all-targets -- -D warnings` and `make check` currently fail **only**
  inside `crates/slipfork` (12 in-flight `crates/slipfork/src/daemon.rs` errors from `M2.16`, which
  is editing that crate in parallel — zero `slipfork-dag` diagnostics). Scoped equivalents above
  all pass; re-run the workspace gates once `M2.16` lands.
- `uv run --no-project tools/guard.py` -> **exit 0** (`guard: OK (321 file(s) checked)`).
- `uv run --no-project tools/critical_guard.py --base HEAD --node M2.26` -> **exit 0**
  (`A crates/slipfork-dag/tests/critical_world_config.rs [added (always allowed)]` /
  `critical-guard: OK: authorized by M2.26`). No existing critical test was modified.

## Scope note (sequencing detail, not a design change)

`docs/design/world-base-image.md` §8's table row for `M2.26` also mentions folding
`l0_rootfs_digest` into the `DagPlanner` `base` fixture root. That is **not** in this node's
authoritative `NODE.md` body/acceptance and cannot be computed in `slipfork-dag`: the `RootfsDigest`
formula (`ADR-0009` §3) hashes `Dockerfile` bytes, the `.dockerignore`-filtered context merkle,
`crun`/`world-init` hashes, build envs, and target arch — all owned by `M2.27`'s
`DockerfileStageBuilder`/`RootfsDigest`. The `base` root input therefore folds in during the
`M2.27`/`M2.29` wiring, with no DI blind spot and no parallel path here.

## Files Touched

- `crates/slipfork-dag/src/world.rs` (new)
- `crates/slipfork-dag/src/lib.rs`
- `crates/slipfork-dag/tests/critical_world_config.rs` (new)
- `plan/nodes/M2/M2.26/RESULT.md`
