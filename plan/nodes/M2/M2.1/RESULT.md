# Result — M2.1 (`STATUS: done`, `H-010` + Frozen-Gate Fixes Applied)

## Summary

Folded all `H-010` rulings, `FOLLOWUPS.md` items, and the 9 frozen-gate + addendum fixes into Slipfork's M2 design of record, removed `NODE-DELTAS.md`, and emitted complete ready-to-splice bodies under `plan/nodes/M2/M2.1/NODE-BODIES-FIX/{M2.7,M2.8,M2.9,M2.10,M2.14,M2.15,M2.16,M2.17}.md`:

1. **Non-template `User=<developer-user>` (`ADR-0005`, `ADR-0008` §5, `ARCHITECTURE.md`, `target-dx.md`, `M2.7`/`M2.10`/`M2.16`):** Kept non-template `slipforkd.{socket,service}` with explicit `UnitSpec.user` resolved once at the CLI composition boundary via `UserIdentity` (`slipfork_engine::current_username` using `getpwuid_r(geteuid())` in `linux_cow.rs`), rejecting empty or `%`-template values; replaced all template `User=` occurrences.
2. **Daemon-owned `forward` (`ADR-0008` §4, `ARCHITECTURE.md` §3.4, `target-dx.md`, `M2.9`/`M2.17`):** `DaemonControl::forward` binds host `127.0.0.1:0` listeners inside `slipforkd` and bridges directly to `WorldEngine::dial_world_tcp` for the lifetime of the open control connection (zero stream-upgrade, zero FD handover, zero `SCM_RIGHTS`).
3. **Canonical `WorldRegistry` & `DaemonControl` spelling (`ADR-0005` §2, `ARCHITECTURE.md` §3.4, `M2.8`/`M2.9`):** `WorldRegistry::{mint_origin, attach_world, release_world, share_world, unshare_world}` mapped 1:1 to `DaemonControl` (`share` -> `share_world`, `unshare` -> `unshare_world`), citing `ADR-0005` §2 solely for the `share_world`/`unshare_world` asymmetry.
4. **Explicit daemon paths, `--listen` rule & typed `cwd` (`ADR-0008` §4–§5, `ARCHITECTURE.md` §3.4, `M2.8`/`M2.9`/`M2.10`/`M2.16`/`M2.17`):** `slipforkd --state-dir <path> --runtime-dir <path>` binds `<runtime_dir>/daemon.sock` (`0600`, never guessing `$XDG_RUNTIME_DIR` or `current_dir()`); `--listen <addr>` binds HTTP only unless `--forward-port <port>` is passed (`EdgeError::NoForwardListener`); `cwd: PathBuf` added to `Up`/`Down`/`Fork`/`Logs`/`HookRun`/`ExecTarget`/`Forward` requests.
5. **Engine-owned `:80` bridge, `OsppClient::resource_offer` & cruft cleanup (`ADR-0002`, `ADR-0008` §3, `ARCHITECTURE.md`, `FOLLOWUPS.md`, `M2.7`/`M2.14`/`M2.15`/`M2.17`):** `ReflinkNetnsEngine` owns the in-netns bridge via `net.ipv4.ip_unprivileged_port_start=0` (probe-verified in `M2.7`); `OsppClient::resource_offer() -> Option<RemoteResourceOffer>` is the sole bundle->offer mapper into `ResourcePlan::resolve`; deleted `EngineCapabilities` from `ADR-0002`; updated `M2.15` to SHA-256 content-hash verification; marked `FOLLOWUPS.md` items applied; deleted `NODE-DELTAS.md`.

## Acceptance Verification

- PASS: `test -f docs/decisions/ADR-0008-ospp-v1-websocket-tpmd-resource-strategy-and-world-ux.md` & `rg -q 'ospp-v1|p256:<66|LISTEN_FDS|SERVICE_NAME_MAX' ...`
- PASS: `! rg -q 'secrets:ensure|/bundle|OsppHttpProvider' docs/decisions/ADR-0001-snow-modes-and-secret-protocol.md`
- PASS: `rg -q 'crates/slipfork-{edge,vcs,ospp}' ARCHITECTURE.md` & `rg -q 'SERVICE_NAME_MAX|31 bytes' ARCHITECTURE.md`
- PASS: `ls docs/design/adr-0008.review-ioc-upward.md docs/design/adr-0008.review-dx.md`
- PASS: `uv run --no-project tools/plan.py validate`, `uv run --no-project tools/check_docs_honesty.py`, `make docs-check`, and `make check`
