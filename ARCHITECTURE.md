# Slipfork Technical Architecture (`ARCHITECTURE.md`)

This document is the authoritative specification of Slipfork's **Composition Root**, **Inversion of Control / Dependency Injection (IoC/DI) Tree**, crate boundaries, wire protocols, and execution invariants (`ADR-0001`..`ADR-0010`, `H-001`..`H-011`).

The single Btrfs world engine (and its injected `SubvolumeDriver` seam, `ADR-0010`), world base-image builder, secret stores, OSPP capability/dump client, VCS workspace drivers, tunnel registrar, and language-blind `STP v1` test orchestrator MUST plug into the trait tree below without global state, ambient OS/env reads, downcasts, opaque extras bags, `match kind` sprawl, or fallback hacks. **All domain traits are synchronous (`std::net`, `UnixStream`, synchronous `tungstenite` over `rustls` + `webpki-roots`, `std::thread`; only `TpmdDeviceKeySigner` encapsulates a private `current_thread` `tokio::runtime::Runtime` internally to drive upstream `tpmd-client`). If a feature cannot fit this DI tree cleanly, stop and file a `direction-challenge` in `plan/HUMAN.md`.**

---

## 1. Composition Root & DI Tree (`H-010` Unified User Daemon & M3 Target-State `ADR-0010` Btrfs Engine)

> **Cargo Feature & Test-Scaffolding Rule (normative):** Cargo features are reserved for language/platform modularity; test scaffolding (such as `TestTreeDriver`, `FakeHostProbe`, `FakeBaseImageBuilder`, `MemorySecretProvider`, and `InMemoryTunnelRegistrar`) lives strictly in `#[cfg(test)]` (crate-internal unit tests), `tests/support/` (per-crate integration suites, e.g. `crates/slipfork/tests/support/` supplying a test `SubvolumeDriver` to `doctor` and in-process `slipforkd`, plus `FakeHostProbe` to `doctor`, in `M3.15`), or an optional `publish = false` workspace testkit crate in `[dev-dependencies]` — never behind a Cargo `[features]` flag on a production crate. (The M2 residue — `crates/slipfork-ospp`'s `test-server` feature enabled from `crates/slipfork/Cargo.toml` — was deleted in `M3.22`: those reference-server doubles now live in the unpublished `crates/slipfork-ospp-testkit` crate (`publish = false`), consumed as a `[dev-dependencies]` entry by both protected test suites.) **Dev-dependency double-compile rule (normative):** a `publish = false` testkit crate that itself depends on the crate under test (`slipfork-engine` →dev→ `slipfork-testkit` →normal→ `slipfork-engine`) makes Cargo compile that crate **twice**, so a testkit double implements a different copy of the crate's traits than its own `--lib` unit tests see — such doubles are consumed from integration suites (`tests/`), which link the normal library and can pass `&TestTreeDriver as &dyn SubvolumeDriver`, while `--lib` unit tests use crate-internal fakes.

Only **two entrypoints** in the entire repository wire concrete adapters from ambient environment variables, wall clocks, OS sockets, or real filesystems:

1. **Rust Unified User Daemon Composition Root (`slipforkd` in `crates/slipfork`, shipped in `0.2.0`; M3 Phase 1 `BtrfsEngine` switchover in `M3.20`, `M3.12`–`M3.15`, `M3.21`; M3 Phase 2 `STP v1` wiring in `M3.2`):** Runs unprivileged as `User=<developer-user>` (`H-010`'s `%i` shorthand denotes the developer's own OS account, resolved once at the CLI composition boundary via `UserIdentity` into non-template `slipforkd.{socket,service}` plus M3 target-state `/etc/apparmor.d/slipfork` `userns` profile via `slipfork daemon unit --only apparmor` [`M3.15`, `ADR-0010` §7], or `--listen 127.0.0.1:0 --state-dir <path> --runtime-dir <path>` auto-spawned by the CLI). `slipforkd` receives explicit `--state-dir` and `--runtime-dir` flags (never defaulting or guessing `$XDG_RUNTIME_DIR` or `current_dir()`; optional composition-root helper overrides `--crun <path>` and `--world-init <path>` inject explicit binary paths into `runtime_assets`, `ADR-0009` §1), binds `<runtime_dir>/daemon.sock` (`0600`) plus `<state_dir>/http-ingress.sock` (`0600`), provisions `runtime_assets` (`ADR-0009` §1), injects each world's disjoint per-world control directory `WorldSpec.control_dir` (`ADR-0010` §2), and owns **all** stateful adapters in-process: shipped `0.2.0` `BaseImageBuilder`, `SecretProvider`, persistent `OsppClient`, `ResourcePlan`, `VcsWorkspaceFactory`, `WorldRegistry`, and `TunnelRegistrar`; M3 Phase 1 target-state `BtrfsEngine` (`M3.13`/`M3.15`) constructed over `Arc<dyn SubvolumeDriver>` = `Arc::new(KernelBtrfsDriver)` (`M3.12`, enforcing §3.3 / `ADR-0010` §4's magic-only fail-closed `SubvolumeDriver::ensure_supported` boot check); and M3 Phase 2 target-state (`M3.2`) `STP v1` orchestrator + `BtrfsArtifactStore` (`crates/slipfork-test`, sharing `Arc<dyn SubvolumeDriver>`). The `slipfork` CLI binary (`crates/slipfork/src/main.rs`) is a **thin client** (`clap` + `UserIdentity` + single composition-boundary `Layout::resolve` [resolving `state_dir` and `runtime_dir` once from CLI flags + `$HOME`/`$XDG_*`, using `$XDG_RUNTIME_DIR/slipfork` or `<state_dir>/run` when `$XDG_RUNTIME_DIR` is unset, before dialing `<runtime_dir>/daemon.sock`, rendering units, or auto-spawning `slipforkd`] + `DaemonControlClient` + single `resolve_world_selector` parser [`ADR-0005` §3, `M3.15`] + direct `Command` world entry inheriting `Stdio::inherit()` with zero `SCM_RIGHTS` FD passing at the CLI<->daemon boundary, compile-time `SupportRegistry::embedded()`, unit/AppArmor profile rendering, and pre-daemon `slipfork doctor` constructing `KernelBtrfsDriver` + `OsHostProbe` in `crates/slipfork/src/main.rs` and passing `(&dyn SubvolumeDriver, &dyn HostProbe)` to `slipfork::doctor::run` [`M3.12`/`M3.15`, `ADR-0010` §4] while holding **zero** `WorldEngine`; `BtrfsEngine` spawns its internal PID 1 helper `slipfork-world-init`, §3.3).
2. **OpenCode V2 Plugin Composition Root:** `packages/opencode-slipfork/src/index.ts` (injecting explicit `SlipforkProjectState` in `M2.18` and routing shell calls via `slipfork exec --cwd <cwd>` without resolving or injecting `snow.yml` env vars itself, `ADR-0004`).

```mermaid
flowchart TD
  CLI["Thin CLI Client (crates/slipfork: slipfork, git-remote-slipfork)<br/>clap + UserIdentity + resolve_world_selector + auto-spawn + DaemonControlClient + direct Stdio::inherit() exec<br/>holds NO WorldEngine, NO SecretProvider, NO TunnelLease"]
  CLI -->|0600 Unix JSON-line IPC<br/>&lt;runtime_dir&gt;/daemon.sock| DC["trait DaemonControl<br/>(crates/slipfork-edge, M2)"]
  CLI -->|compile-time read-only| SR["slipfork_dag::SupportRegistry::embedded()"]
  CLI -->|main.rs constructs KernelBtrfsDriver + OsHostProbe (M3.15, ADR-0010 §4)| DR["doctor::run(&amp;dyn SubvolumeDriver, &amp;dyn HostProbe)<br/>(crates/slipfork::doctor — calls btrfs::probe_with)"]
  DR --> SD
  DR --> HP["trait HostProbe<br/>(crates/slipfork::doctor: probe_userns + probe_netns)"]
  HP --- HP1["OsHostProbe (sole prod adapter: fixed-PATH unshare + AppArmor EPERM hint)"]
  HP --- HP2["FakeHostProbe (test-only; §1)"]

  CR["Unified Daemon Composition Root<br/>(slipforkd in crates/slipfork, User=&lt;developer-user&gt;)"]
  DC --> CR
  CR -->|serves HTTP/WS ingress &amp; forward| Edge["HttpIngress &amp; WorldRegistry<br/>(crates/slipfork-edge: LISTEN_FDS / --listen, ingresses prefixes, 308/421)"]
  CR -->|injects| SnowRes["SnowFile / ResourceCoordinates<br/>(crates/slipfork-snow)"]
  CR -->|injects| DagOrch["DagPlanner, SlipforkConfig &amp; ResourcePlan::resolve<br/>(crates/slipfork-dag)"]
  CR -->|holds persistent session| OsppSvc["OsppClient (resource_offer) &amp; DumpStreamer<br/>(crates/slipfork-ospp, M2)"]
  CR -->|injects| VcsSvc["RepoRootResolver &amp; VcsWorkspaceFactory<br/>(crates/slipfork-vcs, M2)"]
  CR -->|injects Phase 2 M3.2| STPOrch["Language-Blind STP v1 DAG Orchestrator<br/>(crates/slipfork-test, Phase 2 M3.2)"]

  CR -->|parses world.image + world.stage, holds BuildImage| BIB["trait BaseImageBuilder<br/>(crates/slipfork-engine::rootfs)"]
  BIB --- BIB1["DockerfileStageBuilder<br/>(BuildxExporter + OciImageStore + AbiProbe)"]
  BIB --- BIB2["FakeBaseImageBuilder (test-only; §1)"]
  BIB -->|WorldRootfs (§3.3)| WE

  SnowRes --> SP["trait SecretProvider (Send + Sync)"]
  SnowRes --> RG["trait EntropySource (Send + Sync)"]
  SP --- SP1["FileVaultProvider (0600 JSON — sole local vault writer)"]
  SP --- SP2["OsppSecretProvider (ospp-v1 WebSocket, zero fs writes)"]
  SP --- SP3["LayeredSecretProvider&lt;P, F&gt; (reads Primary=FileVault -&gt; Fallback=Ospp; writes FileVault)"]
  SP --- SP4["MemorySecretProvider (test-only; §1)"]

  OsppSvc --> DKS["trait DeviceKeySigner (Send + Sync)"]
  OsppSvc --> KSP["KeyStorePaths &amp; trait DeviceKeyStore"]
  OsppSvc --> SDS["trait SeedDumpStore (DiskSeedDumpStore over non-tmpfs CacheRoot)"]
  DKS --- DKS0["FallbackDeviceKeySigner&lt;P, F&gt; (TpmdDeviceKeySigner [pinned tpmd-client/tpmd-ipc] -&gt; SoftwareP256Signer)"]

  DagOrch --> DC2["trait DigestCache"]
  DagOrch --> IH["trait InputHasher"]

  Edge --> WE["trait WorldEngine<br/>(crates/slipfork-engine — in-process inside slipforkd)"]
  Edge --> TR["trait TunnelRegistrar -&gt; Box&lt;dyn TunnelLease&gt;<br/>(crates/slipfork-world)"]
  WE --- WE1["BtrfsEngine (M3.13 sole prod WorldEngine, ADR-0010: one unified Btrfs subvolume per world + WorldSpec.control_dir + slipfork-world-init PID 1 + crun OCI services + ReadinessProbe)"]
  WE1 -->|constructor DI Arc&lt;dyn SubvolumeDriver&gt;| SD["trait SubvolumeDriver<br/>(crates/slipfork-engine::btrfs, M3.12 — sole CoW &amp; probe seam)"]
  SD --- SD1["KernelBtrfsDriver (sole prod SubvolumeDriver: probe + ensure_supported -&gt; BtrfsRequired)"]
  SD --- SD2["TestTreeDriver (test-only; §1)"]
  TR --- TR1["UnavailableTunnelRegistrar (share_world fails closed until M4.1; deleted when WeavedRegistrar ships)"]
  TR --- TR2["WeavedRegistrar (isolated leaf M4.1; gated on weaved IPC spec per closed H-006)"]

  SnowRes --> WN["crates/slipfork-world<br/>ProjectSlug, WorldTarget, WorldId, LocalHostRoute, WorldOrigin::local(&amp;route, port) &amp; WeaverServiceName (&lt;= 31 bytes)"]
  DagOrch --> WN
  OsppSvc --> WN
  WE --> WN
  Edge --> WN
  VcsSvc --> WN
  STPOrch --> WN

  VcsSvc --> VW["trait VcsWorkspaceFactory -&gt; VcsWorkspace"]
  VW --- VW1["GitWorktreeAdapter (.worktrees/&lt;fork&gt; + refs/slipfork/&lt;fork&gt;/wip)"]
  VW --- VW2["JjWorkspaceAdapter (jj workspace + @ / @- -&gt; refs/slipfork/&lt;fork&gt;/wip)"]

  STPOrch --> DagOrch
  STPOrch --> DC
  STPOrch --> AS["trait ArtifactStore (crates/slipfork-test, Phase 2 M3.2)"]
  AS --- AS1["BtrfsArtifactStore (Phase 2 M3.2; reuses Arc&lt;dyn SubvolumeDriver&gt;)"]
  AS1 --> SD
  STPOrch -->|SLIPFORK_PROTOCOL_FD| STP["STP v1 (ADR-0006 §3)"]
```

