# M3.28.17 — Restart vs `up`: one boot pipeline, truthful `WorldUpSummary`

Base `ae71c45`, attempt 3. Authority: `docs/design/dx-0.1.0-ux.md` §1/§4/§10, `ARCHITECTURE.md` §3.4.
Attempt-2 route/re-derivation work is kept and verified (round-2 legality PASS).

## The two ownership fixes

### 1. The engine owns "what this world runs" — `WorldEngine::stop_world_services`

- `crates/slipfork-engine/src/lib.rs`: new required trait method
  `stop_world_services(&self, world: &WorldHandle) -> Result<(), EngineError>` (components first,
  then containers, engine-bounded grace, idempotent, `WorldGone` on a destroyed/foreign world).
- `crates/slipfork-engine/src/btrfs_engine.rs`: implemented **once**, reusing
  `stop_services_with_grace` (the existing one stop path); the private `stop_live_services` is
  deleted and `freeze_world` now calls the trait method, so seal and restart share it.
- `crates/slipfork-engine/src/conformance.rs`: a row asserts the call succeeds on a live world and
  fails closed with `WorldGone` on a destroyed one, for **every** adapter.
- `crates/slipfork/src/daemon.rs`: `reboot` calls it instead of looping the daemon's own
  `LiveWorld.services` handles; a partial spawn failure can no longer leave a running service
  behind. `stop_service` stays the single-service API; the double now answers it `Unsupported` to
  prove the daemon never uses it as a stop path.
- Handle minting for out-of-crate adapters: `WorldEngine::mint_service_handle(&world, id, state)`
  (`Self: Sized`, mirroring `mint_world_handle`) plus `ServiceState::to_bits` as the public bit
  encoding; the `WorldEngine` block in `ARCHITECTURE.md` §3.4 documents both.

### 2. `restarted` comes from the engine rows

`restarted_service_ids(before, after)` is now a pure comparison of two `world_processes`
snapshots: the ids `Running` after the run minus those already `Running` before it. Only
`ServiceState::Running` rows can be named and an id is emitted once, so the Running/unique
guarantees are structural instead of validated after the fact. The daemon's list-building is gone:
`LiveWorld::started_service_ids` is deleted, and `summary` no longer reads rows a second time —
`record_world(live, rows, changed, restarted)` reports exactly the rows the run computed.

**Snapshot point (deliberate, and why):** the `before` snapshot is taken **after**
`stop_world_services` (every row `Stopped`) and **before** `start_world`. Ruling 2's wording says
"pre-stop", but a pre-stop snapshot would exclude every service that was running before the reboot
and is running after it — i.e. it would make `restarted` empty for a real restart, contradicting
both "the ids actually restarted" and the requested assertion `restarted == the engine's
post-reboot Running set`. Post-stop/pre-start is the only snapshot where the comparison means "what
this run brought to `Running`"; the test asserts the resulting set equals the post-reboot `Running`
set.

## One component list (MAJOR)

`ResolvedWorld.components: Vec<ResolvedComponent { id, run_command, readiness_port, env }>` is the
only enabled-component list: `resolve_world` builds it once, `start_world` iterates it (no second
`effective_components` call, no `disabled` re-skip, no `component_envs` map), and `run_hooks`,
`hook_exec_target`, `hook_run`, `resolve_hook`, and `forward_specs` read the same list.
`LiveWorld.enabled_components` + `LiveWorld.component_envs` are deleted, so a hook can no longer
observe a different environment than the component it runs for (the addendum's asymmetric
`unwrap_or_default` guard disappears structurally — no defaulting path remains).

## Docs

- `LiveWorld` documents the two groups: **boot-frozen** (`worktree`, `handle`, `rootfs`, `policy`,
  `workspace`, `hook_logs`, identity) and **re-derived by every run** (`snow`, plan/`resources`,
  `coords`, `base_env`, `components`, registry ingress/domains). The old `coords` doc claimed a
  `restart` never re-derives it; `reboot` overwrites it, so the docs now say so.
- `WorldUpSummary` rustdoc: "every boot is `changed: true`; `restart` is `changed: true` only when
  it started a service or ran a hook" (it previously claimed a `restart` is always `changed`).
- `ARCHITECTURE.md` §3.4: the `restart` trait line now says `changed` is observed; the
  `WorldRegistry` block documents `replace_world` + `EdgeError::WorldNotAttached` next to
  `attach_world` (and its duplicate-attach contract); the `WorldEngine` block documents
  `stop_world_services` and `mint_service_handle`.
- `world_next_commands(Running)` emits `slipfork open` and `slipfork restart` **before** Phase C
  ships those verbs (`M3.28.7` client / `M3.28.8` renderer / `M3.28.9` open). Left as instructed:
  the epic gate publishes only once all of them exist; the wire table is not narrowed to hide the
  gap.

## Tests

