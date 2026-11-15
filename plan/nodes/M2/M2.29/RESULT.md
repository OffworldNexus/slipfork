# RESULT M2.29

Wired `ADR-0009` into the `slipforkd` composition root and the thin CLI, and repaired the M2.17
protected fixtures that the new `WorldSpec.rootfs` requirement invalidated. No engine code changed.

## What shipped

- **Typed request fold**: `ExecTargetRequest.service: Option<String>` replaces
  `ServiceExecTargetRequest`; `DaemonRequest::ServiceExecTarget`, the trait default method, client
  and server arms are gone. `critical_edge.rs` was mechanically updated (recorded as an authorized
  path in `M2.29`).
- **`slipforkd` composition root**: constructs `DockerfileStageBuilder` with
  `CliBuildxExporter` / `FsInputHasher` / `DockerOciImageStore` / `ElfAbiProbe`; resolves pinned
  `crun` (first-run download + SHA-256 verification, explicit `--crun` never downloads) and
  `slipfork-world-init`; resolves `--host-caches` mounts from the real `HOME`.
- **Rootfs at boot**: `up`/`fork` resolve `[world].image` via `WorldImageConfig::select_image`,
  pass `world.stage` through, build `snow.yml` image build-envs and the `SnowPortContract`, then
  `materialize(..)` with cache-hit reuse and a **required** `WorldSpec.rootfs`.
- **Backing services**: `pg_clusters.*`, `redis_clusters.*`, and `s3_buckets` start as
  `ServiceRuntime::OciContainer` on the same `spawn_service` surface, with CoW state mounts
  (`pgdata/<id>`, `redis/<id>`, `s3`), `$UID:$GID`, container `PATH`/`LANG`, and pre-created bind
  destinations that `docker export` omits for `VOLUME`s.
- **Service entry**: `slipfork db` / `slipfork redis` resolve `pg_databases.<id>` →
  `pg_clusters.<cluster>` (with `psql -d <db>`) or `redis_clusters.<id>`, and return
  `WorldEngine::service_entry_command`; a stopped container maps to typed
  `DaemonErrorKind::ServiceNotRunning`, never the app rootfs. `spawn_entry` drops the trailing
  `crun exec --` separator (the engine's emitted separator is not stripped by `crun`).
- **`--host-caches`**: per-invocation flag on `up`/`fork`, carried over the typed wire request;
  the builder mounts only after the ELF/libc ABI probe passes.
- **OCI adapter**: `src/oci_store.rs` resolves official image tags to `sha256:` digests, records
  `cache/oci/tags.json`, and exports `cache/oci/<hex>/{rootfs,config.json}` once.
- **Tests**: new protected `tests/critical_l0_wiring.rs` (real redis:8 container boot, `crun exec`
  entry, `ServiceNotRunning` after `redis-cli shutdown`, ABI refusal, matching-glibc pass).

## M2.17 fixture repairs (uncommitted review tree)

- `tests/support`: every fixture repo now writes a real `Dockerfile` and every `snow.yml` uses a
  buildable `images:` entry; daemon starts get `--crun <pinned>`.
- Reserved env names fixed: container env uses `ExecEnv::world_scoped` + `inherit(PATH/LANG)`,
  never `set("HOME"/"PATH"/"LANG")`.
- Postgres container `PATH` includes `/usr/lib/postgresql/<v>/bin`; Redis gets `LANG=C`.
- Forward fixture's inert target now accepts the readiness probe then closes its listener, so the
  dead-target isolation assertion is unchanged but boot succeeds.
- `db_redis_exec…` now asserts the resolved container entry requires a TTY (services are running);
  the stopped-service `ServiceNotRunning` refusal moved to `critical_l0_wiring`.

## Verification

```console
$ cargo test -p slipfork --all-targets
critical_cli 14 passed; critical_l0_wiring 4 passed; critical_slug 6 passed; critical_vcs_remote 1 passed

$ cargo test -p slipfork --test critical_l0_wiring
4 passed; 0 failed

$ M2.17 filters: hook_failure rescue_commands logs_follow forward_bridge db_redis_exec auth_login
  support_json (critical_cli) + git_fetch (critical_vcs_remote)
each: test result: ok. 1 passed

$ cargo test -p slipfork-edge --all-targets
3 + 31 passed; 0 failed

$ cargo test -p slipfork-engine --all-targets
28 + 14 + 3 + 13 passed; 0 failed; critical_rootfs_live 2 ignored (pre-existing live opt-in)

$ cargo clippy --workspace --all-targets -- -D warnings
Finished (exit 0; pre-existing `unused dependency socket2` cargo note)

$ cargo fmt --all -- --check
exit 0

$ RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
exit 0

$ make check
exit 0 (check_docs_honesty: OK, 8 published docs)

$ uv run --no-project tools/guard.py
guard: OK (349 files)

$ uv run --no-project tools/critical_guard.py --base 6465d4b --node M2.29
critical-guard: OK: authorized by M2.29

$ uv run --no-project tools/critical_guard.py --base b92c9aa --node M2.29
FAILED: only `crates/slipfork-engine/tests/critical_engine.rs`, whose change is committed in
6465d4b and authorized by M2.28 (`crates/slipfork-engine/**`). No M2.29-owned protected change is
unauthorized; b92c9aa predates the M2.28 commit, so this is a base-boundary artifact.
```

`M2.17`'s own protected test (`critical_cli.rs`) is authorized by its `crates/slipfork/**` path.
`M2.29`'s `paths` were extended with `crates/slipfork-edge/src/daemon/**` and
`crates/slipfork-edge/tests/critical_edge.rs` (the typed-request fold necessarily touches them).

## Deferred (explicit)

1. **Prune cache GC** (`cache/{rootfs,oci}` digests): no cross-restart reference index exists yet;
   naive GC would delete the offline/cache-hit guarantee. Lands with the engine snapshot/cache
   reference surface.
2. **Rootfs progress events**: `RootfsBuildEvent` is consumed but not yet forwarded as a
   `BootEvent`; cache hit/miss is silent to the CLI. No acceptance command requires it.
3. **SeaweedFS digest pin**: `ADR-0009` names only `docker.io/chrislusf/seaweedfs`, without a
   version/digest. The adapter resolves `:latest` once and records the real digest in `tags.json`
   (no invented pin); a compile-time pin needs an ADR decision.
4. **Docs promotion**: `docs/reference/cli.md` `psql`/`redis-cli` container semantics and
   `[world]`/`--host-caches` promotion are outside this node's locked `crates/**` paths and belong
   to `M2.19` (per design §8 deltas).
5. **Engine follow-up**: `ReflinkNetnsEngine::service_entry_command` emits a trailing `--` that
   `crun exec` treats as the command; the CLI strips it, but the engine should emit the exact argv.