---

## 2. Crate & Package Boundaries

| Crate / Package | Milestone | Purity & `unsafe` Policy | Owned Domain Ports (Traits) & Responsibilities |
|---|---|---|---|
| `crates/slipfork-world` | M1 (shipped) + M2 `WorldOrigin` & `WeaverServiceName` (`M2.3`) + **M3 `WorldId` (`M3.25`, `ADR-0005` §3)** | `#![forbid(unsafe_code)]`; depends only on `serde`, `thiserror` (+ `sha2`, `hex` in `M2.2`); zero OS socket/FD/env/fs types | Validated `ProjectSlug`, `WorldTarget { Canonical, UserFork, TestSlot }`, M3 `WorldId` (`Display` + `FromStr` + `WorldId::parse_selector`, single owner of `<project>` / `<project>:<fork>` grammar, `ADR-0005` §3), `WorldError`, encapsulated `LocalHostRoute`, M2 `WorldOrigin { Local, Shared }` (`WorldOrigin::local(&LocalHostRoute, ingress_port)` + `WorldOrigin::shared(url)`, `H-010` #2), `WeaverServiceName` (`SERVICE_NAME_MAX = 31`, `derive_weaver_service_name`, `ADR-0008` §6), and pure `TunnelRequest` + `TunnelLease` + `TunnelRegistrar` |
| `crates/slipfork-snow` | M1 (shipped) + M2 `Send + Sync` & `effective_components` (`M2.4`) | `#![forbid(unsafe_code)]`; depends on `slipfork-world` (zero `slipfork-dag` dep); zero ambient `env`/`net`/clock (`FileVaultProvider` `0600` is the sole vault filesystem writer; `M2.4` deletes M1 `OsppBundle` & `from_bundle`) | `SnowFile` v2 AST (`modes.all` + `modes.<m>`, `SnowFile::effective_components(mode)` per `ADR-0001` §1, `SnowError::ForbiddenTopLevelKey { key }` in `M2.4`), `Faceless` v2 (`??` vs `!!`), `ResourceCoordinates` (`WorldOrigin` precedence over `domains::`, redacted `Debug`), `SecretValue`, `SecretProvider: Send + Sync` (`&self`), `EntropySource: Send + Sync` (`&self`) |
| `crates/slipfork-dag` | M1 (shipped) + M2 `[resources]` & `fs` gate (`M2.5`) + `WorldImageConfig` (`M2.26`, shipped in `0.2.0`) | `#![forbid(unsafe_code)]`; depends on `slipfork-world` (zero `slipfork-snow`/`slipfork-engine`/`std::env`/clock dep); domain core is filesystem-free (`default = []`, `FsMerkleHasher`/`FsDigestCache` behind `#[cfg(feature = "fs")]`) | Root `SlipforkConfig` (`project`, `artifacts`, `fixtures`, `test: TestSpec`, M2 `resources: ResourcesSpec`, M2 `world: WorldImageConfig { image, stage }` — two optional keys under `#[serde(deny_unknown_fields)]`, `ADR-0009` §1), `FixtureSpec` (`per_world: bool`), `TestSpec` / `CoverageConfig` / `CoverageThreshold` (shipped in `0.1.0`), M2 `ResourceMode { Local, Test }`, `ResourceStrategy { Local, PreferRemote, Remote }`, `DumpPolicy`, `RemoteResourceOffer`, and single `ResourcePlan::resolve(...)` (`ADR-0008` §3), `SupportRegistry` + `IntegrationTier::required_evidence()` (`support/integrations.toml`, `ADR-0007`), `DagPlanner`, `InputHasher`, and `DigestCache` |
| `crates/slipfork-ospp` | M2 (`M2.13`–`M2.14`) | `#![forbid(unsafe_code)]`; depends on `slipfork-world`, `slipfork-snow`, `slipfork-dag`, `tungstenite` + `rustls` + `webpki-roots` (sync), `p256`, `zstd`, pinned git `tpmd-client` + `tpmd-ipc` (`rev = "90cb2db..."`) + private `tokio` (`rt`/`net`/`time` confined to `TpmdDeviceKeySigner`); zero `std::env`, zero `slipfork-engine` dep | Slipfork-owned `OSPP v1` sync WebSocket client (`ospp-v1` subprotocol, 16 KiB text-frame cap, `"ospp-v1" \|\| nonce_s(32) \|\| nonce_c(32) \|\| server_name`), `DeviceKeySigner` (`KeyId` = `p256:<66 hex>`, 64-byte raw `r \|\| s` `DeviceSignature`; generic `FallbackDeviceKeySigner<P, F>`, `TpmdDeviceKeySigner` [`sealed` floor, `ResumeOrCreate` over `KeyStorePaths`], `SoftwareP256Signer` [`DeviceKeyStore` + `EntropySource`]), fragment-only `PairingUrl` + `BrowserOpener`, `OsppSecretProvider` (zero fs writes), `RemoteResourceBundle` + single `OsppClient::resource_offer() -> Option<slipfork_dag::RemoteResourceOffer>` mapper, and bounded-window `zstd` `DumpStreamer` over `SeedDumpStore` (`ADR-0008` §1–§3) |
| `crates/slipfork-engine` | M1 + M2 (`L0` rootfs + `crun`) + **M3 target state (`M3.20`, `M3.12`–`M3.14`, `M3.21`, `M3.2`, `ADR-0010`)** | M3 target state: `unsafe` confined strictly to two audited kernel modules — **`src/btrfs.rs`** (`M3.12` `KernelBtrfsDriver` per `ADR-0010` §§3–5) and **`src/linux_cow.rs`** (`M3.21`: `current_uid_gid`, `signal_process`, and `SCM_RIGHTS` `send_fd`/`recv_fd`), each with a per-call `// SAFETY:` proof (zero `pty.rs`, zero per-file extent-clone helpers, zero test Cargo features per §1); `#![forbid(unsafe_code)]` everywhere else (`src/btrfs_engine.rs`, `src/rootfs/`, `src/bin/slipfork-world-init.rs`) | `WorldEngine` (`M3.20` distinct RO snapshot vs writable clone signatures + `ensure_base` + Phase 2 `M3.2` `prune_snapshots`; zero mode flags, zero capability boolean bags), `SubvolumeDriver` (`M3.12` sole CoW & Btrfs probe seam: `KernelBtrfsDriver` + `BTRFS_MOUNT_HINT` + `BtrfsProbeReport` + `btrfs::probe_with(&dyn SubvolumeDriver, ...)` in prod; test-only `TestTreeDriver` per §1), `WorldRef` + `EngineError` + `userns_remediation_hint` (§3.3), `BaseKey` (`derive` + `snapshot_id`) + `BaseSeedSpec` + `WorldSpec` (`world` + `control_dir` + `state_subdirs`), `ServiceSpec` / `ReadinessProbe` / `ServiceRuntime` (`ADR-0002` §2), opaque `WorldHandle`, `#![forbid(unsafe_code)]` `src/bin/slipfork-world-init.rs` (see §3.3 & `ADR-0010` §6), `src/rootfs/` (`WorldRootfs` with `crun_bin = /.slipfork/bin/crun`, `WorldRootfsRequest`, `OciRootfs` [carrying `readiness: ReadinessProbe` in `M3.13`/`M3.15`], `trait BaseImageBuilder` [`materialize` + Phase 2 `M3.2` `prune_cache(&CacheKeepSet, dry_run)` -> `CachePruneReport`] + prod `DockerfileStageBuilder` stopping at plain `cache/{rootfs,oci}` trees, 6-rule World Image Contract — `ADR-0009` §1–§3 is normative; test-only `FakeBaseImageBuilder` per §1), and M3 sole production adapter `BtrfsEngine` (`M3.13`, `ADR-0002`, `ADR-0010` §1–§2: implements `WorldEngine::ensure_base` — `M3.20` aligns `trait WorldEngine` beforehand, and `M3.21` deletes all previously shipped engine adapters, their daemon/REST protocol, storage pools, and per-file extent-clone helpers after `M3.12`–`M3.15` land) |
| `crates/slipfork-edge` | M2 (`M2.8`–`M2.10`, shipped in `0.2.0`) | `#![forbid(unsafe_code)]` (uses safe `listenfd` and `httparse`); depends on `slipfork-world`, `slipfork-engine`; `std::env` confined strictly to `activation.rs` (`LISTEN_PID` / `LISTEN_FDS`) | Unprivileged `slipforkd` HTTP/WS ingress (`adopt_systemd_listener`, `--listen`, `<state_dir>/http-ingress.sock` [§3.3], non-template `UnitSpec` renderer with `User=<developer-user>` & `ReadWritePaths=`, `httparse` `<= 16 KiB` cap, `ADR-0008` §5): `WorldRegistry` (`mint_origin`, `attach_world`, `release_world`, `share_world`, `unshare_world`), `IngressRoute` -> `dial_world_tcp`, `308`/`421` rules (`ADR-0005` §1), `DaemonControl` (`<runtime_dir>/daemon.sock` `0600`), `UnavailableTunnelRegistrar` + test-only `InMemoryTunnelRegistrar` (§1) + isolated `WeavedRegistrar` (`M4.1`) |
| `crates/slipfork-vcs` | M2 (`M2.11`–`M2.12`) | `#![forbid(unsafe_code)]`; depends only on `slipfork-world`, `serde`, `serde_json`, `thiserror` (zero `slipfork-dag` dep); zero `src/bin/*` targets | `ProjectSlugOverride` (injected `<vcs_root>/slipfork.toml` `project` reader), `RepoRootResolver` (`RepoRoot`), `VcsWorkspace` (`WorkspaceRefs`, `WorkspaceChanges`), `VcsWorkspaceFactory` (`WorkspaceRequest`), `GitWorktreeAdapter` (`.worktrees/<fork>`, `refs/heads/<fork>`, and D/F-safe `refs/slipfork/<fork>/wip` via `.slipfork/git/index-<fork>`), `JjWorkspaceAdapter` (`@-` -> `refs/heads/<fork>`, `@` -> `refs/slipfork/<fork>/wip`), and pure `remote_helper` (`git-remote-slipfork` line protocol over `BufRead`/`Write`) |
| `crates/slipfork-test` | **M3 Phase 2 target state (`M3.2`, `ADR-0006` §3)** | `#![forbid(unsafe_code)]`; depends on `slipfork-world`, `slipfork-dag`, `slipfork-edge`, `slipfork-engine`; injected `EntropySource` + `Arc<dyn SubvolumeDriver>` | Language-blind **`STP v1` DAG orchestrator** over inherited `SLIPFORK_PROTOCOL_FD` (`STP v1` wire framing in `ADR-0006` §3): orchestrates global DAG nodes including persistent setup/fixture stage nodes frozen into RO Btrfs subvolume snapshots (`ADR-0010` §3), protocol-native coverage & mutation testing (`mutants`, zero hardcoded per-language Rust drivers, zero magic runtime guessing), `trait ArtifactStore` (`ensure_artifact` + `prune_artifacts`, §3.6) + `BtrfsArtifactStore` (sealing `[artifacts.*]` RO subvolumes via `Arc<dyn SubvolumeDriver>`), surviving `SLIPFORK_OUTPUT_DIR`, and Btrfs-warm `TestSlot` worlds (`ResourceMode::Test`) |
| `crates/slipfork` | M1..M4 (`M2.16`–`M2.17` shipped; **M3 target state `M3.15` & `M3.2`**) | `#![forbid(unsafe_code)]`; **`slipforkd` is sole stateful Rust Composition Root; `slipfork` is thin client** | Binaries `slipforkd` (`--state-dir` + `--runtime-dir` unified user daemon Composition Root assembling `DaemonService: DaemonControl`, provisioning `runtime_assets` [`ADR-0009` §1], constructing M3 target-state `BtrfsEngine::with_driver(..., Arc::new(KernelBtrfsDriver))` [`M3.15`, failing closed with magic-only `EngineError::BtrfsRequired` at boot before any world directory exists, `ADR-0010` §4–§5], injecting `WorldSpec.world` + `WorldSpec.control_dir` + `state_subdirs` and `BaseSeedSpec` [`ADR-0010` §2], and wiring `DockerfileStageBuilder` -> `WorldRootfs` -> `dyn WorldEngine` with `--host-caches`, `ADR-0009`), `slipfork` (thin `clap` + `UserIdentity` + shared `project::resolve_world_selector` -> `WorldId` [`ADR-0005` §3, `M3.15`: `<project>`, `<project>:<fork>`, or `<fork>`] + `DaemonControlClient` + auto-spawner + M3 `daemon unit --only socket\|service\|apparmor` renderer [`M3.15`] + M3 `doctor::run(&dyn SubvolumeDriver, &dyn HostProbe, ...)` wired in `main.rs` over `KernelBtrfsDriver` + `OsHostProbe` [`M3.15`, `ADR-0010` §4] + direct `WorldEntryCommand` `Stdio::inherit()` executor; `HOME`-unset fail-closed guard), and `git-remote-slipfork` |
| `packages/opencode-slipfork` | M1 (shipped) + M2 e2e (`M2.18`) | Type-only `@opencode/plugin`; injected `SlipforkProjectState` + `SlipforkGateway` (zero `existsSync` inside gateway in M2) | OpenCode V2 `worktree.transform`, `shell.hook("create.before")` (routes to `slipfork exec --cwd <cwd>`, never injects `snow.yml` envs), and `session.hook("context")` |

---

## 3. Core Trait Contracts & Wire Protocols

### 3.1 `SecretProvider`, `ResourceCoordinates` & Mode-Scoped Components (`crates/slipfork-snow`, `ADR-0001`, `ADR-0005`, `ADR-0008`)

```rust
// Shipped in 0.1.0 with `&mut self`; promoted to `Send + Sync` + `&self` in M2.4:
pub trait SecretProvider: Send + Sync {
    fn get(&self, namespace: Namespace, key: &str) -> Result<Option<String>, ProviderError>;
    /// Persist `proposed` under `secrets::<key>` iff absent (`put_if_absent` / CAS).
    fn ensure_secret(&self, key: &str, proposed: &str) -> Result<String, ProviderError>;
}

pub trait EntropySource: Send + Sync {
    fn fill_bytes(&self, buf: &mut [u8]) -> Result<(), ProviderError>;
}

pub struct LayeredSecretProvider<Primary, Fallback> {
    primary: Primary,   // e.g. FileVaultProvider (0600): read first, sole write sink for !!
    fallback: Fallback, // e.g. OsppSecretProvider: read second, never written in local mode
}
```

- **Single `0600` Vault Owner & `OsppBundle` Deletion (`M2.4`):** `FileVaultProvider` (`flock` + `O_EXCL` temp + atomic `rename`, `0600`) is the **only** secret-vault filesystem writer; `M2.4` deletes M1's `OsppBundle` + `MemorySecretProvider::from_bundle` from `slipfork-snow`. **OSPP is never a prerequisite for local worlds** (`ADR-0005` §1).
- **Redaction, Forbidden Keys & Mode-Scoped Components (`M2.4`):** Top-level `envs:` and `project:` return `SnowError::ForbiddenTopLevelKey { key }` (`M2.4`, key name only, never values); `SecretValue`, `S3Coordinates`, and `ResourceCoordinates` implement redacting `Debug`; `SnowFile::effective_components(mode)` implements `ADR-0001` §1's single add/remove/override contract.
- **Origin-Aware `ResourceCoordinates`:** `ResourceCoordinates` sets `app_url`/`app_domain` strictly from the world's `WorldOrigin` (`WorldOrigin::local(&route, ingress_port)` or `WorldOrigin::shared(url)`, overriding provider `domains::` in `Local` worlds and failing closed on `${{ domains::<id> }}` in `Shared` worlds) and accepts plain `S3Coordinates` overrides resolved from `ResourcePlan` inside `slipforkd` (zero `slipfork-dag` dependency in `slipfork-snow`).

### 3.2 `OSPP v1` WebSocket, `DeviceKeySigner`, `OsppClient::resource_offer` & `ResourcePlan` (`crates/slipfork-ospp`, `crates/slipfork-dag`, `ADR-0008`)

```rust
// crates/slipfork-dag — [resources] & single ResourcePlan::resolve owner (M2.5, zero slipfork-snow dep)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)] // derived strictly from WorldTarget; no Deserialize
#[serde(rename_all = "snake_case")]
pub enum ResourceMode {
    Local, // WorldTarget::Canonical | UserFork(_) -> defaults_for = ResourceStrategy::PreferRemote
    Test,  // WorldTarget::TestSlot(_) -> defaults_for = ResourceStrategy::Local (hermetic by construction)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceStrategy {
    Local,        // world-local ResourceCoordinates (seaweedfs `crun` container on 127.0.0.1:9000 in the world netns, ADR-0009 §2) / clean initdb
    PreferRemote, // use OSPP S3 / seed dump when paired & offered, else Local with [resources] diagnostic
    Remote,       // require OSPP S3 / seed dump; fail closed if unpaired/offline/absent
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DumpPolicy {
    Default,     // governed by effective ResourceStrategy
    NoDump,      // --no-dump (mutually exclusive with --refresh-dump): clean initdb
    RefreshDump, // --refresh-dump: require live OSPP check; fail closed if offline or Local
}

impl ResourcePlan {
    /// Sole owner of S3 + PG seed resolution consumed by `slipforkd` (`up`/`fork`), `OsppClient`, and M3 `crates/slipfork-test`.
    /// A resolved local target is a `crun` service container (`ADR-0009` §2): seed dumps are read-only
    /// `readonly_mounts` on the `postgres:<v>` `ServiceRuntime::OciContainer` spec and are restored inside
    /// that container (`ADR-0008` §2).
    pub fn resolve(
        spec: &ResourcesSpec,
        mode: ResourceMode,
        dump_policy: DumpPolicy,
        offer: Option<&RemoteResourceOffer>,
    ) -> Result<Self, DagError>;
}
```

```rust
// crates/slipfork-ospp — ospp-v1 sync WebSocket (tungstenite + rustls + webpki-roots), pinned tpmd crates, SeedDumpStore
pub struct KeyStorePaths {
    pub candidates: Vec<PathBuf>, // [$XDG_RUNTIME_DIR/tpmd/tpmd.sock (if set), /run/tpmd/tpmd.sock]
}

pub struct KeyId(String);             // SEC1 compressed 33-byte P-256 pubkey as "p256:<66 lowercase hex>"
pub struct DeviceSignature([u8; 64]); // raw fixed-width r || s (never ASN.1 DER)
pub struct SoftwareP256Key(/* private 32-byte scalar, zeroized on Drop, redacted Debug */);

pub trait DeviceKeyStore: Send + Sync {
    fn load_or_create_p256(&self, entropy: &dyn EntropySource) -> Result<SoftwareP256Key, OsppError>;
}

pub trait DeviceKeySigner: Send + Sync {
    fn key_id(&self) -> &KeyId;
    fn backend_label(&self) -> &'static str; // e.g. "tpmd:sealed", "tpmd:hardware", "software:file-0600"
    fn sign(&self, message: &[u8]) -> Result<DeviceSignature, OsppError>;
}

pub struct FallbackDeviceKeySigner<P: DeviceKeySigner, F: DeviceKeySigner> {
    primary: P,  // TpmdDeviceKeySigner (pinned git tpmd-client + tpmd-ipc, private current_thread tokio Runtime)
    fallback: F, // SoftwareP256Signer (selected ONLY when P reports tpmd absent/unavailable across all candidates)
}

pub trait BrowserOpener: Send + Sync {
    fn open_pairing_url(&self, url: &PairingUrl) -> Result<(), OsppError>;
}

pub trait SeedDumpStore: Send + Sync {
    fn lookup(&self, sha256_compressed: &str) -> Option<PathBuf>;
    fn write_from(
        &self,
        sha256_compressed: &str,
        compressed_bytes: u64,
        src: &mut dyn DumpStream,
    ) -> Result<Box<dyn VerifiedDump>, OsppError>;
    fn discard(&self, sha256_compressed: &str) -> Result<(), OsppError>;
    /// `DiskSeedDumpStore` decompresses via bounded `decompress_bounded` (`MAX_DUMP_WINDOW_LOG = 23`) to `<sha256>.pgcustom`.
    fn materialize(&self, sha256_compressed: &str) -> Result<PathBuf, OsppError>;
}

impl OsppClient {
    /// Sole conversion owner from wire `RemoteResourceBundle` to `slipfork_dag::RemoteResourceOffer`
    /// (redacting secrets, zero resolution logic); `slipforkd` passes the result directly to `ResourcePlan::resolve`.
    pub fn resource_offer(&self, project: &ProjectSlug, mode: &str) -> Result<Option<slipfork_dag::RemoteResourceOffer>, OsppError>;
}
```

### 3.3 `WorldEngine`, `SubvolumeDriver`, `ReadinessProbe`, Persistent `slipfork-world-init` & Disjoint Control/Socket Paths (`crates/slipfork-engine`, M3 target state `M3.20`, `M3.12`–`M3.15`, `M3.21`; `ADR-0002`, `ADR-0008`, `ADR-0009`, `ADR-0010`, `H-010`, `H-011`)

```rust
// crates/slipfork-engine::btrfs (M3 target state: M3.12) — sole CoW & Btrfs probe seam (ADR-0010 §3–§5)
pub const BTRFS_SUPER_MAGIC: i64 = 0x9123_683e;
/// Single mount remediation constant shared by `EngineError::{BtrfsRequired, SubvolumeDestroyDenied}` and `doctor` (`ADR-0010` §4).
pub const BTRFS_MOUNT_HINT: &'static str = "sudo mount -o user_subvol_rm_allowed,noatime /dev/<device> <state_dir>";

/// Single AppArmor/userns remediation formatter (`crates/slipfork-engine`, `#![forbid(unsafe_code)]`, `M3.13` target state, zero `current_exe()` call),
/// shared by `BtrfsEngine` (`EngineError::UsernsDenied`, `world_init_bin` parent, `M3.13`) and `OsHostProbe::probe_userns` (`M3.15`, `ADR-0010` §§4, 7).
pub fn userns_remediation_hint(bin_dir: &Path) -> String;

/// `statfs`/`statvfs` evidence for `path` (or its nearest existing ancestor): the only kernel-specific
/// input to the single `BtrfsProbeReport` producer (`M3.12` lead-ruled refinement).
pub struct ProbeFacts { pub probed_path: PathBuf, pub fstype: i64, pub noatime: bool }

/// The single `BTRFS_SUPER_MAGIC` gate shared by `ensure_supported`, `list_children`, and the test driver
/// (`ADR-0010` §4): returns `EngineError::BtrfsRequired { path, actual_fstype, mount_hint: BTRFS_MOUNT_HINT }`.
pub fn require_btrfs(fstype: i64, path: &Path) -> Result<(), EngineError>;

pub trait SubvolumeDriver: Send + Sync {
    /// Required (`M3.12` lead-ruled refinement): inspect `statfs`/`statvfs` on `path` (or nearest existing
    /// ancestor); a driver supplies facts only, never a report.
    fn probe_facts(&self, path: &Path) -> Result<ProbeFacts, EngineError>;
    /// Provided (`M3.12` lead-ruled refinement): rejects a non-absolute `path`, then builds the one
    /// `BtrfsProbeReport` from `probe_facts`, creating nothing on non-Btrfs and running the scratch-subvolume
    /// `user_subvol_rm_allowed` round-trip (`Option<Result<(), i32>>`) iff Btrfs
    /// (`BtrfsProbeReport` fields specified once in `ADR-0010` §4).
    fn probe(&self, path: &Path) -> Result<BtrfsProbeReport, EngineError> { /* default body */ }
    /// Magic-only `slipforkd` boot gate (`ADR-0010` §4): fails closed with `EngineError::BtrfsRequired`
    /// unless `statfs` reports `BTRFS_SUPER_MAGIC` (does NOT run the scratch round-trip; missing
    /// `user_subvol_rm_allowed` is caught by `probe` in `doctor` or by `destroy_subvolume` ->
    /// `EngineError::SubvolumeDestroyDenied` at teardown).
    fn ensure_supported(&self, path: &Path) -> Result<(), EngineError>;
    fn create_subvolume(&self, parent: &Path, name: &str) -> Result<PathBuf, EngineError>;
    fn snapshot_ro(&self, source_subvol: &Path, dest_parent: &Path, name: &str) -> Result<PathBuf, EngineError>;
    fn snapshot_writable(&self, source_subvol: &Path, dest_parent: &Path, name: &str) -> Result<PathBuf, EngineError>;
    /// Clears read-only flags internally before destroying `<parent>/<name>` (`ADR-0010` §3; zero boolean mode switch);
    /// maps `EPERM` on Btrfs to `EngineError::SubvolumeDestroyDenied { path, errno, mount_hint: BTRFS_MOUNT_HINT }` (`ADR-0010` §4).
    fn destroy_subvolume(&self, parent: &Path, name: &str) -> Result<(), EngineError>;
    fn list_children(&self, parent: &Path) -> Result<Vec<PathBuf>, EngineError>;
}

/// Injected pre-daemon CLI probe helper (`M3.12`/`M3.15`, `ADR-0010` §4): `crates/slipfork/src/main.rs`
/// constructs `KernelBtrfsDriver` at the CLI composition boundary and passes `&dyn SubvolumeDriver` here
/// (holding NO `WorldEngine`), while tests pass a test `SubvolumeDriver` via §1 (`tests/support/` or dev-dep testkit).
pub fn probe_with(driver: &dyn SubvolumeDriver, state_dir: &Path) -> Result<BtrfsProbeReport, EngineError>;

// crates/slipfork::doctor (#![forbid(unsafe_code)], M3.15 target state; ADR-0010 §4, M3.25)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostCheckOutcome {
    Ok,
    Failed { detail: String, hint: Option<String> }, // subprocess spawn/stderr message + optional remediation hint
}

