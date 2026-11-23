# RESULT M2.16 — `slipforkd` Composition Root + thin CLI part 1

**Status:** complete (attempt 5). All acceptance and quality gates exit `0`; `critical_guard.py --base de666ad --node M2.16` authorizes the two protected test files; `pgrep -a slipforkd` is empty after the suite.

## Lead rulings applied

1. **DI: `ProjectVaults` capability** — `crates/slipfork/src/vaults.rs` now owns
   `trait ProjectVaults { fn vault(&ProjectSlug) -> Result<Arc<dyn SecretProvider>, EdgeError>; fn mutable_vault(&ProjectSlug) -> Result<Arc<dyn MutableSecretStore>, EdgeError>; }`
   implemented by `LayoutProjectVaults` over `Layout` + `FileVaultProvider` (`0600`). `Layout` now owns `data_home` (`Layout::data_home`, `Layout::vault_path`) and `Layout::resolve` takes the explicit `xdg_data_home`. `slipforkd` constructs exactly one `Arc<LayoutProjectVaults>` and injects it via `DaemonPorts`; `DaemonService::open_vault` and all free `data_home`/`vault_path` helpers are deleted.
2. **DI: read port typed** — `world_env`/`component_env` take `&dyn SecretProvider`; `boot` passes `vault.as_ref()`.
3. **DI: capability split** — `SecretProvider` is minimal again (`get`, `ensure_secret`; no default `Unsupported` methods, `ProviderError::Unsupported` deleted). New `MutableSecretStore: SecretProvider { put, remove, keys }` in `crates/slipfork-snow/src/provider.rs`, implemented by `FileVaultProvider`, `MemorySecretProvider`, and `LayeredSecretProvider` (put/remove/keys target the local write sink; remote read-only keys stay readable but unlisted/unremovable). `DaemonService` obtains it only through `ProjectVaults::mutable_vault` for `secret set|unset|list`. **Constraint:** `FileVaultProvider`'s inherent `put` is retained because the protected `crates/slipfork-ospp/tests/critical_ospp.rs` (outside this node's paths) calls it; the trait `put` delegates to that one implementation.
4. **DI + wire: `snow_resolve`** — added `SnowResolveRequest { cwd, file, mode, component, expose_secrets }` and `SnowResolveSummary { vars }` to `crates/slipfork-edge/src/daemon.rs`, `DaemonRequest::SnowResolve` / `DaemonReply::SnowResolved`, the server dispatch arm, the client method, and the lib re-exports. `DaemonService::snow_resolve` resolves the repository from the manifest's parent, opens the project vault through `ProjectVaults`, and returns already-rendered values (redacted unless `expose_secrets`). The thin CLI's `snow resolve` now calls the daemon and no longer opens a vault. The trait method has a fail-closed default for control backends that do not own a vault (the protected `slipfork-edge/tests/critical_edge.rs` `TestControl` fake is outside this node's paths; it receives the honest refusal without a faked vault).
5. **Docs truthfulness** — `README.md`, `docs/index.md`, and `docs/agents-and-opencode.md` now state the OpenCode plugin gateway is updated in `M2.18`; every `--snow-file`/`--workspace` example/claim is deleted. `check_docs_honesty.py` and `zensical --strict` are green.
6. **Auto-spawn cleanup proof** — the auto-spawn test asserts `kill -TERM -- -<pgid>` succeeds (with `--` so util-linux does not parse the negative pid as a signal), bounds the socket poll to 10 s, then hard-asserts `UnixStream::connect(daemon.sock).is_err()`. `pgrep -a slipforkd` is empty after `cargo test -p slipfork --all-targets`.
7. **Cruft** — one `project::repository_resolver()` constructor is used by the CLI and daemon; one `project::load_slipfork_config()` loader (missing file is an error, no silent `SlipforkConfig::default()`); `DaemonService::load_config` now fails closed with an actionable message when `<vcs_root>/slipfork.toml` is missing, and `up`/`fork` therefore require that manifest; the `force_local` identity branch is collapsed in `resolve_plan`; the stale `slipfork-ospp` mention is removed from `lib.rs`; `DaemonService::new` takes a `DaemonPorts` value (8→4 arguments); NODE invariant 2 and the context line now name the shipped `id -un` `UserIdentity` adapter (no engine `current_username`/`getpwuid_r`).

## Test-fixture consequences

- `critical_cli::repo_with_snow` now writes/commits `project = "acme-app"` in `slipfork.toml`, because world boot requires the resources/DAG manifest.
- `critical_slug` now passes `--runtime-dir <scratch>/rt` (the default runtime directory was shared across tests, so a daemon from one test served another and wrote its vault under the first test's `XDG_DATA_HOME`) and spawns each CLI in its own process group, reaping the auto-spawned daemon after the invocation (stderr silenced for invocations that spawn none). `snow resolve` is daemon-served in these tests.

## Acceptance / gate evidence (exact)

```text
$ cargo test -p slipfork --all-targets
test result: ok. 16 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out   (lib)
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out    (critical_cli)
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out    (critical_slug)
$ pgrep -a slipforkd
(no output)

$ cargo test -p slipfork --test critical_cli -- --nocapture up_down
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out
$ ... one_url_per_world
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 5 filtered out
$ ... home_unset
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out
$ ... share_blocked
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out

$ cargo test -p slipfork-snow -p slipfork-dag -p slipfork-engine
snow:    4 unit + 12 critical_snow                                     ok
dag:     6 unit + 11 critical_dag + 4 critical_support + 13 critical_world_config   ok
engine:  4 unit + 14 critical_engine (20.39 s)                         ok

$ cargo clippy --workspace --all-targets -- -D warnings                  # exit 0
$ cargo fmt --all -- --check                                            # exit 0
$ RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps            # exit 0

$ make check                                                            # CHECK_EXIT=0
No issues found                                                         (zensical --strict)
check_docs_honesty: OK (8 published doc(s) match real CLI subcommands)
# every workspace unit/integration/doc test green, including critical_cli 7 and critical_slug 6

$ uv run --no-project tools/critical_guard.py --base de666ad --node M2.16
critical-guard: de666ad..working tree, 64 changed file(s)
  M  crates/slipfork/tests/critical_slug.rs  [authorized by crates/slipfork/**]
  A  crates/slipfork/tests/critical_cli.rs  [added (always allowed)]
critical-guard: OK: authorized by M2.16
```

## FOLLOWUPS

1. **jj-aware resolution (M2.12/M2.17):** no `JjRepoRootResolver` exists. `repository_resolver()` is Git-only; a jj-only workspace fails closed, a colocated `jj git init --colocate` checkout resolves through Git, and the thin CLI never spawns `jj` (proved by `critical_slug`). Add a jj resolver in a later node.
2. **`EdgeError -> DaemonError` kind loss (edge, not in this node's paths):** `From<&EdgeError> for DaemonError` maps `EdgeError::Control(DaemonError)` to `Internal`, so typed daemon-originated kinds do not survive the wire (messages do). A lossless `Control` passthrough belongs in `crates/slipfork-edge/src/daemon.rs` in a follow-up.
3. **OSPP offer is `None` until `auth login` (M2.17):** no endpoint source exists; offline worlds report `prefer_remote -> local (not_offered)`. `slipfork-ospp` remains removed from `crates/slipfork/Cargo.toml` until the persistent client/signer/dump-store wiring lands.
4. **`prune` has no snapshot/cache GC:** `WorldEngine` exposes no snapshot deletion capability, so `PruneReport.snapshots` is empty rather than falsely claiming a reap.
5. **`FileVaultProvider::put` inherent method** remains alongside `MutableSecretStore::put` solely because the protected `slipfork-ospp` test (outside paths) calls the concrete method; deleting it requires that test update.
6. **`DaemonControl::snow_resolve` default refusal** exists so non-vault control backends (the protected edge `TestControl` fake) fail closed without a faked vault; production daemon/client override it.
7. **`secret` capability semantics:** `LayeredSecretProvider` lists/removes only the local write sink; remote-only keys are readable but not enumerated or removable.
8. **Daemon restart semantics:** base state snapshots are minted on first use per daemon lifetime and world handles never survive restart (`H-010` #1); `drop` of a fork unknown to a fresh daemon returns `false` without removing a stale checkout (an `open_workspace` capability would be required).
9. **`forward`/`auth_login`** remain honest fail-closed M2.17 rejections; `logs`/`hook_run` work for world services/hooks. `service_entry_command`/`exec_in_service` remain M2.29 as scoped.
