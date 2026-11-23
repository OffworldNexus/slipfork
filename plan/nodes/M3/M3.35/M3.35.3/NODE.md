---
id: M3.35.3
title: "Consolidate Rust & OpenCode integration suites into crisp user-story E2E binaries"
kind: task
status: superseded
owner: implementer
priority: 3
depends_on: [M3.35.2]
paths:
  - "crates/slipfork/tests/**"
  - "crates/slipfork-engine/tests/**"
  - "crates/slipfork-world/tests/**"
  - "crates/slipfork-edge/tests/**"
  - "crates/slipfork-vcs/tests/**"
  - "crates/slipfork-forkfile/tests/**"
  - "crates/slipfork-ospp/tests/**"
  - "crates/slipfork-lsp/tests/**"
  - "crates/slipfork-dag/tests/**"
  - "crates/slipfork-testkit/**"
  - "crates/slipfork-ospp-testkit/**"
  - "packages/opencode-slipfork/tests/**"
  - Makefile
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-20T16:14"
updated: "2026-11-21T00:43"
---

# Goal

Collapse today's 33 integration files / 38 test binaries (`20,553` LOC, `critical_*` prefix ceremony, 5 separate Btrfs world boots) into 6 cohesive zero-mock user-story E2E binaries that assert real end-to-end state and a fail-closed boundary for every `TESTING_PLAN.md` §2–§6 story.

# Context

Measured inventory to merge: `crates/slipfork/tests/{browser,critical_btrfs_live,critical_cli,critical_l0_wiring,critical_ospp_daemon,critical_slug,critical_vcs_remote,critical_world_journey,daemon_unit_install,interact_lab,migrate}.rs` + `tests/support/{mod,install_tool}.rs` (8,390 LOC); `crates/slipfork-engine/tests/{critical_btrfs_live,critical_engine,critical_oci_runtime,critical_pod_bundle,critical_pod_runtime,critical_rootfs_contract,critical_rootfs_live}.rs` + `rootfs_conformance/mod.rs` + `support/{mod,rootfs}.rs` (5,248); `crates/slipfork-edge/tests/{critical_edge,critical_edge_live}.rs` (1,918); `crates/slipfork-vcs/tests/{critical_vcs,critical_conformance_matrix}.rs` (1,749); `crates/slipfork-dag/tests/critical_dag.rs` (785); `crates/slipfork-world/tests/critical_world.rs` (532); `crates/slipfork-ospp/tests/critical_ospp.rs` (462 — §7 keeper from M3.35.2, do not merge); `crates/slipfork-lsp/tests/` (§7 keeper).

Target set (shared helpers in `crates/slipfork-testkit` / `crates/slipfork-engine/tests/support/`, never duplicated per file):

1. `crates/slipfork/tests/e2e_cli.rs` — §3/§4: `doctor` 3-row readiness + remedies, `daemon install` socket-activated `:80` (port-less URLs) + ephemeral fallback, `migrate` from `snow.yml`, `up/restart/down`, bare `status` tree, `ps`, `--json` parity, `NO_COLOR`/non-TTY purity, `logs -f` surviving restart, `shell`/`exec`/`env`, piped+interactive `db`/`redis`, hidden-prompt `secret set/unset/list`, `forward`, `hook`, `open`, `daemon_unit_install` refusal paths.
2. `crates/slipfork/tests/e2e_world_pods.rs` — §2: polyglot `Forkfile` world boot (HMR bind mount + CoW `volume` submount), `.localhost` path/subdomain/WebSocket routes, `postgres` 18 / `redis` 7 / S3 resources, `features`/`env` groups/`secret(...) ?? gen` never on disk/logs/argv, setup+deploy hooks preserving failed worlds with log tails, `slipfork run`, plus the surviving `critical_world.rs` merkle/artifact assertions.
3. `crates/slipfork/tests/e2e_fork_agents.rs` — §5: CoW `fork` millisecond latency + full PG/Redis/volume divergence proof, `drop` dirty-worktree refusal (`--force`), `down --all`, `prune` reaping containers/process groups with zero leaked mounts/subvolumes; folds `critical_vcs_remote.rs` + `critical_conformance_matrix.rs` worktree stories.
4. `crates/slipfork/tests/e2e_ospp_edge.rs` — §6: `auth <url>` PoW + P-256 PoP, Git-remote-matched WS subscribe, `hpke-p256` secret decrypt + live `env_updated` restart, encrypted seed dumps into reusable `base` (`--no-dump`/`--refresh-dump`), fail-closed `.localhost`/control-socket origin, DNS-rebinding and UID refusals (re-home the deleted `daemon/tests/refusals.rs` assertions here); folds `slipfork-edge` ingress + `critical_l0_wiring`/`critical_slug`/`critical_edge_live` into ONE shared world boot.
5. `crates/slipfork-engine/tests/e2e_world_engine.rs` — §2 substrate: Btrfs subvolume snapshot/fork + `SIOC*` CoW round-trip, OCI rootfs build/cache/pin/contract/dedupe, pod bundle+runtime, supervisor signal/reaping (`-pgid`), all sharing ONE world boot (today 7 files each boot their own).
6. `packages/opencode-slipfork/tests/e2e.test.ts` — §5 plugin: `ctx.worktree.transform` provisioning, `ctx.shell.hook` cwd routing, `ctx.session.hook` cleanup; merge and delete `cli-contract.critical.test.ts`, `coordinator.critical.test.ts`, `e2e.critical.test.ts`.

Rules: zero `Mock*`/`Fake*`/`Stub*`; every assertion checks real HTTP/DB/Btrfs/socket/process state (no exit-code-only or substring-only oracles); `require_btrfs()` fails closed in one line naming the substrate (never silent skip); `--test-threads=1` for world-booting suites. Rewrite `Makefile` `RS_LIVE_SUITES`/`rs-test`/`rs-test-live` to enumerate the new `e2e_*` targets explicitly.

# Acceptance

- `cargo test -p slipfork --tests --no-run && cargo test -p slipfork-engine --tests --no-run`
- `uv run --no-project tools/do_vm.py sync && uv run --no-project tools/do_vm.py test` on `165.232.86.174` exits `0` (all `e2e_*` live journeys green)
- `bun test packages/opencode-slipfork/tests`
- `ls crates/slipfork/tests/*.rs crates/slipfork-engine/tests/*.rs` shows only the 5 new `e2e_*` files (+ shared `support/`)
- `cat crates/slipfork/tests/*.rs crates/slipfork-engine/tests/*.rs | wc -l` <= 9000 and `! grep -rn 'critical_' Makefile`
- `cargo clippy --workspace --all-targets -- -D warnings`