/// `Skipped` exists only on `DoctorReport.netns` (synthesized solely by `doctor::run` when `unshare_userns != Ok`;
/// `HostProbe` methods return two-variant `HostCheckOutcome` so `probe_userns` can never report `Skipped`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetnsCheck {
    Executed(HostCheckOutcome),
    Skipped,
}

/// Single `doctor` report owner (`crates/slipfork::doctor`, M3.15); text & flattened `--json` projection rules in `ADR-0010` §4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorReport {
    pub ok: bool,
    pub uid: u32,
    pub gid: u32,
    pub btrfs: BtrfsProbeReport,       // fields: ADR-0010 §4
    pub filesystem: String,            // mapped once from `btrfs.fstype` (`ADR-0010` §4)
    pub unshare_userns: HostCheckOutcome,
    pub netns: NetnsCheck,
    pub fixes: Vec<String>,            // emission order & `ok` invariant: `ADR-0010` §4
}

/// Sole pre-daemon namespace probe seam (`OsHostProbe` constructed in `crates/slipfork/src/main.rs`,
/// injected into `doctor::run` only — never `slipforkd` boot; `ADR-0010` §4).
pub trait HostProbe: Send + Sync {
    fn probe_userns(&self, uid: u32, gid: u32) -> HostCheckOutcome; // fixed PATH + LC_ALL=C unshare --user; EPERM -> userns_remediation_hint
    fn probe_netns(&self, uid: u32, gid: u32) -> HostCheckOutcome;  // fixed PATH + LC_ALL=C unshare ... --net --ipc --pid -- ip link set lo up
}

