# Result M2.6 (attempt 2) — `slipfork-engine`: multi-mount `WorldSpec`, redacted `ExecEnv`, opaque `WorldHandle`, `freeze_world`

**STATUS: done.** The engine contract from attempt 1 is unchanged; the transitional M1 CLI surface is
adapted so the workspace is green, no cross-process handle API exists, and every daemon-served
command now advertises and behaves honestly.

## Engine contract (reviewed implementation, unchanged)

| Invariant | Implementation |
|---|---|
| 1. Types & `EngineCapabilities` deletion | `EngineCapabilities` + `WorldEngine::capabilities()` are gone (`rg` finds zero hits). Added `SnapshotMode { DiskCow, StatefulRam }`, `MountMode { ReadOnly, ReadWrite }`, `WorldMount`, `WorldSpec { target, snapshot, mounts, base_env, local_ingress_port }`, opaque `WorldHandle` (private `engine`/`seq`, hand-written stable `Debug`, no serde, no constructor outside the engine), `ServiceSpec { id, argv, cwd, env, readiness_port }`. `SnapshotMode::StatefulRam` fails closed with `EngineError::StatefulRamUnsupported { engine }` — no capability bag. `SnapshotId::validate()` rejects path-escaping ids. |
| 2. Env redaction | `src/env.rs`: `ExecEnv { home, inherited: BTreeMap<InheritedVar,_>, plain, secrets }`, `InheritedVar { Path, Term, Lang }` (closed enum = the allowlist; `HOME` only through `world_scoped(home)`), `overlaid()` (`effective = base ⊕ overlay`, never a plain+secret duplicate), hand-written redacting `Debug` (`[REDACTED]`). Two release boundaries only: `expose_to_child_process()` — now **`pub(crate)`**, so only `src/child_env.rs` (the engine's spawner) can reach it and the one-boundary rule is compiler-enforced — and the serde wire form documented as `0600 <runtime_dir>/daemon.sock`-only for `DaemonControl::exec_target`. Ambient allowlist lookup lives in the adapters, never in the domain types. |
| 3. Mounts | `WorldSpec::validate()` fails closed on non-absolute paths and nested/equal RW host or guest paths (`EngineError::OverlappingMounts`). `ReflinkNetnsEngine` runs under `unshare --mount` and bind-mounts every `WorldMount` (`mount -o remount,bind,ro` for RO) with paths passed positionally (no shell interpolation). |
| 4. `freeze_world` | `Live -> Freezing -> Frozen` seal via a real `ioctl(FICLONE)` clone of the live state dir into `snapshots/<id>`; already-frozen / mid-seal / destroyed worlds fail closed with `EngineError::WorldNotFreezable`; a world destroyed mid-seal drops the half-claimed snapshot. Double destroy fails closed with `EngineError::WorldGone`. |
| 5. Conformance | `assert_world_engine_conformance`: typed `StatefulRam` contract, secret redaction through `ExecEnv`/`WorldSpec` Debug, `SnapshotNotFound`, nested-RW `OverlappingMounts`, clone → stable opaque handle `Debug` → seal-once freeze → `WorldNotFreezable` → `WorldGone` double-destroy. Real lineage/mounts/UID/parallel-same-port pinned against real reflink + namespaces (never mocked). `IncusEngine` implements the same trait against a Unix-socket protocol harness (live `incusd` gated on `H-004`). |
| 6. Purity | `unsafe` untouched outside `linux_cow.rs` (no `pty.rs`); no engine-name branching in domain code; ambient env reads only inside `incus.rs` / `reflink_netns.rs`. |

## Transitional CLI adaptation (`crates/slipfork/src/main.rs`)

- `snap` → `create_snapshot(source_dir, &SnapshotId(label), SnapshotMode::DiskCow)`.
  `doctor`, `snow resolve` and `dag plan` are untouched.
- `fork` / `exec` / `drop` → **fail closed, exit 2**. The thin CLI never constructs a
  `ReflinkNetnsEngine` to mint world state, and `WorldHandle` (daemon-owned, opaque) is never
  reconstructed: no constructor, `open_world`, `from_persisted` shim, or `#[allow]`. This is the
  transitional consequence of `H-010`/`ADR-0008` §7 — **all three are daemon-served from M2.17**
  (`DaemonControl::exec_target` / daemon-owned teardown).
- `daemon_owned_surface_fails_closed(json_out, command, target) -> !` diverges, so no caller can
  treat the surface as recoverable. It emits `{"ok":false,"error":"<diagnostic>"}` on stdout when
  `--json` is set and the human diagnostic on stderr otherwise (both exit `2`).
- Argument validation still names the real flag/cause before the refusal: `exec` rejects an empty
  argv, a missing `--cwd` directory and an unreadable/invalid `--snow-file`; `fork` rejects a
  path-escaping `--from` snapshot id and a missing `--workspace` directory.
- clap help/about for `fork`/`exec`/`drop` now reads “Daemon-owned: … served by slipforkd in 0.2.0
  (`M2.17`)”, so `--help` never advertises a command that refuses to run. Subcommand names and flags
  are unchanged, so `tools/check_docs_honesty.py` stays green.
- **Published-doc debt (explicit):** `README.md`, `docs/index.md`, `docs/getting-started.md` and
  `docs/agents-and-opencode.md` still document the M1 `fork`/`exec`/`drop` recipes. Those pages are
  owned by **M2.23** (purge stale M1 docs) and **M2.19** (publish the 0.2.0 DX docs); this node does
  not edit `docs/**`/`README.md`. Until they land, the doc surface over-promises relative to the
  honest CLI help.

## Verification (exact commands, shared tree)

- `cargo test -p slipfork-engine --all-targets` → **0** (`6 passed; 0 failed`; includes
  `r10_r11_reflink_netns_conformance_cow_isolation_uid_and_parallel_same_port`,
  `multi_mount_repo_rw_and_readonly_artifact_volume_are_real_bind_mounts`,
  `freeze_lineage_seals_once_and_sibling_clones_stay_isolated`,
  `redacted_exec_env_and_opaque_world_handles`,
  `r12_incus_engine_protocol_harness_matches_conformance_contract`,
  `r10_tmpfs_fails_closed_without_silent_byte_copy_fallback`).
- `cargo test -p slipfork-engine --test critical_engine` → **0** (`6 passed`).
- `cargo test -p slipfork-engine --test critical_engine -- --nocapture mount` → **0**.
- `cargo test -p slipfork-engine --test critical_engine -- --nocapture freeze` → **0**.
- `cargo test -p slipfork-engine --test critical_engine -- --nocapture redact` → **0**.
- `! rg -q 'EngineCapabilities' crates/slipfork-engine/src` → **0**.
- `cargo test -p slipfork --all-targets` → **0** (`6 passed`, including
  `missing_snow_file_names_the_unreadable_path`).
- `cargo fmt --all -- --check` → **0**; `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` → **0**.
- `cargo clippy --workspace --all-targets -- -D warnings` → **0**; `cargo check --workspace --all-targets` → **0**
  (only pre-existing cargo `unused_dependencies` manifest notes from the M2.2 scaffolding).
- `make check` → **0** (`plan.py validate`, `guard.py` 250 files, `ruff`, `pytest`, `bun test`,
  `cargo fmt`, clippy, rustdoc, `cargo test --workspace --all-targets`, `--doc`,
  `zensical build --strict`, `check_docs_honesty: OK (8 published doc(s))`).
- `uv run --no-project tools/guard.py` → **0** (`guard: OK (250 file(s) checked)`).
- `uv run --no-project tools/critical_guard.py --base 6e6a276 --node M2.6` → **non-zero** for
  *other* nodes' in-flight files (`crates/slipfork-dag/tests/{critical_dag,critical_support}.rs` —
  M2.5, `crates/slipfork-snow/tests/critical_snow.rs` — M2.4,
  `crates/slipfork-world/tests/critical_world.rs` — M2.3). This node's file is authorized:
  `M crates/slipfork-engine/tests/critical_engine.rs  [authorized by crates/slipfork-engine/**]`.
- CLI behaviour spot checks (real binary): `--help` prints “Daemon-owned: … (`M2.17`)” for
  `fork`/`exec`/`drop`; `--json exec|drop` print
  `{"error":"slipfork … is served by slipforkd in 0.2.0 …","ok":false}` on stdout and exit `2`;
  `--json fork --workspace /nope` exits `1` with `{"ok":false,"error":"--workspace '/nope' is not an
  existing directory"}`; `fork exp --from a/b` exits `1` with
  `slipfork: --from: invalid snapshot id 'a/b' …`.

## Notes

- No cross-process world-handle API exists anywhere in the tree (`rg` for `open_world`,
  `from_persisted`, or a `WorldHandle` constructor finds none).
- `expose_to_child_process` is crate-private and proven end-to-end by the redaction critical test,
  which now executes a real world command and asserts the child receives `base ⊕ overlay`
  (overlay secret wins, overridden base secret never reaches the child).
- `.worktrees/m2.6-engine` and the temporary patch were removed after the shared tree went green.
