# M2.1 follow-ups (shipped past the Round-2 gate; fold into the H-010 resolution pass)

Round-2 Quad Review Gate verdicts: `reviewer` (functional), `review-legality`, `review-ioc`, `review-cruft`.
Zero BLOCKERs. The known cross-node spec blind spots are escalated as `H-010` (direction-challenge).
The items below are spec-consistency polish that the architect must fold into the same ADR-0008/ARCHITECTURE/
target-DX amendment that applies the `H-010` ruling — they do not invalidate the committed text.

## MAJOR (must be fixed in the H-010 amendment, not later)

1. **Listener roles undefined** (`ARCHITECTURE.md:263`, `ADR-0005:24`): the listener role is inferred from
   `local_addr().port()` (`:80` vs `forward_port`), but the documented unprivileged path
   `systemd-socket-activate -l 127.0.0.1:0` inherits an ephemeral port and matches neither role.
   Replace the port heuristic with explicit role assignment (documented fd order from the unit generator,
   or "single inherited listener = HTTP ingress") and name `ListenerRoles`.
2. **`DumpPolicy` precedence contradiction** (`ADR-0008:38`, `ARCHITECTURE.md:172`): delete the
   `Skip > Refresh > ResourceStrategy` ladder; `--no-dump` / `--refresh-dump` are mutually exclusive and
   otherwise `ResourceStrategy` decides. `target-dx:110` already spells the correct `[--no-dump | --refresh-dump]`.
3. **Dual world-selector syntax** (`docs/design/target-dx.md:106`): `--world <fork>` plus positional
   `[<fork>]` on `shell`/`logs`/`down`/`share`. Pick one spelling (pre-`1.0`, no alias).
4. **`EngineCapabilities` is an undefined boolean bag** (`ARCHITECTURE.md:231`): delete it and let
   `SnapshotMode::StatefulRam` fail closed with a typed engine error.
5. **`FallbackDeviceKeySigner` hardcodes concrete adapters** (`ARCHITECTURE.md:44,87,184`): make it generic
   (`FallbackDeviceKeySigner<P: DeviceKeySigner, F: DeviceKeySigner>`) like `LayeredSecretProvider<P, F>`.
6. **No owner for `ResourceStrategy` + `DumpPolicy` resolution** (`ARCHITECTURE.md:86-87`): add one
   `ResourcePlan::resolve(...)` visible to both `slipfork-dag` and `slipfork-ospp` so `up`,
   `TestCoordinator`, and the OSPP client cannot re-derive it independently.
7. **`effective_components` rule triplicated** (`ADR-0008:42`, `ADR-0001:16`, `ARCHITECTURE.md:121`): keep
   the normative rule in `ADR-0001` §1 only; reference it elsewhere.

## MINOR

- `SeedDumpStore::write_verified(&mut dyn FnMut(&mut dyn Write))` → `write_from(&self, sha, src: &mut dyn Read)`.
- `ResourceMode` should not derive `Deserialize` (it is derived from `WorldTarget`).
- Define or delete the dangling names `ListenerRoles`, `ScratchDir`, `OsppSession`, `SoftwareP256Key`.
- Name the in-memory `TunnelRegistrar` conformance double used for `shared_hosts`/`308`/`421` tests.
- One JSON contract for `slipfork list --json` (`target-dx` §1.5 is the home; point `ADR-0008:44` at it).
- `slipfork share` selector spelling must match `ADR-0005:39`.
- Drop the retired "64-char" number and the `plan/nodes/M2/LOG.md` path from `ADR-0008:3`.
- State the 31-byte charset/`<prefix>-<sha256_hex12>` derivation once (`ADR-0008` §6); reference it elsewhere.
- `target-dx:30` wrongly says `down` waits for component readiness.
- `daemon unit [--socket-only | --service-only]` → one `--only socket|service` value.
- `ADR-0001:23` says `OsppBundle` is deleted in "M2.1/M2.4"; only M2.4 deletes code.
- Stale outside this diff (dies in M2.4/M2.19): `crates/slipfork-snow/src/provider.rs:141` still documents the
  retired REST bundle wire; `docs/reference/manifest.md:120-122` and `docs/snow-modes.md:67` still present
  `OsppBundle` as current.
