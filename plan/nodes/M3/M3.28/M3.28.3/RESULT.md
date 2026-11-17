# RESULT — M3.28.3 DaemonControl truth (ingress addrs, world parent, lineage, process rows)

- **Status:** `done` (attempt 3; attempt 1 step-capped on unverified WIP, attempt 2 verified the
  behaviour, attempt 3 applied the Quad-gate structure/cleanup list)
- **Node:** `M3.28.3` (`release/0.1.0`, base `f291377`)
- **Owner:** implementer

## Quad-gate findings closed (attempt 3)

1. **(structural, IoC `SPEC_BLIND_SPOT`, option A)** `WorldLineage`, `RootfsLineage`,
   `BaseLineage`, `SeedLineage`, `ServiceImageLineage`, `LineageCache`, `ServiceCache`,
   `ServiceKind`, and `WorldServiceInfo` now live **once** in
   `crates/slipfork-engine/src/lineage.rs` (pure data, serde, no OS imports), re-exported by
   `slipfork-edge` for the wire. Edge's duplicate definitions and the `service_state_wire` adapter
   are deleted; the engine's `ServiceState` derives `Serialize`/`Deserialize` (snake_case) as the
   single model. `WorldServiceSummary { world: String, #[serde(flatten)] info: WorldServiceInfo }`
   keeps the flat wire object with **zero** mapping ladder. `WorldLineage::unknown()` remains the
   interim constructor, documented as "no engine facts recorded yet (`M3.28.4` fills them)";
   `M3.28.4` deletes it.
2. **(structural)** `DaemonControl::shutdown`, `DaemonRequest::Shutdown`, the
   `serve_connection -> bool` return, the `AtomicBool`, and the shutdown test coverage are
   **deleted** — no production caller in `0.1.0`. The control server is back to the plain
   thread-per-connection accept loop. Nothing added in its place.
3. **Dead accessors deleted:** `ListenerSet::http_port()` and `WorldRegistry::http_ingress_port()`
   (tests now read `http_addrs()[0].port()` / build the ingress from the bound listener).
4. **`ARCHITECTURE.md` §3.4 matches the shipped wire:** `WorldSummary.world: String`,
   `parent: Option<String>` (with the single "typed `WorldId` is `0.2.0` target state" note),
   `WorldServiceSummary` as `world` + flattened `WorldServiceInfo`, `WorldSelector.world:
   Option<String>`, `world_id(..) -> String` plus the new `parent_world(..) -> Option<String>`;
   `IngressInfo` documented as private fields + `IngressInfo::new` fail-closed contract +
   `#[serde(try_from)]` re-validation; `WorldLineage` no longer claims `#[derive(Default)]`.
5. **`docs/design/dx-0.1.0-ux.md` §3 no longer restates the schema** — it names only the fields the
   CLI reads (`ingress`, `parent`, `lineage`, `WorldProcessReport`, `WorldServiceSummary`) and
   points at `ARCHITECTURE.md` §3.4 as the single home.
6. **Truth in docs/source:** the `WorldUpSummary::restarted` "sound 1:1 join" claim is gone; the
   "only source of the lineage line" present-tense claim is gone; the public `WorldSummary` doc
   cites `ADR-0005` §3 instead of `plan/nodes/M3/LOG.md`; the production `M3.28.4 adds …` roadmap
   comments now state the `0.1.0` fact (no engine facts recorded yet → `None` renders `-`).
7. **Typed error preserved on the wire:** `DaemonErrorKind::IngressPortInvalid` →
   `ingress_port_invalid`, no longer collapsed into `ingress_missing`; one row added to dx §10's
   closed code table (`addrs`, `port` extras).
8. **Tests:** the interim-lineage assertion now checks the rendered **shape**
   (`rootfs`/`base`/`seed` null, `services: []`) instead of comparing to `WorldLineage::unknown()`;
   the repeated `row(...)` and `IngressInfo::new(vec![..])` fixtures are deduped into
   `service_row(...)` / `ingress_info(...)`; `crates/slipfork/src/daemon.rs` states that
   `WorldState::Stopped` is not produced yet (no daemon path records it); `parent` comes only from
   `registry::parent_world`, the single target→parent owner used by `list()`.