/// Pure `doctor` composer (`crates/slipfork::doctor`, M3.15): calls `btrfs::probe_with(driver, state_dir)` +
/// `host.probe_userns` (and `NetnsCheck::Executed(host.probe_netns)` iff `userns == Ok`, else `NetnsCheck::Skipped`) -> `DoctorReport`.
pub fn run(
    driver: &dyn SubvolumeDriver,
    host: &dyn HostProbe,
    state_dir: &Path,
    uid: u32,
    gid: u32,
) -> Result<DoctorReport, EngineError>;

/// Typed world/slot identity on `EngineError` (`crates/slipfork-engine`, `M3.20`/`M3.13` target state — zero stringly `world: String`):
/// `Display` delegates `World(id)` to `WorldId::fmt` (`<project>`, `<project>:<fork>`, or non-selector `<project>#test-<slot>`, `ADR-0005` §3),
/// formats `SeedSlot(sha12)` as `seed-{sha12}` (`ADR-0010` §2), and formats `Handle(h)` as `{h:?}` (foreign handles in `WorldGone`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorldRef {
    World(WorldId),
    SeedSlot(String),
    Handle(WorldHandle),
}

/// Single normative `EngineError` set (`crates/slipfork-engine`, M3 target state `M3.20`, `M3.12`–`M3.13`, `M3.21`).
#[derive(Debug, Error)]
pub enum EngineError {
    BtrfsRequired { path: PathBuf, actual_fstype: i64, mount_hint: &'static str },       // M3.12: `ensure_supported` non-Btrfs `statfs` (`BTRFS_MOUNT_HINT`)
    SubvolumeDestroyDenied { path: PathBuf, errno: i32, mount_hint: &'static str },      // M3.12: `destroy_subvolume` `EPERM` on Btrfs (`BTRFS_MOUNT_HINT`)
    Ioctl { op: &'static str, path: PathBuf, errno: i32 },                               // M3.12: non-`EPERM` `statfs`/`statvfs`/`BTRFS_IOC_*` driver errors (`ENOENT`/`ENOSPC`/`EIO`/`EBUSY`/...)
    UsernsDenied { detail: String, hint: String },                                       // M3.13: `unshare` userns/AppArmor denial (`userns_remediation_hint`)
    ServiceReadinessFailed { world: WorldRef, id: String, last_exit_code: Option<i32>, stderr: String }, // M3.13: `spawn_service` `ReadinessProbe` (`ADR-0002` §2)
    SnapshotNotFound(String),
    InvalidSnapshotId { id: String },
    InvalidWorldSpec { reason: String },
    InvalidEnvName { name: String, reason: String },
    OverlappingMounts { outer: String, inner: String },
    WorldNotFreezable { world: WorldRef },
    WorldGone { world: WorldRef },
    WorldInitFailed { world: WorldRef, reason: String },
    ServiceAlreadyRunning { world: WorldRef, id: String },
    UnknownService { world: WorldRef, id: String },
    ServiceNotRunning { world: WorldRef, id: String },
    DialFailed { world: WorldRef, port: u16, errno: i32 },
    Execution(String),
    Io { path: String, message: String },
}

/// Content-addressed base key (`"slipfork-base:v2\0"` over `project` × `WorldRootfs.digest` × sorted
/// `BackingServiceKey`/`OciRootfs.digest` × sorted `BaseDatabaseSeed` tags; `ADR-0010` §2, `M3.13` target state).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseKey {
    pub project: ProjectSlug,
    pub sha256: String,                // 64 lowercase hex digits
    pub rootfs_digest: RootfsDigest,   // feeds `CacheKeepSet.rootfs_digests` (`M3.2`)
    pub oci_digests: BTreeSet<String>, // feeds `CacheKeepSet.oci_digests` (`M3.2`)
}

impl BaseKey {
    pub fn derive(
        project: &ProjectSlug,
        rootfs: &WorldRootfs,
        databases: &[BaseDatabaseSeed],
    ) -> Self;
    pub fn sha12(&self) -> &str;             // `&self.sha256[..12]`; names the base slots in `ADR-0010` §2
    pub fn snapshot_id(&self) -> SnapshotId; // `<project>-base-<base_key_sha12>` (`ADR-0010` §2) — sole `BaseKey -> SnapshotId` constructor
}

/// Explicit base-seeding carrier injected by `slipforkd` into `WorldEngine::ensure_base` (`M3.20`/`M3.13` target state; never embedded in `WorldSpec`).
pub struct BaseSeedSpec {
    pub control_dir: PathBuf,             // seed control dir created & cleaned up by slipforkd (`ADR-0010` §2)
    pub project: ProjectSlug,
    pub base_key_sha12: String,
    pub rootfs: RootfsDigest,
    pub app_rootfs: PathBuf,              // <state_dir>/cache/rootfs/<l0_merkle_sha256>/app
    pub state_subdirs: Vec<PathBuf>,      // cloned from the single `slipforkd`-authored list passed to `WorldSpec.state_subdirs`
    pub service_specs: Vec<ServiceSpec>,
    pub databases: Vec<BaseDatabaseSeed>,
}

pub struct BaseDatabaseSeed {
    pub database: String,
    pub service: String,
    pub source: BaseSeedSource,
}

pub enum BaseSeedSource {
    Clean,                                  // tag() -> "clean"
    Dump { path: PathBuf, sha256: String }, // verified <sha256>.pgcustom; tag() -> sha256
}

// Production (`M3.13`): `BtrfsEngine::new(...)` delegates to `BtrfsEngine::with_driver(..., Arc::new(KernelBtrfsDriver))`.
// Tests (`M3.12`/`M3.14`/`M3.15`): `BtrfsEngine::with_driver(..., Arc<dyn SubvolumeDriver>)` receives test-only `TestTreeDriver` per §1.
impl BtrfsEngine {
    pub fn new(state_dir: PathBuf, host_uid: u32, host_gid: u32, world_init_bin: PathBuf) -> Result<Self, EngineError>;
    pub fn with_driver(
        state_dir: PathBuf,
        host_uid: u32,
        host_gid: u32,
        world_init_bin: PathBuf,
        driver: Arc<dyn SubvolumeDriver>,
    ) -> Result<Self, EngineError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountMode { ReadOnly, ReadWrite }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldMount {
    pub host_path: PathBuf,
    pub guest_path: PathBuf,
    pub mode: MountMode, // overlapping RW mounts fail closed (EngineError::OverlappingMounts)
}

/// In-netns `$BASE_URL` loopback bridge configuration (`H-011` #4): `slipfork-world-init`
/// binds `127.0.0.1:<port>` inside the world netns and splices accepted streams to `host_ingress_sock`
/// (`<state_dir>/http-ingress.sock`, `0600` pathname `AF_UNIX` served by `slipforkd`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalIngressBridge {
    pub port: u16,
    pub host_ingress_sock: PathBuf,
}

/// Self-contained child environment (`Command::env_clear()`): restricted inherited allowlist
/// (`PATH`, `TERM`, `LANG` + world-scoped `HOME`) plus plain/secret vars (`effective = base ⊕ overlay`).
/// Custom `Debug` redacts every secret value; serialized over IPC ONLY inside `WorldExecTarget` on `0600` `<runtime_dir>/daemon.sock`.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ExecEnv { /* private allowlist + plain/secret entries; custom redacting Debug */ }

// crates/slipfork-engine::rootfs (#![forbid(unsafe_code)], zero slipfork-snow/slipfork-dag dep; ADR-0009 §1–§3)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OciRootfs {
    pub digest: String,            // sha256:<64 lowercase hex>
    pub rootfs: PathBuf,           // <state_dir>/cache/oci/<sha256>/rootfs ($UID:$GID, RO in crun)
    pub config: PathBuf,           // upstream image config.json (ENV/PATH)
    pub readiness: ReadinessProbe, // M3.13/M3.15 target state: passed unchanged to ServiceSpec.readiness (ADR-0002 §2)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldRootfs {
    pub digest: RootfsDigest,                                   // sha256:<64 lowercase hex>; hex-string `Serialize`/`Deserialize` via `hex()`/`from_hex` in `M3.20` (`ADR-0009` §3)
    pub app_rootfs: PathBuf,                                    // <state_dir>/cache/rootfs/<hex>/app
    pub crun_bin: PathBuf,                                      // /.slipfork/bin/crun (static ELF)
    pub service_images: BTreeMap<BackingServiceKey, OciRootfs>, // postgres:<v>, redis:<v>, seaweedfs
    pub metadata: ImageContractMetadata,                        // os_release, elf_abi, exposed_ports
    pub host_caches: Vec<WorldMount>,                           // empty unless --host-caches ABI probe passes
}

/// Single normative keep-set for `BaseImageBuilder::prune_cache` (Phase 2 `M3.2` target state):
/// `DaemonService::prune` in `slipforkd` is the **sole retention owner** across snapshots and caches (`ADR-0010` §3).
/// Each surviving attached live world record, in-flight `BaseSeedSpec` / `M3.2` `WorldPool` lease, and active
/// `DigestCache` lineage root (`M3.2.13`) retains its `BaseKey` inside `DaemonService`; `DaemonService::prune`
/// populates `CacheKeepSet` from every surviving live world's `WorldSpec.rootfs` (`digest` + `service_images[..].digest`)
/// plus every retained `BaseKey` (`rootfs_digest` + `oci_digests`), so `prune_cache` never derives OCI liveness from
/// on-disk rootfs metadata and never collects a live `crun` container's `cache/oci/<sha256>/rootfs`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CacheKeepSet {
    pub rootfs_digests: BTreeSet<RootfsDigest>,
    pub oci_digests: BTreeSet<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CachePruneReport {
    pub rootfs_digests: Vec<RootfsDigest>, // Phase 2 `M3.2` target state: unreferenced `<state_dir>/cache/rootfs/<hex>`
    pub oci_digests: Vec<String>,          // Phase 2 `M3.2` target state: unreferenced `<state_dir>/cache/oci/<sha256>`
}

pub trait BaseImageBuilder: Send + Sync {
    fn materialize(
        &self,
        req: &WorldRootfsRequest,
        on_event: &mut dyn FnMut(RootfsBuildEvent),
    ) -> Result<WorldRootfs, BaseImageError>;
    /// Single normative Phase 2 `M3.2` GC contract for `<state_dir>/cache/{rootfs,oci}` (dry-run previews without deleting):
    /// deletes entries outside `keep` (`CacheKeepSet`), while `<state_dir>/cache/runtime/` (`ADR-0009` §1)
    /// is owned solely by `crates/slipfork::runtime::runtime_assets` (`slipforkd`) and is **never** collected by `prune`
    /// (retained so offline `Dockerfile` rebuilds succeed after `prune --yes`).
    fn prune_cache(&self, keep: &CacheKeepSet, dry_run: bool) -> Result<CachePruneReport, BaseImageError>;
}

pub struct WorldSpec {
    pub world: WorldId,                            // slipfork_world::WorldId (`project` + `target`, `ADR-0005` §3)
    pub control_dir: PathBuf,                      // per-world control dir injected by slipforkd (ADR-0010 §2)
    pub state_subdirs: Vec<PathBuf>,               // M3.13 target state: subvolume-relative `state/{pgdata/<id>,redis/<id>,s3}` authored once by `slipforkd`
    pub rootfs: WorldRootfs,                       // Dockerfile-derived L0 rootfs + crun binary + OCI service images (ADR-0009 §1–§3)
    pub snapshot: SnapshotId,                      // content-addressed origin-independent RO Btrfs snapshot or `BaseKey::snapshot_id()`
    pub mounts: Vec<WorldMount>,                   // repo (RW) + ArtifactVolumes/SeedDump (RO) + M3 run dirs (RW)
    pub base_env: ExecEnv,                         // resolved at world boot from single WorldOrigin
    pub local_ingress: Option<LocalIngressBridge>, // Some when WorldOrigin::Local (H-010 #2, H-011 #4)
}

/// In-process `(engine, seq)` lookup token owned inside `slipforkd` (never `Serialize`/`Deserialize`, H-010 #1; `WorldHandle::mint` is `pub(crate)`):
/// an engine honours any handle whose engine tag matches and whose `seq` is live in its world table (failing closed `EngineError::WorldGone` otherwise);
/// forgery is in-process via concrete `Self: Sized` `mint_world_handle` (authority over live `seq` on matching-tag engines only, zero cross-engine or IPC authority).
#[derive(Clone, PartialEq, Eq)]
pub struct WorldHandle(/* private (engine, seq) lookup token; custom Debug exposes no paths/env/secrets */);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamSource { Stdout, Stderr }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamChunk { pub source: StreamSource, pub bytes: Vec<u8> }

/// Engine-supplied argv prefix (`nsenter --target <init_pid> ...`) allowing the thin `slipfork`
/// CLI to enter a running world directly inheriting native `Stdio::inherit()` (zero CLI<->daemon `SCM_RIGHTS`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldEntryCommand {
    pub program: PathBuf,
    pub args_prefix: Vec<String>,
    pub cwd: PathBuf,
}

