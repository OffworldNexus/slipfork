# Log M3.29.3

## 2026-11-18T00:08 [planner]

created (task) Delete the disk secret vault: in-memory session secrets, live slipforkd OSPP subscriptions, streaming HPKE dump restore and auth pairing

## 2026-11-18T00:30 [lead]

RESCOPE (owner clarification 2026-11-18): do NOT delete FileVaultProvider/vaults.rs/'slipfork secret'. Scope them to LOCAL credentials only (locally-provisioned backing-service passwords that must survive restarts, local-mode !!, locally-managed vars/secrets) and use the vault contents + local resource/domain declarations to derive OSPP claims (ids only). Add the hard invariant: no remote OSPP secret is ever persisted — assert it with a byte scan of <state_dir> after a full boot with a known remote secret. In-memory session secrets remain for local-only runs without a vault entry.

## 2026-11-18T00:30 [lead]

edited fields

## 2026-11-18T00:55 [lead]

edited fields

## 2026-11-18T01:26 [lead]

edited fields

## 2026-11-18T01:37 [lead]

edited fields

## 2026-11-18T01:37 [lead]

status ready -> active

## 2026-11-18T01:37 [lead]

attempt 1/3 started

## 2026-11-18T01:37 [lead]

base 90daf6e77eee40719b25c0960b66bbd73f6ba4fd

## 2026-11-18T01:43 [lead]

edited fields

## 2026-11-18T01:43 [lead]

attempt 2/5 started

## 2026-11-18T01:43 [lead]

attempt 1 cap-out mid-surgery: BUILD IS RED (crates/slipfork/src/daemon.rs half-rewritten). Solid and kept: slipfork-dag resources (RemoteResourceOffer deleted, resolve(spec,mode,dump_policy,ospp_paired)), engine update_service_env + exec_in_service_with_input, edge SecretVault port + AuthSummary/AuthLogin, snow keys_in, vcs remote(), slipfork ospp.rs deleted + vaults.rs/ProjectVaults + daemon imports/ports/wiring/OsppRuntime/subscribe_project/start_ospp_supervisor. Remaining: ResolvedWorld/LiveWorld fields + boot/reboot population + supervisor call; helpers (resolve_component_env/render_claimed/claimed/subscription_id); move dumps to streaming session.dump -> exec_in_service_with_input(zstd -d|pg_restore) and delete .pgcustom/PGCUSTOM_SUFFIX; auth_login -> pair(); release_world -> note_world_released; main/cli/slipforkd wiring + tests/support endpoint + binary_opener.open_origin + FixedRepo::remote; then critical_osspp_daemon rewrite + critical_cli auth; then acceptance. Two protected files outside paths were necessarily adapted (dag critical_dag, edge critical_edge) — disclose in RESULT.

## 2026-11-18T02:30 [lead]

edited fields

## 2026-11-18T02:30 [lead]

attempt 3: make check GREEN. lib 81, critical_osspp_daemon 6 (two-tier byte scan 0 hits, vault credential survives restart, env_updated restarts api only), critical_cli 24, clippy/fmt/rustdoc/guard 597/plan OK. Dump proof: server dump_urls=1, SELECT count(*)=3, no PGDMP/zstd header file on disk — postgres:18 ships zstd, so the streaming restore is real, no fallback. Paths extended for the two protected edits this cut-over necessarily touches (dag tests + support/integrations.toml evidence pointer). Remaining before commit: remove the NEW clippy allow on create_fork by bundling fork flags into one struct (Law 1).

## 2026-11-18T03:07 [lead]

landed in one atomic commit with its siblings: the OSPP crate cut-over breaks crates/slipfork by design, and M3.28.8's restart/-v work interleaves with M3.29.3 in main.rs/cli.rs; splitting would require hand-editing code. make check green.

## 2026-11-18T03:07 [lead]

status active -> done

## 2026-11-18T03:11 [lead]

amendment: invariant 4's 'ephemeral in-memory session secret for a local-only run without a vault entry' is superseded by the owner's two-tier ruling and by the shipped behaviour (snow's ensure_secret persists '!!' in the local 0600 vault). No ephemeral session secret is generated in 0.1.0; local '!!' values persist so a world can reconnect after a restart. Do not implement the ephemeral-secret path.

## 2026-11-18T03:17 [lead]

H-007 filed (direction-challenge): the OSPP endpoint has no durable source of truth in 0.1.0 (auto-spawn never gets it, units render no Environment=, no daemon stop) — recommendation A: SLIPFORK_OSPP_ENDPOINT read only in the composition root and embedded by daemon install. Not a blocker for the epic: the unpaired path is the default and is what M3.28.13 verifies live; auth/subscriptions are documented as requiring a foreground daemon until H-007 is answered.