- **Real `DaemonService`** (`crates/slipfork/src/daemon.rs` module tests):
  `restart_applies_an_edited_snow_file_and_a_second_up_touches_nothing` now boots a world **with a
  component, a pre-deploy hook, an env value, and an ingress** over a recording `WorldEngine`
  double (rows + `mint_service_handle`), a stub `BaseImageBuilder`, a fixed `RepoRootResolver`, and
  the real registry/vault/workspace factory. It asserts: boot ⇒ `restarted == ["api"]` (one spawn,
  one hook, zero world stops); a second `up` ⇒ `changed: false`, empty `restarted`, and **no**
  spawn/stop/hook call; then edits the ingress port, the hook body, and the env value, and
  `restart` ⇒ one `stop_world_services` + one respawn + the hook, `restarted` **equals** the
  post-reboot `Running` set (`["api"]`), the registry serves the new port, the recorded hook argv is
  `echo two`, the respawned component's `GREETING` is `two`, and the handle was never destroyed.
- Pure: `restarted_ids_are_the_engine_rows_this_run_brought_to_running` (post-stop rows, idempotent
  empty, exited never named, crashed-then-back counts, duplicate rows collapse),
  `boot_verb` table incl. `(Restart, Stopped) → refusal`,
  `restart_on_a_never_booted_world_fails_closed_naming_up`, rerun-policy merge/refusals.
- Engine: conformance rows in `critical_engine` (15) and `slipfork-engine --lib` (66).
- Wire/registry (`crates/slipfork-edge/tests/critical_edge.rs`): restart over the real socket,
  typed `NotRunning` refusal naming `slipfork up`, `registry_replace_world_swaps_routes_without_touching_the_handle`,
  `world_next_commands_follow_the_one_state_table`, the §10 document-key/join test. The
  `TestControl` restart fixture remains wire plumbing only.

## Acceptance (exact output)

```console
$ cargo test -p slipfork-edge --lib
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

$ cargo test -p slipfork-edge --test critical_edge
test result: ok. 40 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.58s

$ cargo test -p slipfork --test critical_ospp_daemon -- --test-threads=1
running 4 tests
test dot_jj_repository_routes_to_jujutsu_and_never_falls_back_to_git ... ok
test optional_slipfork_toml_boots_and_malformed_config_fails_closed ... ok
test paired_ospp_resolves_remote_secrets_restores_a_real_dump_and_forks_rows ... ok
test unpaired_remote_strategy_and_refresh_dump_fail_closed ... ok
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 130.28s

$ cargo test -p slipfork --lib restart
running 4 tests
test daemon::tests::restart_applies_an_edited_snow_file_and_a_second_up_touches_nothing ... ok
test daemon::tests::restart_on_a_never_booted_world_fails_closed_naming_up ... ok
test daemon::tests::restart_rescues_a_failed_world_and_reruns_a_running_one ... ok
test daemon::tests::restarted_ids_are_the_engine_rows_this_run_brought_to_running ... ok
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 38 filtered out; finished in 0.13s

$ cargo test -p slipfork --lib
test result: ok. 42 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.12s

$ cargo test -p slipfork-engine --lib
test result: ok. 66 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 5.03s

$ cargo test -p slipfork-engine --test critical_engine -- --test-threads=1
test result: ok. 15 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 31.36s

$ ! grep -rn 'down.*then.*up\|restart: bool' crates/slipfork/src/daemon.rs ; echo one-pipeline
one-pipeline

$ cargo clippy --workspace --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s)
```

Gates:

```console
$ cargo fmt --all -- --check
fmt-ok
$ uv run --no-project tools/guard.py
guard: OK (561 file(s) checked)
$ uv run --no-project tools/critical_guard.py --base ae71c45 --node M3.28.17
critical-guard: ae71c45..working tree, 41 changed file(s)
  M  crates/slipfork-edge/tests/critical_edge.rs  [authorized by crates/slipfork-edge/tests/**]
critical-guard: OK: authorized by M3.28.17
$ uv run --no-project tools/plan.py validate
plan OK
$ make check
```

`make check` output is appended below after the run.

```console
cargo test -p slipfork --test critical_cli -- --test-threads=1
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 80.69s
cargo test -p slipfork --test critical_l0_wiring -- --test-threads=1
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 34.85s
cargo test -p slipfork --test critical_ospp_daemon -- --test-threads=1
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 139.83s
cargo test -p slipfork --test critical_btrfs_live -- --test-threads=1
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.28s
uvx --offline zensical build --strict
No issues found
uv run --no-project tools/check_docs_honesty.py
check_docs_honesty: OK (10 published doc(s) match real CLI subcommands)
```

`make check` ran every prerequisite to the end (the last one, `docs-check`, succeeded, so `make`
exited `0`); `ts-test` needed `$HOME/.bun/bin` on `PATH`. The load-sensitive
`world init … survived SIGTERM and SIGKILL` teardown timeout did not appear.

## Scope notes

- Files touched: `crates/slipfork-engine/{src/lib.rs,src/btrfs_engine.rs,src/conformance.rs}` (paths
  widened by the lead), `crates/slipfork-edge/src/{daemon.rs,daemon/client.rs,daemon/server.rs,lib.rs,registry.rs}`,
  `crates/slipfork-edge/tests/critical_edge.rs`, `crates/slipfork/src/daemon.rs`, `ARCHITECTURE.md`.
- `crates/slipfork/tests/support/mod.rs` and the `docs/` + `plan/` deltas in `git status` belong to
  other agents/nodes and were not touched here.
- No commit; the only `plan/` addition is this `RESULT.md`.