/// Engine-owned service readiness gate (`ADR-0002` §2 is normative; target state `M3.13`–`M3.15` & `M3.2`):
/// passed through unchanged from `OciRootfs.readiness` (`ADR-0009` §2) for `OciContainer` services and derived
/// from `snow.yml` `components[].internal_ports` for `WorldProcess` services so `spawn_service` returns a
/// genuinely protocol-usable service (or fails closed with `EngineError::ServiceReadinessFailed` + stderr).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadinessProbe {
    None,
    TcpPort { port: u16, timeout: Duration },
    Exec { port: Option<u16>, argv: Vec<String>, timeout: Duration },
}

/// One world-owned service. `ADR-0002` §2 and `ADR-0009` §2–§3 are normative.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceSpec {
    pub id: String,              // stable id: `api`, `postgres:<v>`, `redis:<v>`, `seaweedfs`
    pub runtime: ServiceRuntime, // never a boolean mode flag, never a second supervisor
    pub env: ExecEnv,            // overlay on `WorldSpec.base_env`
    pub readiness: ReadinessProbe,
}

/// Single supervision payload for BOTH service kinds — one `spawn_service` / `stop_service` / `service_logs` path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceRuntime {
    /// An `L0` app component running on the world rootfs as `$UID:$GID`.
    WorldProcess { argv: Vec<String>, cwd: PathBuf },
    /// A daemonless `crun` backing-service container (`ADR-0009` §2): read-only rootfs from
    /// `WorldRootfs::service_images`, no `network` namespace so it shares the world `127.0.0.1`.
    OciContainer {
        image: BackingServiceKey,            // postgres:<v> | redis:<v> | seaweedfs
        argv: Vec<String>,
        state_mounts: Vec<StateSubdirMount>, // RW subvolume state subdir -> container path (`ADR-0010` §2)
        readonly_mounts: Vec<WorldMount>,    // RO in-world -> container (e.g. materialized `<sha256>.pgcustom`, ADR-0008 §2)
    },
}

/// RW `$UID:$GID` backing-service state mount inside the world's unified Btrfs subvolume (`ADR-0010` §2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateSubdirMount { pub host_subdir: PathBuf, pub container_path: PathBuf }

