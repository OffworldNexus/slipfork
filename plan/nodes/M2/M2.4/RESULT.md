# M2.4 RESULT — `Send + Sync` providers, redacted `Debug`, mode-scoped `effective_components`

STATUS: done (attempt 1)

Base: `6e6a27638e3892127a3e3d4c085409507e82b41e` (`release/0.2.0`); working tree left uncommitted for `lead`.

## Summary

1. **`Send + Sync` ports with `&self` (`ARCHITECTURE.md` §3.1, `ADR-0001` §3).**
   `pub trait SecretProvider: Send + Sync { fn get(&self, …); fn ensure_secret(&self, key, proposed) }`
   and `pub trait EntropySource: Send + Sync { fn fill_bytes(&self, buf) }`; `resolve_mode`,
   `render_template` and `GeneratorSpec::generate` take `&dyn` ports, so one provider/entropy value can
   be shared by reference across resolver threads. Interior synchronization lives only in the adapters
   (`MemorySecretProvider` → `RwLock<VaultRecord>`; `FileVaultProvider` → `flock` around each write).
2. **`FileVaultProvider::ensure_secret` is a real compare-and-set.** `open` keeps no cached copy; the
   vault is read through on every call. A write takes `File::lock()` (exclusive `flock`) on the vault
   inode, re-reads the vault **under the lock**, and rewrites through a `0600` `O_EXCL` temp
   (`create_new`) + `fsync` + atomic `rename`. Because the rename retires the locked inode, `update`
   verifies `dev`/`ino` identity with the path after locking and re-acquires if a writer replaced it —
   so the losing racer performs **zero** writes and returns the winner's value. A stale `.json.tmp`
   left by a crashed writer is cleared under the lock. `put` goes through the same locked path.
   `LayeredSecretProvider<Primary, Fallback>` reads `primary` then `fallback` and writes only
   `primary` (H-003 read local → remote, write local), and converges on an already-stored remote
   secret without writing anything.