## What shipped (cumulative)

- `IngressInfo { addrs, port }`: port derived from the bound listeners; empty set →
  `EdgeError::IngressMissing`, `:0` / disagreeing ports / forged `port` absent from `addrs` →
  `EdgeError::IngressPortInvalid`. `DaemonStatus { pid, ingress, forward_port, worlds }` — the
  retired scalar `http_ingress_port` is deleted, no alias.
- `WorldSummary { world, parent, state, origin, worktree, lineage }`;
  `WorldProcessReport { state, lineage, services }` from one read, ids unique
  (`WorldProcessReport::new`), unknown world → typed `UnknownWorld` (never an empty report);
  `WorldServiceSummary { world, info }`; `pid`/`port`/`uptime_secs` and every lineage fact stay
  `None`. `DaemonWiring` carries `IngressInfo`, so `slipforkd` reports what it actually bound
  (`ListenerSet::http_addrs`).

## `# Acceptance` (real output, attempt 3)

```console
$ cargo test -p slipfork-edge --lib
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

$ cargo test -p slipfork-edge --test critical_edge
test result: ok. 37 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.54s

$ cargo test -p slipfork --test critical_ospp_daemon -- --test-threads=1
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 126.69s

$ ! grep -rn '/proc' crates/slipfork/src/daemon.rs && echo no-proc
no-proc

$ cargo clippy --workspace --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.35s

$ cargo fmt --all -- --check && echo fmt-ok
fmt-ok
$ uv run --no-project tools/guard.py
guard: OK (548 file(s) checked)
$ uv run --no-project tools/plan.py validate
plan OK
```

`make check` (whole chain): plan OK, guard OK, ruff OK, 126 tools tests passed, support OK, then
`make: bun: No such file or directory` (`ts-test`) — this host has no `bun`; the remaining chain was
run explicitly:

```console
$ make rs-fmt rs-lint rs-test-list
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
$ make rs-test docs-check    # make-exit=0, 44 × "test result: ok", zero FAILED
check_docs_honesty: OK (9 published doc(s) match real CLI subcommands)
```

No sibling-file stop remained: `M3.28.5`'s in-flight `slipfork-vcs` has since compiled; its files
(`crates/slipfork-vcs/**`, `crates/slipfork/src/vcs.rs`, `Makefile`, `tools/rs_test_list.py`,
`crates/slipfork-world/src/lib.rs`) were never touched here.

## `critical_edge` coverage (protected test extended, never weakened)

- `ingress_info_round_trips_port_80_and_ephemeral_binds` — `:80` (`127.0.0.1` + `[::1]`) and a real
  ephemeral bind; order non-semantic; port present in `addrs`; empty/`:0`/mixed/forged-port fail
  closed typed.
- `world_summary_parent_and_lineage_survive_serde` — `parent: null` canonical / `"acme"` fork;
  every lineage key present as `null`/`[]`; `null` stays `null`, measured `0` stays `0`;
  `miss_offline`/`hit`; `DaemonStatus` has `ingress` and no `http_ingress_port`.
- `world_process_reports_are_world_scoped_and_keep_stopped_rows` — two worlds never share a row, a
  stopped service keeps its row with `None` facts, unknown `ServiceState` fails closed, duplicate
  ids refused.
- `daemon_control_reports_world_processes_over_the_socket` — `world_processes` crosses the socket
  with state/lineage/rows; unknown world is a typed `UnknownWorld` error.

## Notes for downstream nodes

- `M3.28.4` fills `pid`/`port`/`uptime_secs` and every `WorldLineage` fact from the engine (the
  daemon already records each row's `ServiceKind` at spawn) and deletes `WorldLineage::unknown()`.
- `WorldUpSummary` still lacks `state`/`changed`/`restarted`/`lineage`/`services`/`next`
  (`M3.28.4`/`M3.28.8` own that shape).
- No global state, no `/proc`/`/sys`/env/clock read in the control path, no new dependency, no
  `unsafe`, no compatibility alias.