/// Distinct signatures separate RO snapshot creation (`create_snapshot`, `freeze_world`) from writable
/// world cloning (`clone_world`); no mode flag or capability boolean bag exists (`ADR-0010` §3, `M3.20`).
pub trait WorldEngine: Send + Sync {
    /// Idempotent base-seeding & sealing port (`M3.20`/`M3.13` target state, `ADR-0010` §2; sole production impl
    /// `BtrfsEngine`): sits on `trait WorldEngine` so `DaemonService` holds only `Arc<dyn WorldEngine>` (never concrete
    /// `BtrfsEngine`), while `WorldSpec` carries no seed payload and `clone_world` fails closed `SnapshotNotFound` on missing snapshots.
    fn ensure_base(&self, seed: &BaseSeedSpec) -> Result<(), EngineError>;
    fn create_snapshot(&self, source_dir: &Path, id: &SnapshotId) -> Result<(), EngineError>;
    /// Stops `WorldProcess` services, then `OciContainer` services cleanly (`SIGTERM` -> bounded grace),
    /// then seals the unified world subvolume into an immutable RO snapshot (`ADR-0010` §3).
    fn freeze_world(&self, world: &WorldHandle, id: &SnapshotId) -> Result<(), EngineError>;
    fn clone_world(&self, spec: &WorldSpec) -> Result<WorldHandle, EngineError>;
    /// Provided handle constructor for `clone_world` impls & §1 test doubles (`Self: Sized` excludes `dyn WorldEngine`; minting a live `seq` yields authority on matching-tag engines).
    fn mint_world_handle(&self, seq: u64) -> WorldHandle where Self: Sized { /* provided via pub(crate) WorldHandle::mint */ }
    /// Single supervision surface for `WorldProcess` and `OciContainer` runtimes (`ADR-0002` §2, `ADR-0009` §3).
    fn spawn_service(&self, world: &WorldHandle, spec: &ServiceSpec) -> Result<ServiceHandle, EngineError>;
    fn stop_service(&self, service: &ServiceHandle) -> Result<(), EngineError>;
    fn service_logs(&self, world: &WorldHandle, id: &str) -> Result<ServiceLogs, EngineError>;
    /// Runs `argv` INSIDE a running service's own rootfs — never the app rootfs — e.g. `pg_restore` +
    /// `CHECKPOINT;` inside `postgres:<v>` (`ADR-0008` §2, `ADR-0009` §2); fails closed with
    /// `EngineError::ServiceNotRunning` when the container is not `ServiceState::Running`.
    fn exec_in_service(
        &self,
        service: &ServiceHandle,
        argv: &[String],
        overlay_env: &ExecEnv,
        on_output: &mut dyn FnMut(StreamChunk),
    ) -> Result<ExecOutput, EngineError>;
    /// `crun exec` entry (`WorldEntryCommand`) for interactive service clients: `slipfork db` -> `psql`,
    /// `slipfork redis` -> `redis-cli` (`ADR-0009` §3); fails closed `EngineError::ServiceNotRunning`.
    fn service_entry_command(&self, service: &ServiceHandle) -> Result<WorldEntryCommand, EngineError>;
    fn exec_in_world(
        &self,
        world: &WorldHandle,
        argv: &[String],
        cwd: &Path,
        overlay_env: &ExecEnv,
        on_output: &mut dyn FnMut(StreamChunk),
    ) -> Result<ExecOutput, EngineError>;
    fn entry_command(&self, world: &WorldHandle, cwd: &Path) -> Result<WorldEntryCommand, EngineError>;
    /// Daemon-internal (`slipforkd` HTTP/WS ingress and `DaemonControl::forward`): connects to `<control_dir>/netns.sock`
    /// (`WorldSpec.control_dir`), sends `port: u16`, and receives the in-world `TcpStream` via `linux_cow.rs`'s single audited `SCM_RIGHTS` helper (`H-011` #3).
    fn dial_world_tcp(&self, world: &WorldHandle, port: u16) -> Result<std::net::TcpStream, EngineError>;
    /// Phase 2 `M3.2` target state (`ADR-0010` §3): destroys (or previews when `dry_run`) unreferenced
    /// `<state_dir>/snapshots/*` subvolumes outside `keep` via `SubvolumeDriver::destroy_subvolume`.
    /// `DaemonService::prune` populates `keep` with every retained `BaseKey::snapshot_id()` (from surviving live worlds,
    /// in-flight `BaseSeedSpec` / `M3.2` `WorldPool` leases, and active `DigestCache` lineage roots, `M3.2.13`) plus every
    /// surviving `WorldSpec.snapshot` and `DigestCache`-referenced stage/fixture `SnapshotId`, so retained sealed bases are
    /// protected by typed `SnapshotId` membership while superseded bases are collected.
    fn prune_snapshots(&self, keep: &BTreeSet<SnapshotId>, dry_run: bool) -> Result<Vec<SnapshotId>, EngineError>;
    fn destroy_world(&self, world: WorldHandle) -> Result<(), EngineError>;
}
```

- **Btrfs Substrate, Diagnostic Seam Ownership & Unified Layout (M3 target state `M3.12`–`M3.15`, `M3.21`; **`ADR-0010` §§2–5 is normative**):** `BtrfsEngine` (`crates/slipfork-engine/src/btrfs_engine.rs`) is the sole production `WorldEngine`, constructed via `BtrfsEngine::new` / `BtrfsEngine::with_driver(..., Arc<dyn SubvolumeDriver>)` and implementing `WorldEngine::ensure_base(&BaseSeedSpec)` (`ADR-0010` §2) plus Phase 2 `M3.2` `WorldEngine::prune_snapshots` (`ADR-0010` §3). `crates/slipfork-engine::btrfs` (`SubvolumeDriver::{probe, ensure_supported}` implemented by `KernelBtrfsDriver`, `BTRFS_MOUNT_HINT`, and injected `btrfs::probe_with(&dyn SubvolumeDriver, state_dir)`) is the single owner of Btrfs host probing (`BtrfsProbeReport` & `EngineError::{BtrfsRequired, SubvolumeDestroyDenied, Ioctl}`, `ADR-0010` §4): `slipforkd` boot calls magic-only `ensure_supported`, pre-daemon `slipfork doctor` calls `doctor::run(&dyn SubvolumeDriver, &dyn HostProbe, ...)` -> `DoctorReport` (running `btrfs::probe_with`'s scratch-subvolume `user_subvol_rm_allowed` round-trip iff `btrfs_super_magic == true` alongside `OsHostProbe` in `main.rs`, `ADR-0010` §4), and `destroy_subvolume` maps runtime `BTRFS_IOC_SNAP_DESTROY_V2` `EPERM` to `EngineError::SubvolumeDestroyDenied` when `doctor` was skipped. Unified single-subvolume world layout, disjoint `<runtime_dir>/daemon.sock` vs `<state_dir>/http-ingress.sock` vs `WorldSpec.control_dir` (`ADR-0010` §2), and the ioctl lifecycle follow `ADR-0010` §§2–5; engine-owned `ServiceSpec.readiness: ReadinessProbe` follows `ADR-0002` §2.
- **`WorldRootfs` / `BaseImageBuilder` Ownership & In-World `crun` Supervision (`ADR-0009` §1–§3):** `WorldRootfs`, `WorldRootfsRequest`, `BackingServiceKey`, `OciRootfs` (carrying `readiness: ReadinessProbe` in `M3.13`/`M3.15`, passed through unchanged to `ServiceSpec.readiness`), `ImageContractMetadata`, `CacheKeepSet` + `CachePruneReport` (`M3.2`), and `trait BaseImageBuilder` (`materialize` shipped in `0.2.0`; `prune_cache` added in Phase 2 `M3.2`) live in `#![forbid(unsafe_code)]` `crates/slipfork-engine/src/rootfs/` (zero `slipfork-snow`/`slipfork-dag`/`SubvolumeDriver` dep; `DockerfileStageBuilder` owns `<state_dir>/cache/{rootfs,oci}`). Static `crun` provisioning, staging, and in-world execution follow `ADR-0009` §1 normatively. `ServiceRuntime::OciContainer` specs flow through `spawn_service` / `stop_service` / `service_logs` alongside `WorldProcess` (`crates/slipfork-engine` owns the single `match runtime` over `WorldProcess | OciContainer` — no `ContainerRuntime` trait, no `dockerd`, no second supervisor).
- **Persistent `slipfork-world-init` PID 1, `--ipc` (`CLONE_NEWIPC`), AppArmor `userns` & `<state_dir>/http-ingress.sock` Bridge (`H-011`; **`ADR-0010` §§6–7 is normative**):** Because multithreaded `setns` into an unprivileged userns/netns fails (`EINVAL`/`EPERM`), `BtrfsEngine::clone_world` spawns `#![forbid(unsafe_code)]` `crates/slipfork-engine/src/bin/slipfork-world-init.rs` under `ADR-0010` §6's unprivileged `unshare` namespace set (including normative `--ipc` / `CLONE_NEWIPC`, `M3.13`). `slipfork-world-init` brings `lo` up, writes `0` to `/proc/sys/net/ipv4/ip_unprivileged_port_start`, mounts the read-only `L0` root + subvolume state mounts (`ADR-0010` §2) + `spec.mounts` and `pivot_root`s into it, binds `0600` pathname `AF_UNIX` `<control_dir>/netns.sock` (`spec.control_dir`) to service `dial_world_tcp(port)` via `linux_cow.rs`'s audited `sendmsg`/`recvmsg` `SCM_RIGHTS` pair (`send_fd`/`recv_fd`, `MSG_CMSG_CLOEXEC`, rejecting >1 descriptors without leaking), and — when `WorldSpec.local_ingress` is `Some` — binds `127.0.0.1:<port>` (including `:80`) inside the world netns and splices accepted streams to `slipforkd`'s `0600` `<state_dir>/http-ingress.sock`. Ubuntu 24.04 LTS AppArmor `userns` profile rendering (`slipfork daemon unit --only apparmor`, `M3.15`) follows `ADR-0010` §7. Across the workspace, M3 `unsafe` lives solely in `crates/slipfork-engine/src/{btrfs.rs,linux_cow.rs}` (each call carrying a `// SAFETY:` proof; `H-010`'s "zero `SCM_RIGHTS`" rule applies strictly to the CLI <-> `slipforkd` TTY boundary).

### 3.4 Port-Aware `WorldOrigin::local`, `WorldRegistry` & `DaemonControl` (`crates/slipfork-world`, `crates/slipfork-edge`, `ADR-0005`, `ADR-0008`, `H-010`)

`slipforkd` runs unprivileged (`User=<developer-user>`, empty `CapabilityBoundingSet=`, explicit `--state-dir <path> --runtime-dir <path>`). Alongside its TCP HTTP listener(s), `slipforkd` binds a `0600` pathname `AF_UNIX` HTTP ingress listener at `<state_dir>/http-ingress.sock` (`H-011` #4) serving the exact same HTTP/WS dispatch loop for in-netns `$BASE_URL` bridges. In `activation.rs` (`#![forbid(unsafe_code)]` via `listenfd`), when `LISTEN_PID` or `LISTEN_FDS` is set, `adopt_systemd_listener() -> Result<Option<ListenerSet>, EdgeError>` verifies `LISTEN_PID == getpid()`, calls `set_nonblocking(true)` immediately on every inherited socket, and assigns roles by explicit descriptor count/order (`LISTEN_FDS == 1` -> single HTTP ingress + `forward_listener: None`; `LISTEN_FDS >= 2` -> `UnitSpec`'s emitted order `127.0.0.1:80`, `[::1]:80`, `127.0.0.1:<forward_port>`; any other shape fails closed `EdgeError::ActivationBroken`, `ADR-0008` §5). When both activation env vars are absent, `--listen <addr>` binds the HTTP ingress listener **only**; a forward listener exists in `--listen` mode **only** when `--forward-port <port>` is also passed (binding `127.0.0.1:<port>`), otherwise `share_world` / `DaemonControl::share` fails closed with `EdgeError::NoForwardListener` (and absent both activation and `--listen`, startup fails closed `EdgeError::NoListener`). HTTP/1.1 headers are parsed via `httparse` (`<= 16 KiB` cap, bounded read timeout), routing `.localhost` and `shared_hosts` requests through each world's `IngressRoute { prefix, port }` longest-prefix table (`snow.yml` `ingresses`) and enforcing `308`/`421` on `*.localhost` when `Shared`.

```rust
// crates/slipfork-world — pure identity, single WorldId grammar owner, port-aware single origin, <= 31-byte service label & tunnel port
pub const SERVICE_NAME_MAX: usize = 31; // ADR-0008 §6 is normative for <= 31-byte derivation

/// Single owner of world-id formatting, parsing, and selector resolution (`ADR-0005` §3 is normative for the
/// grammar, uniform ASCII whitespace trimming, `<project>:<project>` disambiguation, and diagnostics-only
/// `<project>#test-<slot>` formatting / `TestSlot` wire exclusion). Implements `fmt::Display`, `std::str::FromStr`,
/// and manual JSON-string `Serialize`/`Deserialize` (never derived struct-object serde: `Serialize` calls
/// `serializer.collect_str(self)` for `Canonical | UserFork` and fails closed `ser::Error::custom` on
/// `WorldTarget::TestSlot(_)`; `Deserialize` delegates to `FromStr`, forming a bijection over `Canonical | UserFork`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldId { /* private project: ProjectSlug, target: WorldTarget */ }

impl WorldId {
    pub fn from_parts(project: ProjectSlug, target: WorldTarget) -> Self;
    pub fn parse_selector(raw: &str, current_project: &ProjectSlug) -> Result<Self, WorldError>;
    pub fn project(&self) -> &ProjectSlug;
    pub fn target(&self) -> &WorldTarget;
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct WeaverServiceName(String);

pub fn derive_weaver_service_name(full_label: &str) -> Result<WeaverServiceName, WorldError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)] // custom fail-closed Deserialize rejecting empty/non-http(s) URLs
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorldOrigin {
    Local { url: String },  // constructed solely via WorldOrigin::local(&LocalHostRoute, ingress_port: u16) (H-010 #2)
    Shared { url: String }, // constructed via WorldOrigin::shared(url) from TunnelLease::public_url()
} // WorldOrigin::local omits :80 and formats :<port> when ingress_port != 80

