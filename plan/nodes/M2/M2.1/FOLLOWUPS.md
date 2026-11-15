# M2.1 follow-ups (applied in M2.1 H-010 ratification pass)

Round-2 Quad Review Gate verdicts: `reviewer` (functional), `review-legality`, `review-ioc`, `review-cruft`.
Zero BLOCKERs; `H-010` answered and closed in `plan/HUMAN.md`. Every item below has been folded into `ADR-0001..0008`, `ARCHITECTURE.md`, `docs/design/target-dx.md`, and the `M2.*` node specs.

## MAJOR (applied)

- [x] 1. **Listener roles** (`ARCHITECTURE.md` §3.4, `ADR-0008` §5): explicit descriptor-count/fd-order assignment (`LISTEN_FDS == 1` -> HTTP ingress + `forward_listener: None`; `LISTEN_FDS >= 2` -> `UnitSpec` fd order; `--listen <addr>` binds HTTP only unless `--forward-port <port>` is also passed, failing `share` closed with `EdgeError::NoForwardListener`).
- [x] 2. **`DumpPolicy` mutual exclusion** (`ADR-0008` §3, `ARCHITECTURE.md` §3.2): deleted precedence ladder; `--no-dump | --refresh-dump` are mutually exclusive and `ResourceStrategy` decides under `Default`.
- [x] 3. **Single world-selector syntax** (`docs/design/target-dx.md`, `ADR-0005`, `ADR-0008`): `--world <fork>` everywhere, zero positional `[<fork>]` aliases on `shell`/`logs`/`down`/`share`.
- [x] 4. **`EngineCapabilities` boolean bag deleted** (`ADR-0002`, `ADR-0008` §7, `ARCHITECTURE.md` §3.3): unsupported `SnapshotMode::StatefulRam` fails closed with `EngineError::StatefulRamUnsupported`.
- [x] 5. **Generic `FallbackDeviceKeySigner<P, F>`** (`ADR-0008` §1, `ARCHITECTURE.md` §3.2).
- [x] 6. **Single `ResourcePlan::resolve(...)` owner & `OsppClient::resource_offer()` mapper** (`ADR-0008` §3, `ARCHITECTURE.md` §3.2).
- [x] 7. **Single `effective_components` normative home** (`ADR-0001` §1; referenced from `ADR-0008` §4 and `ARCHITECTURE.md` §3.1).

## MINOR (applied)

- [x] `SeedDumpStore::write_from(&self, sha, src: &mut dyn Read)`.
- [x] `ResourceMode` derives `Serialize` only (never `Deserialize`, derived from `WorldTarget`).
- [x] Defined `SoftwareP256Key`; deleted dangling `ListenerRoles`, `ScratchDir`, `OsppSession`.
- [x] Named `InMemoryTunnelRegistrar` test double for `shared_hosts`/`308`/`421` conformance.
- [x] Single `slipfork list --json` home in `docs/design/target-dx.md` §1.5.
- [x] Unified `slipfork share [--stop] [--world <fork>]` and canonical `WorldRegistry` / `DaemonControl` 1:1 spelling (`ADR-0005` §2).
- [x] Dropped retired 64-char `LOG.md` citation from `ADR-0008:3`; kept 31-byte derivation once in `ADR-0008` §6.
- [x] Fixed `target-dx` (`down` does not wait for readiness; `daemon unit --only socket|service`).
- [x] `ADR-0001:23` attributes `OsppBundle` code deletion strictly to `M2.4`.
- [x] Explicit `--state-dir` / `--runtime-dir` (`<runtime_dir>/daemon.sock`), explicit `User=<developer-user>` via `UserIdentity` (`getpwuid_r`), typed `cwd: PathBuf` in `DaemonControl` requests, daemon-owned `DaemonControl::forward`, and `net.ipv4.ip_unprivileged_port_start=0` per-netns `:80` bridge (`ReflinkNetnsEngine`).
- [x] **Git D/F refname fix & `ProjectSlugOverride` (`ADR-0005` §3, `ARCHITECTURE.md` §3.5, `target-dx.md`, `M2.11`/`M2.12`/`M2.16`/`M2.17`):** Replaced impossible `refs/heads/<fork>/wip` with `refs/slipfork/<fork>/wip` (`git fetch slipfork '+refs/slipfork/<fork>/wip:refs/slipfork/<fork>/wip'`, pointing to `base` when clean) and added injected `trait ProjectSlugOverride` on `RepoRootResolver` so `crates/slipfork-vcs` stays free of `slipfork-dag`. Published `docs/index.md:56` and `README.md:58` remain queued for `M2.19`/`M2.23`.
