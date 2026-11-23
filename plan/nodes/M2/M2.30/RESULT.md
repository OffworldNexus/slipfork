# RESULT M2.30

Completed the `slipforkd` composition root so the shipped OSPP v1, seed-dump, freeze-world, and
Jujutsu surfaces are actually reachable at runtime. No engine or edge code changed.

## What shipped

### 1. Paired OSPP v1 session, layered vault, and resource offer

- `crates/slipfork/src/ospp.rs`: injected `OsppSessionStore` port + `FileOsppSessionStore`
  (`0600` `<data_home>/slipfork/ospp-session.json`, `{host,port,tls,account}`, zero secrets,
  atomic rename). `DaemonService::auth_login` writes it after a successful pairing and drops the
  cached client.
- `DaemonService::ospp_client` lazily dials `OsppEndpoint::connect` once and caches the
  `Arc<OsppClient>`; an unpaired/offline host yields `None` (local vault still serves).
- `LayoutProjectVaults::vault(project, Some(client), mode)` returns
  `LayeredSecretProvider<FileVaultProvider, OsppSecretProvider>`; `FileVaultProvider` remains the
  sole `!!` write sink.
- `resolve_plan` now takes `Option<&RemoteResourceOffer>` and passes
  `OsppClient::resource_offer(project, "local")` into `ResourcePlan::resolve` (no more `None`).

### 2. Real base snapshot: seed world → `pg_restore` → `CHECKPOINT;` → `freeze_world`

- `ensure_base_snapshot` is keyed by `(project, base_digest)` over backing-service versions and
  every resolved dump digest. It boots a short-lived seed world on an empty snapshot, starts the
  backing-service `OciContainer`s, restores each selected dump with
  `exec_in_service(pg_restore --no-owner --no-privileges --clean --if-exists ...)` followed by
  `psql ... -c "CHECKPOINT;"` **inside** `postgres:18`, seals the seed world with `freeze_world`,
  and destroys it before `pre-deploy`.
- `DiskSeedDumpStore::materialize` (new `SeedDumpStore` capability) decompresses the verified
  archive with a bounded `MAX_DUMP_WINDOW_LOG` window into `0600` `<sha256>.pgcustom` (atomic
  rename); the container mounts it read-only at `/dumps/<sha256>.pgcustom`.
- `--no-dump` seals the clean `initdb` base through the same seed-world path; no dump is streamed.
- `ensure_database` creates each declared `pg_databases.<id>` in the fresh cluster before restore.
- `service_specs` now carries `readonly_mounts` and pre-creates the `/dumps/...` file destination
  in the OCI image rootfs (the engine requires the bind destination to exist).
- SeaweedFS starts with `-ip=127.0.0.1 -ip.bind=127.0.0.1`: the world netns has no DNS resolver,
  so `weed` must never resolve `localhost` for its master/filer/S3 bind addresses.

### 3. Optional `slipfork.toml`

- `project::load_optional_slipfork_config`: `NotFound` → `SlipforkConfig::default()`; malformed or
  unreadable still fails closed. `DaemonService::load_config` uses it.

### 4. `.jj` routing

- `crates/slipfork/src/vcs.rs`: `RepositoryWorkspaces` selects `JjWorkspaceAdapter` iff
  `<vcs_root>/.jj` is a directory, else `GitWorktreeAdapter`; a missing `jj` fails closed with
  `VcsError::ToolMissing` and never creates a Git worktree. `slipforkd` injects it.
- Fixed the stale `main.rs` doc comment: `snow resolve` runs through `slipforkd`.

### 5. Shared test support

- `slipfork-ospp` gained an opt-in `test-server` feature exposing
  `tests/support/reference_server.rs` (and the pinned-protocol fake `tpmd` server) as
  `slipfork_ospp::test_server` / `test_server_tpmd`; `crates/slipfork` enables it only from
  `[dev-dependencies]`. Zero WebSocket/tpmd server code was duplicated.
- The fake `tpmd` now keeps an authenticated connection open across idle capability gaps
  (previously it closed after 100 ms, which broke a daemon that signs later).

## Reachability matrix (each caller is exercised by a failing test if removed)