3. **Redaction is total.** Hand-written `Debug` for `ResourceCoordinates`, `S3Coordinates`
   (`secret_key` → `[REDACTED]`) and `SecretValue` (`SecretValue([REDACTED])`, never the raw text).
   `RejectedKey` no longer keeps the rejected value at all: `serde::de::IgnoredAny` records presence
   only, so a literal secret in a forbidden `envs:` block cannot reach any `Debug`/`Display` path.
   `TopLevelEnvsUnsupported` + the separate `project:` validation string collapse into the single
   `SnowError::ForbiddenTopLevelKey { key }` (M1.6 #7, #21).
4. **Mode-scoped components through one function.** `SnowFile::effective_components(mode) ->
   Result<ResolvedComponents, SnowError>` (`Vec<(String, ResolvedComponent { run_command, disabled })>`,
   `components[]` order with mode-only additions appended) resolves override / addition (non-empty
   `run_command` required) / removal via the sole spelling `disabled: true`; `modes.all.components`
   and unknown-mode ids fail closed in one `SnowError::Validation`. `effective_run_command` (now
   `Result<String, …>`) and `resolve_mode`'s component selection both delegate to it, and
   `validate_structure` folds the same function's errors into its exhaustive accumulator — there is no
   second reader of `modes.<mode>.components`.
5. **`for_world` + `WorldOrigin` (`ADR-0001` §3, `ADR-0008` §7, M2.3 handoff).**
   `ResourceCoordinates::for_world(snow, project, target, origin, s3_overrides)` is the sole coordinate
   builder: it validates `target` against `domains[].id`, renders `app_url` straight from
   `origin.url()` and `app_domain` from `WorldOrigin::host()` (the single host accessor: authority
   without scheme/port/path, bracketed IPv6 kept as-is), and applies plain `S3Coordinates` overrides
   for declared buckets (undeclared ids fail closed with `SnowError::UnknownS3Bucket`).
   `local_for_world` is a thin delegate minting `WorldOrigin::local(&route, 80)`, so **no
   portless-origin helper remains outside `WorldOrigin`**: `LocalHostRoute::url()` is deleted from
   `crates/slipfork-world` and no URL/authority parser survives in `slipfork-snow`.
6. **M1 REST cruft deleted.** `OsppBundle` and `MemorySecretProvider::from_bundle` are gone; the vault
   shape is the private `VaultRecord` (`vars`/`secrets`/`domains`, `deny_unknown_fields`) shared by
   both local adapters, and `MemorySecretProvider` is built via `new()` / `insert()`.
7. **Strict schema.** `#[serde(deny_unknown_fields)]` now covers every nested manifest struct
   (`BuildImage`, `Component`, `Ingress*`, `Hook`, `Domain`, `PgCluster`, `PgDatabase`,
   `RedisCluster`, `S3Bucket`, `Resources`, plus the already-covered `ModeSpec` /
   `ModeComponentOverride`), making the published "unknown fields fail closed" claim true. An unknown
   key inside `modes.<m>.components.<id>` is a hard `SnowError::Yaml`.
8. **Purity retained.** `#![forbid(unsafe_code)]`; zero `std::env` / `std::net` / clock / `Command` in
   `src/` (verified by `rg`); the `0600` vault adapter remains the only filesystem code and uses only
   safe std APIs (`File::lock`, `OpenOptionsExt::mode`, `MetadataExt`, `create_new`) — no `libc`, no
   new dependency.

## Acceptance

```console
$ cargo test -p slipfork-snow --all-targets
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
running 12 tests
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

$ cargo test -p slipfork-snow --test critical_snow
running 12 tests
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

$ cargo test -p slipfork-snow --test critical_snow debug
running 1 test
test coordinates_and_s3_debug_redact_every_secret ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 11 filtered out; finished in 0.00s

$ rg -n 'derive\(Debug\)' crates/slipfork-snow/src/model.rs | rg 'S3Coordinates|ResourceCoordinates|RejectedKey'   # negated
(no output)

$ rg -q 'fn effective_components' crates/slipfork-snow/src/model.rs
exit 0

$ rg -q 'OsppBundle|from_bundle' crates/slipfork-snow/src   # negated
(no output)

$ cargo clippy -p slipfork-snow --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.23s

$ make check
… test result: ok × 21 targets …
check_docs_honesty: OK (8 published doc(s) match real CLI subcommands)
exit 0
```

Additional gates:

- `cargo test -p slipfork-world -p slipfork-snow --all-targets` → world 15 critical + 1 unit, snow 12 → all `ok`.
- `cargo fmt -p slipfork-snow -p slipfork-world -- --check` and `cargo fmt --all -- --check` → exit 0.
- `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` → exit 0.
- `uv run --no-project tools/guard.py` → `guard: OK (250 file(s) checked)`.
- `uv run --no-project tools/critical_guard.py --base 6e6a276 --node M2.4` →
  `M crates/slipfork-snow/tests/critical_snow.rs [authorized by crates/slipfork-snow/**]` and
  `M crates/slipfork-world/tests/critical_world.rs [authorized by crates/slipfork-world/tests/critical_world.rs]`;
  the only remaining `VIOLATION` lines are the concurrent M2.5/M2.6 critical suites
  (`crates/slipfork-dag/tests/critical_{dag,support}.rs`, `crates/slipfork-engine/tests/critical_engine.rs`),
  outside this node.

## Batched-gate follow-up (review-legality, non-blocking)

`slipfork-snow` no longer hand-parses an authority out of an already-validated `WorldOrigin` URL.
`WorldOrigin::host() -> &str` (`crates/slipfork-world/src/lib.rs`) is now the **sole** host accessor:
it returns the authority without scheme, port, or path (bracketed IPv6 keeps its brackets so the value
stays a valid `Host` authority), and `ResourceCoordinates::for_world` takes `app_domain` from it. The
local `origin_host` helper is deleted. `tests/critical_world.rs` pins `host()` for
`http://acme.localhost` → `acme.localhost`, `http://acme.localhost:7080` → `acme.localhost`,
`http://feat-auth.acme.localhost:65535` → `feat-auth.acme.localhost`,
`http://test-3.acme.localhost:8000` → `test-3.acme.localhost`, plus shared
`https://acme.tunnel.example:8443/app` → `acme.tunnel.example`, bracketed `http://[::1]:8080` → `[::1]`,
and a scheme+host-only URL. All gates above were re-run after this change (`make check` exit 0).

Two more gate findings were fixed in the same pass:

- **`MemorySecretProvider` redaction hole.** `Debug` was derived over `RwLock<VaultRecord>`, so
  `{:?}` could print plaintext secrets. It is now a hand-written redacting `Debug`
  (`MemorySecretProvider(<redacted>)`), and the critical suite asserts the exact output and that a
  stored secret value never appears in it (nor through the enclosing `LayeredSecretProvider`).
- **Whole-mode `resolve_mode(None)` used to resolve disabled components' env groups.** The sweep now resolves
  `effective_components(mode)` first and drops groups consumed **only** by `disabled: true`
  components (`component_env_groups`), while still resolving groups no component references (image
  build envs / diagnostics) and still failing closed when a disabled component is named explicitly.
  Pinned by `whole_mode_resolution_skips_disabled_component_env_groups` (`cloud` sweep has no
  `WORKER_KEY`, keeps `API_KEY` and the unreferenced `OPS_TOKEN` group; `local` sweep keeps
  `WORKER_KEY`; `resolve_mode("cloud", Some("worker"))` is a `Validation` error).

## Adversarial assertions pinned in `tests/critical_snow.rs`

- `assert_send_sync::<dyn SecretProvider>()`, `::<dyn EntropySource>()`, `::<FileVaultProvider>()`,
  `::<MemorySecretProvider>()`, `::<LayeredSecretProvider<…>>()`, `::<OsCsprngEntropy>()`.
- 8 barrier-synchronized threads call `ensure_secret("SECRET_KEY", "proposed-N")` through one
  `&dyn SecretProvider`: every racer returns the **same** value, that value is one of the proposals,
  a re-opened provider reads the same value, and the directory holds only `acme.json` (one write, no
  temp leftovers). A crashed-writer `acme.json.tmp` is recovered.
- `Debug` of `S3Coordinates`, `ResourceCoordinates` and `SecretValue` contains `[REDACTED]` and never
  the secret; `format!("{value:?}") == "SecretValue([REDACTED])"`; `format!("{provider:?}") ==
  "MemorySecretProvider(<redacted>)"` for a provider holding a known secret, including inside a
  `LayeredSecretProvider`.
- Top-level `envs:` (mapping **and** explicit `null`, with a literal secret inside) and `project:`
  (mapping and `null`) → `SnowError::ForbiddenTopLevelKey { key }`; the `Display` and `Debug` of the
  error name the key only and never the literal secret.
- `effective_components("local")` order is `["api", "worker", "frontend"]` with the mode-only addition
  appended; `cloud` keeps `worker` disabled (`effective_run_command` refuses it) and never sees
  `frontend`; `disabled: true` on `modes.all.components` is refused; a mode-only addition without
  `run_command` is refused; an unknown override field is a `Yaml` error; unknown mode is `UnknownMode`.
- `for_world` at `WorldOrigin::local(&route, 8080)` → `app_url = http://acme.localhost:8080`,
  `app_domain = acme.localhost`; a shared origin `https://acme.tunnel.example:8443` →
  `app_domain = acme.tunnel.example`; an `s3_overrides` entry replaces the bucket coordinates and an
  undeclared id is `SnowError::UnknownS3Bucket`.

## Caller migrations outside `crates/slipfork-snow/**` (now covered by the amended `paths`)

The mandated `LocalHostRoute::url()` deletion and the `&self` provider promotion have two mechanical
callers outside `crates/slipfork-snow/**`; both changes are minimal, non-weakening and were required
for `make check` to be green:

- `crates/slipfork-world/tests/critical_world.rs` — `expected.url()` → `WorldOrigin::local(&expected, 80)`
  for default-ingress routes and `Err(WorldError::OriginNotDefaultIngress)` for named `domains[].id`
  routes (strictly stronger: it also pins that a named domain is never a world origin). The node's
  earlier Context claim that `model.rs` was the only caller of `LocalHostRoute::url()` was stale.
- `crates/slipfork/src/main.rs` — `resolve_snow_file` drops the now-unnecessary `mut` on the vault and
  entropy bindings (`unused_mut` under `-D warnings`).

## Follow-ups (not in this node's paths)

- **`docs/reference/manifest.md` is now stale** and must be swept by an explicit docs node:
  `TopLevelEnvsUnsupported` / "top-level `project:` (`Validation`)" → `ForbiddenTopLevelKey`,
  §3 `slipfork_snow::OsppBundle` → the local `0600` vault record, "unknown component IDs fail
  validation" → mode-only additions (non-empty `run_command`) and `disabled: true` removal,
  `effective_run_command`'s return type, and `ResourceCoordinates::for_world(…, origin, s3_overrides)`.
- `docs/snow-modes.md` has the same stale references.