#[derive(Debug, Clone, Copy)]
pub struct TunnelRequest<'a> {
    pub route: &'a LocalHostRoute, // deterministically yields WeaverServiceName (ADR-0008 §6)
    pub local_port: u16,           // slipforkd's planned static forward_port (distinct from :80, decided pre-boot)
}

pub trait TunnelLease: Send + Sync {
    fn public_url(&self) -> &str;
    fn lease_id(&self) -> &str;
}

pub trait TunnelRegistrar: Send + Sync {
    fn register(&self, request: &TunnelRequest<'_>) -> Result<Box<dyn TunnelLease>, TunnelError>;
}
```

```rust
// crates/slipfork-edge (M2 + M3 WorldId target state) — activation, unit generator, canonical WorldRegistry & <runtime_dir>/daemon.sock RPC
pub struct ListenerSet {
    pub http_listeners: Vec<std::net::TcpListener>,
    pub forward_listener: Option<std::net::TcpListener>, // None unless LISTEN_FDS >= 2 or --forward-port <port>
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngressRoute { pub prefix: String, pub port: u16 }

impl WorldRegistry {
    pub fn mint_origin(&self, route: &LocalHostRoute, ingress_port: u16) -> Result<WorldOrigin, EdgeError>;
    pub fn attach_world(
        &self,
        project: &ProjectSlug,
        target: &WorldTarget,
        snow_domains: Vec<String>,
        world: WorldHandle,
        ingresses: Vec<IngressRoute>,
    ) -> Result<(), EdgeError>;
    pub fn release_world(&self, project: &ProjectSlug, target: &WorldTarget) -> Result<bool, EdgeError>;
    /// Fails closed with `EdgeError::NoForwardListener` when no forward listener is bound or `EdgeError::TunnelUnavailable`.
    pub fn share_world(&self, project: &ProjectSlug, target: &WorldTarget) -> Result<SharedWorldInfo, EdgeError>;
    /// Idempotent success no-op returning `WorldOrigin::Local` when no tunnel lease is active (ADR-0005 §2).
    pub fn unshare_world(&self, project: &ProjectSlug, target: &WorldTarget) -> Result<WorldOrigin, EdgeError>;
}

pub struct UnitSpec {
    pub user: String,                            // explicit non-empty account name (rejects empty or '%'-template)
    pub listen_addrs: Vec<std::net::SocketAddr>, // e.g. 127.0.0.1:80, [::1]:80
    pub forward_port: u16,                       // must not collide with listen_addrs (EdgeError::UnitPortCollision)
    pub executable: PathBuf,
    pub state_dir: PathBuf,                      // passed as --state-dir & emitted into ReadWritePaths=
    pub runtime_dir: PathBuf,                    // passed as --runtime-dir & emitted into ReadWritePaths=
} // render_socket_unit(&UnitSpec) -> String, render_service_unit(&UnitSpec) -> String

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldSelector {
    pub cwd: PathBuf,
    pub world: Option<WorldId>, // M3 target state (`ADR-0005` §3): `None` resolves nearest world from `cwd`
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DumpRequest { Default, NoDump, RefreshDump }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceRequest { Default, Local }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpRequest {
    #[serde(flatten)]
    pub selector: WorldSelector,
    pub dump: DumpRequest,
    pub resources: ResourceRequest,
    pub host_caches: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForkRequest {
    pub cwd: PathBuf,
    pub fork: String,           // new bare `<fork>` (`!= repo.project`; `ADR-0005` §3)
    pub base: Option<String>,
    pub dump: DumpRequest,
    pub resources: ResourceRequest,
    pub host_caches: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogsRequest {
    #[serde(flatten)]
    pub selector: WorldSelector,
    pub target: Option<String>,
    pub follow: bool,
    pub stderr: bool,
    pub lines: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecTargetRequest {
    #[serde(flatten)]
    pub selector: WorldSelector,
    pub service: Option<String>, // `Some` -> `WorldEngine::service_entry_command` (`crun exec`, fails closed `ServiceNotRunning`); `None` -> world `nsenter` (`ADR-0009` §3)
    pub argv: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookRunRequest {
    #[serde(flatten)]
    pub selector: WorldSelector,
    pub hook: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForwardRequest {
    #[serde(flatten)]
    pub selector: WorldSelector,
    pub targets: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PruneReport {
    pub dry_run: bool,
    pub worlds: Vec<WorldId>,             // `Canonical | UserFork` only (ephemeral `TestSlot` pool worlds excluded; `ADR-0005` §3)
    pub snapshots: Vec<SnapshotId>,       // empty in Phase 1; populated via `WorldEngine::prune_snapshots` in Phase 2 `M3.2` (`ADR-0010` §3)
    pub artifacts: Vec<String>,           // empty in Phase 1; populated via `ArtifactStore::prune_artifacts` in Phase 2 `M3.2`
    pub rootfs_digests: Vec<RootfsDigest>, // empty in Phase 1; `CachePruneReport.rootfs_digests` in Phase 2 `M3.2` (renders bare hex in `--json` via `RootfsDigest` serde)
    pub oci_digests: Vec<String>,         // empty in Phase 1; `CachePruneReport.oci_digests` in Phase 2 `M3.2`
    pub reclaim_bytes: u64,               // `0` in Phase 1; reclaimed bytes in Phase 2 `M3.2` (`M3.2.13`)
}

pub struct WorldExecTarget {
    pub world: WorldId,           // `ADR-0005` §3
    pub origin: WorldOrigin,
    pub entry: WorldEntryCommand, // world entry (`nsenter`) or a service-container entry (`crun exec`, ADR-0009 §3)
    pub env: ExecEnv,             // custom Debug redacts secrets; wire format restricted to 0600 <runtime_dir>/daemon.sock
    pub target_argv: Vec<String>,
}

/// Canonical display, `--json`, & wire world identifier (`crates/slipfork-edge/src/registry.rs`, `ADR-0005` §3):
/// delegates to `WorldId::from_parts` — zero second formatter.
pub fn world_id(project: &ProjectSlug, target: &WorldTarget) -> WorldId;

/// Single CLI `--world` selector entry point (`crates/slipfork::project`, M3.15 target state; `ADR-0005` §3):
/// callers resolve `cwd` via `RepoRootResolver` first, then delegate here (`WorldId::parse_selector(selector, &repo.project)`).
pub fn resolve_world_selector(selector: &str, repo: &RepoRoot) -> Result<WorldId, WorldError>;

/// Typed user-local RPC contract over `<runtime_dir>/daemon.sock` (`0600`, bounded 64 KiB JSON-line frames;
/// every cwd-sensitive request carries explicit `cwd: PathBuf` directly or via `WorldSelector`; 1:1 mapping:
/// `share` -> `WorldRegistry::share_world`, `unshare` -> `WorldRegistry::unshare_world`, `up`/`fork` -> `mint_origin` + `attach_world`,
/// `down`/`drop_fork` -> `release_world`, with `drop_fork` refusing `Canonical` daemon-side naming `slipfork down`;
/// `prune` reaps orphaned worlds in Phase 1 and unreferenced snapshots/artifacts/caches in Phase 2 `M3.2` per `ADR-0010` §3).
pub trait DaemonControl: Send + Sync {
    fn status(&self) -> Result<DaemonStatus, EdgeError>;
    fn up(&self, req: &UpRequest, on_event: &mut dyn FnMut(BootEvent)) -> Result<WorldUpSummary, EdgeError>;
    fn down(&self, req: &WorldSelector) -> Result<bool, EdgeError>;
    fn fork(&self, req: &ForkRequest, on_event: &mut dyn FnMut(BootEvent)) -> Result<WorldUpSummary, EdgeError>;
    fn drop_fork(&self, req: &WorldSelector) -> Result<bool, EdgeError>;
    fn list(&self, repo_root: Option<&Path>) -> Result<Vec<WorldSummary>, EdgeError>;
    fn prune(&self, dry_run: bool) -> Result<PruneReport, EdgeError>;
    fn share(&self, req: &WorldSelector) -> Result<SharedWorldInfo, EdgeError>;
    fn unshare(&self, req: &WorldSelector) -> Result<WorldOrigin, EdgeError>;
    fn secret_set(&self, repo_root: &Path, key: &str, value: &str, force: bool) -> Result<(), EdgeError>;
    fn secret_unset(&self, repo_root: &Path, key: &str) -> Result<bool, EdgeError>;
    fn secret_list(&self, repo_root: &Path) -> Result<Vec<String>, EdgeError>;
    fn logs(&self, req: &LogsRequest, on_chunk: &mut dyn FnMut(StreamChunk)) -> Result<(), EdgeError>;
    fn exec_target(&self, req: &ExecTargetRequest) -> Result<WorldExecTarget, EdgeError>;
    fn hook_run(&self, req: &HookRunRequest, on_chunk: &mut dyn FnMut(StreamChunk)) -> Result<i32, EdgeError>;
    /// `slipforkd` binds host `127.0.0.1:0` listeners in-process and bridges each to `dial_world_tcp` for the
    /// lifetime of this control connection (client holds connection open; disconnect/Ctrl-C tears bridges down).
    fn forward(&self, req: &ForwardRequest, wait_disconnect: &mut dyn FnMut(ForwardSummary)) -> Result<(), EdgeError>;
    fn auth_login(&self, pairing_url: &str) -> Result<AuthSummary, EdgeError>;
}
```

### 3.5 `ProjectSlugOverride`, `RepoRootResolver`, `VcsWorkspace` (`Git` + `Jujutsu`) & `git-remote-slipfork` (`crates/slipfork-vcs`, `ADR-0005`, `ADR-0008`)

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRoot {
    pub vcs_root: PathBuf,
    pub worktree_root: PathBuf,
    pub project: ProjectSlug,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRequest {
    pub repo: RepoRoot,
    pub fork: String,
    pub base: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRefs {
    pub base: String, // refs/heads/<fork> in Git; @- projected as refs/heads/<fork> in Jujutsu
    pub wip: String,  // refs/slipfork/<fork>/wip (equals base commit OID when clean; dirty snapshot via .slipfork/git/index-<fork> or @)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceChanges {
    pub tracked: Vec<PathBuf>,
    pub untracked: Vec<PathBuf>,
    pub truncated: bool,
}

/// Injected into `RepoRootResolver` from `slipforkd` (implemented once over `slipfork_dag::SlipforkConfig`)
/// so `crates/slipfork-vcs` honors `<vcs_root>/slipfork.toml` `project` overrides with zero `slipfork-dag` dep.
pub trait ProjectSlugOverride: Send + Sync {
    fn project_override(&self, vcs_root: &Path) -> Result<Option<ProjectSlug>, VcsError>;
}

pub trait RepoRootResolver: Send + Sync {
    fn resolve(&self, start: &Path) -> Result<RepoRoot, VcsError>;
}

pub trait VcsWorkspace: Send + Sync {
    fn fork_name(&self) -> &str;
    fn path(&self) -> &Path;
    fn refs(&self) -> WorkspaceRefs;
    fn changes(&self) -> Result<WorkspaceChanges, VcsError>;
    fn forget(&self) -> Result<(), VcsError>;
}

pub trait VcsWorkspaceFactory: Send + Sync {
    fn create_workspace(&self, request: &WorkspaceRequest) -> Result<Box<dyn VcsWorkspace>, VcsError>;
}
```

- **D/F-Safe Ref Namespace & Single Parser:** Git forbids `refs/heads/<fork>` and `refs/heads/<fork>/wip` from coexisting; keeping the branch at `refs/heads/<fork>` and placing the dirty view at `refs/slipfork/<fork>/wip` (`git fetch slipfork '+refs/slipfork/<fork>/wip:refs/slipfork/<fork>/wip'`, pointing to `base` when clean) avoids all refname collisions. Zero `AdapterKind` enum outside `slipforkd`'s factory, zero mutation of the live `.git/index`, zero second `slipfork.toml` parser, and zero binary targets in `crates/slipfork-vcs`.

### 3.6 World-Owns-The-Stack, `STP v1` Direction, Typed `[test]` Schema & `SLIPFORK_OUTPUT_DIR` (`crates/slipfork-dag` `0.1.0`, `crates/slipfork-test` Phase 2 `M3.2`, `ADR-0003`, `ADR-0006`)

The CoW world — **never** the test runner — migrates PostgreSQL, seeds snapshots under hermetic `ResourceMode::Test` defaults (`ResourcePlan::resolve`, `ADR-0008` §3), assigns `WorldOrigin`, and boots services (verified protocol-ready via `ReadinessProbe`, `ADR-0002` §2) before tests run. `FixtureSpec.per_world: bool` and the typed `[test]` schema **shipped in `0.1.0`** in `crates/slipfork-dag`:

```rust
// crates/slipfork-dag (shipped in 0.1.0)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageReportFormat { Html, Xml, Json, Lcov, Text }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoverageThreshold(u8); // validated 0..=100 via parse_toml (CoverageThreshold::MAX, percent(self) -> u8)

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageConfig {
    pub reports: BTreeSet<CoverageReportFormat>, // defaults to {Text} when coverage_report omitted
    pub fail_under: Option<CoverageThreshold>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TestSpec {
    pub coverage: Option<CoverageConfig>, // coverage = true is the sole activation switch
}

// crates/slipfork-test (Phase 2 M3.2 target state; BtrfsArtifactStore over Arc<dyn SubvolumeDriver>, ADR-0006 §4, ADR-0010 §§3, 5)
pub trait ArtifactStore: Send + Sync {
    fn ensure_artifact(
        &self,
        digest: &str,
        populate: &mut dyn FnMut(&Path) -> Result<(), ArtifactError>,
    ) -> Result<PathBuf, ArtifactError>;
    /// Phase 2 `M3.2` target state (`ADR-0010` §3): destroys (or previews when `dry_run`) unreferenced
    /// `<state_dir>/artifacts/<digest>` RO subvolumes outside `keep` via `SubvolumeDriver::destroy_subvolume`.
    fn prune_artifacts(&self, keep: &BTreeSet<String>, dry_run: bool) -> Result<Vec<String>, ArtifactError>;
}
```

- **`FixtureSpec.per_world` Read & Write Non-Sharing Invariant (`ADR-0003` §1):** `per_world` (`"per-world"` vs `"shared"`) is hashed into every fixture digest, cannot be the `parent` of an origin-independent fixture (`DagError::OriginIndependentChildOfPerWorld`), is **never** reported as `CacheAction::Cached` by `DagPlanner::plan`, and **MUST NEVER be inserted into `DigestCache`** by snapshot materializers or the test orchestrator.
- **Slipfork Testing Protocol (`STP v1`) & Language-Blind Orchestrator (Phase 2 `M3.2` target state; **`ADR-0006` §§3, 5 is normative**):** Over inherited `SLIPFORK_PROTOCOL_FD` (`STP v1` wire framing in `ADR-0006` §3), `crates/slipfork-test` (`M3.2`) orchestrates global DAG nodes — freezing persistent setup/fixture stage nodes into read-only Btrfs subvolume snapshots (`ADR-0010` §3), sealing/pruning `[artifacts.*]` subvolumes via `ArtifactStore` (`BtrfsArtifactStore`), driving protocol-native coverage (`TestSpec.coverage` / `CoverageThreshold`) and mutation testing (`mutants`), mounting surviving slot-scoped `SLIPFORK_OUTPUT_DIR` (`ADR-0006` §5), and following `ADR-0006` §3's zero-guessing, external-proof-plugin, and Phase 2 specification rules.

### 3.7 Integration Support Registry (`support/integrations.toml`, `crates/slipfork-dag`, `ADR-0007`, `H-009`)

`0.1.0` shipped the typed schema, `SupportRegistry::embedded()`, `support/integrations.toml`, and `critical_support.rs`; M2 wires `slipfork support [--json]`, `tools/check_support.py` (`make check` ceiling + `make support-sync`), and generated `docs/support-matrix.md`.

```rust
// crates/slipfork-dag/src/support.rs
#[derive(Debug, Error)]
pub enum RegistryError {
    Toml(String),
    EmptyRegistry,
    DuplicateId(String),
    MissingEvidence { id: String, tier: IntegrationTier, field: EvidenceField },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)] // + fmt::Display
#[serde(rename_all = "kebab-case")]
pub enum IntegrationTier { None, Declared, Exercised, FieldTested, Hardened }

impl IntegrationTier {
    /// Single tier→evidence ladder consumed by parse_toml, critical_support.rs, slipfork support --json,
    /// and tools/check_support.py (never restated in Python).
    pub fn required_evidence(self) -> &'static [EvidenceField];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IntegrationCategory { WorldEngine, Secrets, Tunnel, Vcs, TestRunner, Cli }

#[derive(Debug, Clone, Copy, PartialEq, Eq)] // + as_str(self) -> &'static str + fmt::Display
pub enum EvidenceField {
    Rationale, SignoffDoc, SignoffCommit, RecipePage, CriticalTest,
    CiJob, DeployRef, ProductionSuiteRef, TestAudit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupportRegistry { /* private entries: Vec<IntegrationEntry> */ }

impl SupportRegistry {
    pub fn embedded() -> Result<Self, RegistryError>;   // include_str!("../../../support/integrations.toml")
    pub fn parse_toml(raw: &str) -> Result<Self, RegistryError>;
    pub fn entries(&self) -> &[IntegrationEntry];
}
```

- **User-Reachable Scope & Total Pointer Verification (`H-009`):** Only user-reachable integrations appear in `support/integrations.toml`; pre-CLI internal adapters (`WeavedRegistrar` in `M4.1` while gated on deferred `weaved` IPC per closed `H-006`) are omitted until `declared`. Both `critical_support.rs` (`M2.5`) and `tools/check_support.py --check` (`M2.15`) verify **all present** `EvidenceField` file/symbol pointers on every entry (not only tier-required ones), while `tools/check_support.py` additionally verifies `git` `signoff_commit` existence/attestation, `.github/workflows/*.yml` `ci_job` keys, and byte-exact parity between `docs/support-matrix.md` and the rendered Markdown table from `SupportRegistry::embedded()` using `required_evidence` from `slipfork support --json`.

---

## 4. Validated Architectural Decisions & Open Host Items (`plan/HUMAN.md`)

1. **`H-001` [Validated + Gate Confirmation]:** Mode-scoped components (`modes.<mode>.components.<id>` add/remove/override via `disabled: true`, `ADR-0001` §1) live in `snow.yml`, while `[artifacts]`, `[fixtures]` (`per_world`), `[resources]` (`ResourceStrategy`), and the typed `[test]` table (`coverage`, `coverage_report`, `coverage_fail_under`) live in `slipfork.toml` (`ADR-0006`, `ADR-0008`).
2. **`H-002` [Validated]:** Locked `::` namespace separator, unquoted `all:` universal mode, and hard rejection of top-level `envs:` and `project:` (`SnowError::ForbiddenTopLevelKey` in `M2.4`).
3. **`H-003` [Validated + Gate Addendum]:** `LayeredSecretProvider` persists newly generated `!!` secrets to the local `0600` vault in `local` mode and to cloud `EnvGroup` secrets in `cloud` mode; **OSPP is not a prerequisite for M2 or M3**, and Slipfork-assigned `WorldOrigin` always overrides provider `domains::` in local worlds (`ADR-0005` §1, `ADR-0008` §2–§3).
4. **`H-004` [Closed by Owner Ruling — Superseded by `ADR-0010`]:** Closed by owner ruling (2026-11-16) — superseded by `ADR-0010` (`BtrfsEngine` is Slipfork's sole `WorldEngine`). Zero open item remains.
5. **`H-005` [Validated + Gate Amendment]:** Canonical VCS `ProjectSlug` resolution (`ADR-0005` §3), single `WorldOrigin = Local { url } | Shared { url }` per world, and reserved `test-*` prefix across forks and `domains[].id`.
6. **`H-006` [Closed by Deferral — Isolated Leaf `M4.1`]:** `weaved`'s registration wire payload remains gated on `tunnel-weaver` publishing `weaved`'s IPC specification; `UnavailableTunnelRegistrar` makes `share_world` fail closed in the meantime (while `unshare_world` is an idempotent no-op per `ADR-0005` §2, and `UnavailableTunnelRegistrar` is deleted once `WeavedRegistrar` lands in `M4.1`), `weaved` forwards to `slipforkd`'s planned static `forward_port`, and service names obey `SERVICE_NAME_MAX = 31` bytes (`ADR-0008` §6).
7. **`H-008` [Validated]:** Unprivileged `LISTEN_FDS` `slipforkd` is the **sole local + shared ingress** (`shared_hosts` map + `snow.yml` `ingresses` prefixes -> `dial_world_tcp` with `Host` + `X-Forwarded-*` preserved; `*.localhost` -> `308`/`421` when `Shared`, `ADR-0005` §1–§2, `ADR-0008` §5).
8. **`H-009` [Validated — User-Reachable Scope]:** `ADR-0007`'s 5-tier scale (`none`..`hardened`) is scoped strictly to user-reachable integrations; pre-CLI internal library adapters are omitted from `support/integrations.toml` until wired + documented at `declared` (`plan/HUMAN.md`).
9. **`H-010` [Validated]:** Unified user daemon `slipforkd` (`User=<developer-user>`, `--state-dir`, `--runtime-dir`, `<runtime_dir>/daemon.sock`) is the single stateful Rust Composition Root owning worlds, persistent `OSPP v1` session, secrets, and daemon-owned `forward` listeners while thin-client `slipfork` enters worlds directly inheriting native terminal stdio (zero `SCM_RIGHTS` across the CLI<->daemon TTY boundary, zero `pty.rs` `unsafe`) and auto-spawns `slipforkd --listen 127.0.0.1:0` when `daemon.sock` is absent; `WorldOrigin::local(&route, ingress_port)` formats `:<port>` when `!= 80`; and `slipfork-ospp` links pinned git crates `tpmd-client` + `tpmd-ipc` (`rev = "90cb2db45b3ea6bc065918f11c7e0fd23b4ce837"`).
10. **`H-011` [Validated]:** Persistent per-world PID 1 (`slipfork-world-init`), `<control_dir>/netns.sock` (`WorldSpec.control_dir`, `ADR-0010` §2) `SCM_RIGHTS` dialer, and `<state_dir>/http-ingress.sock` `$BASE_URL` bridge — normative detail in §3.3, `ADR-0008` §3/§7, and `ADR-0010` §2/§6.
