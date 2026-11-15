# M2.3 RESULT — `WorldOrigin` + deterministic `<= 31`-byte `WeaverServiceName`

STATUS: done (attempt 2 — Round-1 Quad Gate remediation folded)

Base: `8d51a749e3e790730098dd4c5a1a515be4fb6081` (`release/0.2.0`); working tree left uncommitted for `lead`.

## Summary

1. **`WorldOrigin` (`ADR-0005` §1, `ADR-0008` §3, `H-010` #2).** `pub enum WorldOrigin { Local(LocalOrigin), Shared(SharedOrigin) }`
   with the `kind` tag / snake_case shape and a fail-closed `Deserialize`. The payloads are opaque
   (`LocalOrigin` / `SharedOrigin` hold a private `url`), so a struct literal can no longer mint an
   unvalidated origin: `WorldOrigin::local(&LocalHostRoute, ingress_port: u16)` and
   `WorldOrigin::shared(url)` are the **only** minting paths. `local` is **portless only at 80**
   (`http://<host>`, otherwise `http://<host>:<ingress_port>`) and fails closed with
   `WorldError::OriginNotDefaultIngress { domain }` when `route.snow_domain()` is `Some`. `shared`
   validates an absolute `http://`/`https://` URL (non-empty host, no whitespace).
   `WorldOrigin::url() -> &str` returns the rendered URL.
2. **`derive_weaver_service_name` (`ADR-0008` §6, normative).** Normalizes to lowercase dash-separated
   `[a-z0-9]` runs (every non-`[a-z0-9]` byte is a separator; leading/trailing/doubled separators
   collapse; a label with no alphanumeric byte fails closed). `<= 31` bytes is used verbatim **only**
   when the label is not `[0-9a-f]{12}` and does not end in `-[0-9a-f]{12}`; otherwise
   `prefix = normalized[..min(len, 18)].trim_end_matches('-')` and the result is `<prefix>-<sha256_hex12>`
   (bare hash in the spec-literal empty-prefix arm). The hash is SHA-256 of the **full normalized**
   label, so the verbatim and truncated codomains are disjoint and the name is identical across runs
   and hosts for the same full label.
3. **`WeaverServiceName`.** `WeaverServiceName::parse` is the only constructor and enforces
   `^[a-z0-9]+(-[a-z0-9]+)*$` plus `len() <= SERVICE_NAME_MAX (31)`; `Display`/`as_str` exist and
   `Deserialize` goes through `parse`, so the wire form fails closed too.
4. **M1.6 follow-ups cleared.** #1: the duplicated in-module `normalize_host` test is gone and the
   `test-99999999999` overflow case now lives in the route critical suite (its uppercase /
   trailing-dot / absolute-`Host:port` coverage was restored as an explicit critical assertion).
   #2: `is_reserved_test_slot_label` is private (behaviour asserted through `validate_fork_name`).
   #22: `src/conformance.rs` is deleted and the in-memory `TunnelRegistrar` double +
   `assert_tunnel_registrar_conformance` moved to `tests/support/mod.rs` (test-only, never shipped).
   #27: `is_lease_active` is no longer production API. #23: `LocalHostRoute::parse`'s single-label
   branch resolves `(target, snow_domain)` once and calls `Self::new` exactly once.
5. **Purity retained.** `#![forbid(unsafe_code)]` stays; zero `std::net`, `std::fs`, `std::env` in
   `src/`; no backend `match kind` ladder. `sha2`/`hex` (already workspace deps from M2.2) are the
   only new imports.

## Round-1 Quad Gate remediation

1. **[BLOCKER] Second `Local` constructor.** `WorldOrigin` variants now carry opaque payloads
   (`LocalOrigin` / `SharedOrigin`, private `url`), so `WorldOrigin::Local { url: String::new() }` no
   longer compiles outside the crate. `tests/critical_world.rs` fixtures go through
   `WorldOrigin::local` / `shared`; the fail-closed `Deserialize` matrix still feeds hand-written
   wire payloads with empty / schemeless / non-`http(s)` / host-less / unknown-kind URLs and asserts
   every one is rejected.
2. **[MINOR] Duplicate `Wire` schema.** `WorldOrigin` derives `Deserialize` via
   `#[serde(try_from = "Wire")]`; the single `Wire` enum owns the `kind` tag + `url` field schema,
   and `TryFrom<Wire>` runs the same validation.
3. **[MINOR] Duplicate test route builders.** `tests/support/mod.rs` now exports the single
   `route(project, target, snow_domain)` builder plus `canonical_route()`; the conformance suite and
   `critical_world.rs` both use it (no hand-built canonical/slot routes remain).
4. **[MINOR] Private-helper unit test.** The empty-prefix arm of `join_weaver_prefix` is spec-literal
   but unreachable from a normalized label, so its in-module unit test is kept; the determinism
   coverage lives in the public weaver suite.
5. **[NIT] Determinism, not idempotence.** Rustdoc now says the function is deterministic and stable
   across runs/hosts for the same full label, that its input is always a raw full label (never a
   previous output), and that re-deriving a truncated name re-enters the hash path
   (`f(f(x)) != f(x)`). A dedicated test asserts two calls on the same input are equal and that a
   second derivation of a truncated output differs.
6. **[NIT] Host round-trip coverage restored.** `ACME.localhost.:80` and
   `ADMIN.FEAT-AUTH.ACME.localhost.:443` must resolve to the same routes as their lowercase
   canonical spellings.

## Files

- `crates/slipfork-world/src/lib.rs` (modified: opaque `LocalOrigin`/`SharedOrigin` payloads,
  `try_from = "Wire"` deserialize, `WorldOrigin`, `WeaverServiceName`,
  `derive_weaver_service_name`, three `WorldError` variants, follow-up #1/#2/#23 cleanups)
- `crates/slipfork-world/src/conformance.rs` (deleted: moved to `tests/support/mod.rs`)
- `crates/slipfork-world/tests/support/mod.rs` (new: single route builder + test-only
  `InMemoryTunnelRegistrar` + conformance suite)
- `crates/slipfork-world/tests/critical_world.rs` (modified: `weaver*` / `world_origin*` critical
  tests incl. determinism and uppercase-host round-trip, route-suite overflow case; existing
  assertions unchanged)
- `crates/slipfork-world/Cargo.toml` (modified: `[dev-dependencies] serde_json.workspace = true`)
- `Cargo.lock` (mechanical: adds the already-locked `serde_json` to `slipfork-world`'s dependency list)
- `plan/nodes/M2/M2.3/LOG.md` (remediation + ownership note via `plan.py log`)

## Acceptance evidence (exact commands — every command re-run on the final tree)

1. `cargo test -p slipfork-world --all-targets` → exit 0
   ```
   running 1 test
   test result: ok. 1 passed; 0 failed; 0 ignored; ...
   running 14 tests
   test result: ok. 14 passed; 0 failed; 0 ignored; ...
   ```
   `cargo test -p slipfork-world --test critical_world weaver` → exit 0
   ```
   running 5 tests
   test weaver_collision_suffix_shaped_labels_are_hashed_not_used_verbatim ... ok
   test weaver_service_name_is_deterministic_but_not_idempotent ... ok
   test weaver_service_name_rejects_invalid_charset_and_length ... ok
   test weaver_service_name_is_verbatim_up_to_the_31_byte_boundary ... ok
   test weaver_service_name_truncation_strips_dashes_and_never_joins_bad_dashes ... ok
   test result: ok. 5 passed; 0 failed; ...; 9 filtered out
   ```
   `cargo test -p slipfork-world --test critical_world world_origin` → exit 0
   ```
   running 3 tests
   test world_origin_local_omits_port_80_and_renders_other_ports ... ok
   test world_origin_local_rejects_named_snow_domain_routes ... ok
   test world_origin_serializes_with_a_kind_tag_and_deserializes_fail_closed ... ok
   test result: ok. 3 passed; 0 failed; ...; 11 filtered out
   ```
2. `cargo test -p slipfork-world --test critical_world weaver_collision` → exit 0
   ```
   running 1 test
   test weaver_collision_suffix_shaped_labels_are_hashed_not_used_verbatim ... ok
   test result: ok. 1 passed; 0 failed; ...; 13 filtered out
   ```
3. `! rg -q 'pub mod conformance' crates/slipfork-world/src` → exit 0
   `! rg -q 'pub fn is_reserved_test_slot_label' crates/slipfork-world/src` → exit 0
4. `! rg -q 'std::net|std::fs|std::env' crates/slipfork-world/src` → exit 0
   `cargo clippy -p slipfork-world --all-targets -- -D warnings` → `Finished \`dev\` profile ...`, exit 0
   `make check` → exit 0 (all stages; `zensical build --strict` → `No issues found`,
   `check_docs_honesty: OK (8 published doc(s) match real CLI subcommands)`)

Additional gates:
- `cargo fmt --all -- --check` → exit 0 (no output).
- `RUSTDOCFLAGS="-D warnings" cargo doc -p slipfork-world --no-deps` → exit 0.
- `uv run --no-project tools/guard.py` → `guard: OK`.
- `uv run --no-project tools/critical_guard.py --base 8d51a749e3e790730098dd4c5a1a515be4fb6081 --node M2.3`
  → `critical-guard: OK: authorized by M2.3` (only `tests/critical_world.rs` is protected, covered by
  `crates/slipfork-world/**`).

## Adversarial assertions pinned in `tests/critical_world.rs`

- `"a".repeat(31)` is verbatim; `"a".repeat(32)` → `aaaaaaaaaaaaaaaaaa-3ba3f5f43b92` (exactly 31 bytes).
- A 38-byte label whose 18-byte cut lands on `-` → `bbbbbbbbbbbbbbbbb-2a0052359bde` (dash stripped; 30 bytes).
- Normalization makes `a--b`/`a-` joins impossible (`a--b` → `a-b`, `-a-` → `a`, `  ACME  ` → `acme`); every
  derived name re-parses through `WeaverServiceName::parse`.
- `foo-0123456789ab` → `foo-0123456789ab-25dbe45e8a2f` and `0123456789ab` → `0123456789ab-d407ad901895`
  (the truncated-suffix shape is never used verbatim); a 31-byte label ending in 12 hex bytes with no separator
  stays verbatim.
- Two 39-byte labels sharing their first 18 bytes get the same prefix and different suffixes.
- Determinism: the same full label derives the same name twice; re-deriving a truncated output differs
  (documented `f(f(x)) != f(x)`, never claimed otherwise).
- `ACME.localhost.:80` / `ADMIN.FEAT-AUTH.ACME.localhost.:443` parse to the same routes as their
  canonical lowercase spellings.
- `WorldOrigin::local` at `80` omits the port and at `7080`/`65535`/`8000` renders `:<port>`; named
  `domains[].id` routes are rejected; `Serialize` emits `{"kind":"local"|"shared","url":...}` and
  deserialization rejects empty, schemeless, non-`http(s)`, host-less, and unknown-kind payloads.

## Notes

- **`LocalHostRoute::url()` ownership (M2.4).** `LocalHostRoute::url()` deletion + the
  `slipfork-snow` `app_url` migration to `WorldOrigin::local(&route, 80)` is owned by **M2.4** (path
  ownership): its only caller is `crates/slipfork-snow/src/model.rs:669`, outside M2.3's paths.
  `LocalHostRoute::url()` was left untouched here.
- **Non-blocking observation (review-ioc).** A canonical project named `acme-feat` and a fork `feat`
  of project `acme` both produce the full label `acme-feat` and therefore the same derived service
  name. This is a tunnel-uniqueness question for the gated tunnel work (`M2.22` / `H-006`), not a
  `derive_weaver_service_name` defect: the derivation is a pure function of the full label it is
  given.
- **`Cargo.lock`:** the only file outside `crates/slipfork-world/**`; it is the mechanical one-line
  dependency-list update for the test-only `serde_json` dev-dependency (already a workspace
  dependency used by other crates). It lets the critical suite assert the normative serde wire shape
  and fail-closed deserialization without adding a new third-party crate.
- **Normalization intent:** `ADR-0008` §6's "validate/normalize" wording is implemented as the literal
  normalization the node specifies. The normalized label *is* the world label, so two raw labels that
  normalize identically deliberately denote the same world; the doc comment states this explicitly.
- **`H-010` #2:** no second minting path exists; `LocalOrigin`/`SharedOrigin` are opaque and
  `WorldOrigin::local(&route, ingress_port)` / `shared(url)` are the only constructors.