| Newly reachable caller | Runtime path | Test that fails if removed |
|---|---|---|
| `OsppClient` | `DaemonService::ospp_client` → vault/offer/dump | `paired_ospp_...` (`snow resolve`/`up` cannot connect) |
| `OsppSessionStore` / `FileOsppSessionStore` | `DaemonPorts.ospp_sessions`, `auth_login` save | `paired_ospp_...` (no coordinates → unpaired) |
| `LayeredSecretProvider` / `OsppSecretProvider` | `LayoutProjectVaults::vault` | `paired_ospp_...` remote vars/secrets assertions |
| `OsppClient::resource_offer` | `DaemonService::resource_offer` → `resolve_plan` | `paired_ospp_...` remote S3 endpoint assertion |
| `DumpStreamer` | `materialize_seed_dumps` | `paired_ospp_...` `dumps_served == ["main"]` |
| `DiskSeedDumpStore` + `materialize` | `slipforkd` port; `.pgcustom` materializer | `paired_ospp_...` restore/fork rows |
| `WorldEngine::exec_in_service` | `exec_service_checked` (`pg_restore`, `CHECKPOINT`, `createdb`) | `paired_ospp_...` restored rows |
| `WorldEngine::freeze_world` | `ensure_base_snapshot` | `paired_ospp_...` base `PG_VERSION` + fork rows |
| `JjWorkspaceAdapter` | `RepositoryWorkspaces` | `dot_jj_...` success + `ToolMissing` |
| Optional `slipfork.toml` | `load_optional_slipfork_config` | `optional_slipfork_toml_...` |
| `test-server` reference server | `slipfork_ospp::test_server` | every OSPP test in `critical_ospp_daemon` |

## Acceptance (exact results)

```
$ cargo test -p slipfork-ospp --all-targets
test result: ok. 32 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
EXIT=0

$ cargo test -p slipfork --test critical_ospp_daemon -- --nocapture
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
EXIT=0

$ cargo test -p slipfork --test critical_cli -- --nocapture
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
EXIT=0

$ cargo test -p slipfork --test critical_l0_wiring -- --nocapture
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
EXIT=0

$ cargo clippy --workspace --all-targets -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.10s
EXIT=0

$ make check
Build finished in 0.19s
check_docs_honesty: OK (9 published doc(s) match real CLI subcommands)
EXIT=0

$ uv run --no-project tools/guard.py
guard: OK (375 file(s) checked)
EXIT=0

$ uv run --no-project tools/critical_guard.py --base 6265eeb --node M2.30
critical-guard: OK: authorized by M2.30
EXIT=0
```

## Notes

- The seed-dump test produces its archive from a real `pg_dump -Fc` executed inside the running
  `postgres:18` container, then serves it through the reference OSPP server; the fork test deletes
  the local dump cache before forking and asserts `dumps_served` stays at one, proving the sealed
  base is reused rather than re-streamed/re-restored.
- The session file lives at `<data_home>/slipfork/ospp-session.json` (the Slipfork data namespace,
  alongside the vault and software device key), not directly in `<data_home>`.
- One earlier parallel `make check` run hit the pre-existing `slipfork-engine`
  `critical_oci_runtime` postgres-readiness race; the final run above is green.

## Review fix (legality/functional MAJOR)

`DaemonService::ospp_client` no longer turns every connect failure into an offline fallback. Only
`OsppError::Transport` yields `Ok(None)` (a genuinely unreachable server); an invalid endpoint or a
paired host that refuses the session (revoked key, bad signature, TLS/protocol refusal) now
propagates as a typed `EdgeError` naming the paired `host:port`. Both the connect path and
`resource_offer` invalidate the cached `Arc<OsppClient>` on a transport failure, so a dead socket is
never reused and the next call reconnects from the persisted coordinates.

Re-run after the fix:

```
$ cargo test -p slipfork --test critical_ospp_daemon       -> 4 passed, EXIT=0
$ cargo test -p slipfork --all-targets                      -> every binary green, EXIT=0
$ cargo clippy --workspace --all-targets -- -D warnings     -> EXIT=0
$ make check                                                -> EXIT=0
$ tools/critical_guard.py --base 6265eeb --node M2.30       -> OK: authorized by M2.30, EXIT=0
```
