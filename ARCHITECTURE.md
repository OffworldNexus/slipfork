# Slipfork Technical Architecture (`ARCHITECTURE.md`)

This document is the authoritative specification of Slipfork's **Composition Root**, **Inversion of Control / Dependency Injection (IoC/DI) Tree**, crate boundaries, wire protocols, and execution invariants (`ADR-0001`..`ADR-0010`, `H-001`..`H-011`).

The single Btrfs world engine (and its injected `SubvolumeDriver` seam, `ADR-0010`), world base-image builder, secret stores, OSPP capability/dump client, VCS workspace drivers, tunnel registrar, and language-blind `STP v1` test orchestrator MUST plug into the trait tree below without global state, ambient OS/env reads, downcasts, opaque extras bags, `match kind` sprawl, or fallback hacks. **All domain traits are synchronous (`std::net`, `UnixStream`, synchronous `tungstenite` over `rustls` + `webpki-roots`, `std::thread`; only `TpmdDeviceKeySigner` encapsulates a private `current_thread` `tokio::runtime::Runtime` internally to drive upstream `tpmd-client`). If a feature cannot fit this DI tree cleanly, stop and file a `direction-challenge` in `plan/HUMAN.md`.**

---

## 1. Composition Root & DI Tree (`H-010` Unified User Daemon & M3 Target-State `ADR-0010` Btrfs Engine)

> **Cargo Feature & Test-Scaffolding Rule (normative):** Cargo features are reserved for language/platform modularity; test scaffolding (such as `TestTreeDriver`, `FakeBaseImageBuilder`, `MemorySecretProvider`, and `InMemoryTunnelRegistrar`) lives strictly in `#[cfg(test)]` (crate-internal unit tests), `tests/support/` (per-crate integration suites, e.g. `crates/slipfork/tests/support/` supplying the test `SubvolumeDriver` to `doctor`/`probe_with` and in-process `slipforkd` in `M3.15`), or an optional `publish = false` workspace testkit crate in `[dev-dependencies]` — never behind a Cargo `[features]` flag on a production crate. (Existing M2 residue — `crates/slipfork-ospp`'s `test-server` feature enabled from `crates/slipfork/Cargo.toml` — is deleted in `M3.22` by moving those reference-server doubles into `crates/slipfork/tests/support/`.)

Only **two entrypoints** in the entire repository wire concrete adapters from ambient environment variables, wall clocks, OS sockets, or real filesystems:

1. **Rust Unified User Daemon Composition Root (`slipforkd` in `crates/slipfork`, shipped in `0.2.0`; M3 Phase 1 `BtrfsEngine` switchover in `M3.20`, `M3.12`–`M3.15`, `M3.21`; M3 Phase 2 `STP v1` wiring in `M3.2`):** Runs unprivileged as `User=<developer-user>` (`H-010`'s `%i` shorthand denotes the developer's own OS account, resolved once at the CLI composition boundary via `UserIdentity` into non-template `slipforkd.{socket,service}` plus M3 target-state `/etc/apparmor.d/slipfork` `userns` profile via `slipfork daemon unit --only apparmor` [`M3.15`, `ADR-0010` §7], or `--listen 127.0.0.1:0 --state-dir <path> --runtime-dir <path>` auto-spawned by the CLI). `slipforkd` receives explicit `--state-dir` and `--runtime-dir` flags (never defaulting or guessing `$XDG_RUNTIME_DIR` or `current_dir()`), binds `<runtime_dir>/daemon.sock` (`0600`) plus `<state_dir>/http-ingress.sock` (`0600`), injects each world's disjoint per-world control directory `WorldSpec.control_dir` (`ADR-0010` §2), and owns **all** stateful adapters in-process: shipped `0.2.0` `BaseImageBuilder`, `SecretProvider`, persistent `OsppClient`, `ResourcePlan`, `VcsWorkspaceFactory`, `WorldRegistry`, and `TunnelRegistrar`; M3 Phase 1 target-state `BtrfsEngine` (`M3.13`/`M3.15`) constructed over `Arc<dyn SubvolumeDriver>` = `Arc::new(KernelBtrfsDriver)` (`M3.12`, enforcing §3.3 / `ADR-0010` §4's fail-closed `SubvolumeDriver::ensure_supported` boot check); and M3 Phase 2 target-state (`M3.2`) `STP v1` orchestrator + `BtrfsArtifactStore` (`crates/slipfork-test`, sharing `Arc<dyn SubvolumeDriver>`). The `slipfork` CLI binary (`crates/slipfork/src/main.rs`) is a **thin client** (`clap` + `UserIdentity` + single composition-boundary `Layout::resolve` [resolving `state_dir` and `runtime_dir` once from CLI flags + `$HOME`/`$XDG_*`, using `$XDG_RUNTIME_DIR/slipfork` or `<state_dir>/run` when `$XDG_RUNTIME_DIR` is unset, before dialing `<runtime_dir>/daemon.sock`, rendering units, or auto-spawning `slipforkd`] + `DaemonControlClient` + direct `Command` world entry inheriting `Stdio::inherit()` with zero `SCM_RIGHTS` FD passing at the CLI<->daemon boundary, compile-time `SupportRegistry::embedded()`, unit/AppArmor profile rendering, and pre-daemon `slipfork doctor` constructing `KernelBtrfsDriver` at `crates/slipfork/src/main.rs` and passing `&dyn SubvolumeDriver` to `slipfork_engine::btrfs::probe_with(driver, state_dir)` [`M3.12`/`M3.15`, `ADR-0010` §4] while holding **zero** `WorldEngine`; `BtrfsEngine` spawns its internal PID 1 helper `slipfork-world-init`, §3.3).
2. **OpenCode V2 Plugin Composition Root:** `packages/opencode-slipfork/src/index.ts` (injecting explicit `SlipforkProjectState` in `M2.18` and routing shell calls via `slipfork exec --cwd <cwd>` without resolving or injecting `snow.yml` env vars itself, `ADR-0004`).

```mermaid
flowchart TD
  CLI["Thin CLI Client (crates/slipfork: slipfork, git-remote-slipfork)<br/>clap + UserIdentity + auto-spawn + DaemonControlClient + direct Stdio::inherit() exec<br/>holds NO WorldEngine, NO SecretProvider, NO TunnelLease"]
  CLI -->|0600 Unix JSON-line IPC<br/>&lt;runtime_dir&gt;/daemon.sock| DC["trait DaemonControl<br/>(crates/slipfork-edge, M2)"]
  CLI -->|compile-time read-only| SR["slipfork_dag::SupportRegistry::embedded()"]
  CLI -->|main.rs constructs KernelBtrfsDriver -&gt; probe_with(&amp;dyn SubvolumeDriver) (M3.15)| SD

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

  SnowRes --> WN["crates/slipfork-world<br/>ProjectSlug, WorldTarget, LocalHostRoute, WorldOrigin::local(&amp;route, port) &amp; WeaverServiceName (&lt;= 31 bytes)"]
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
  STPOrch -->|SLIPFORK_PROTOCOL_FD AF_UNIX socketpair| STP["STP v1 (ADR-0006 §3)"]
```

---

## 2. Crate & Package Boundaries

| Crate / Package | Milestone | Purity & `unsafe` Policy | Owned Domain Ports (Traits) & Responsibilities |
|---|---|---|---|
| `crates/slipfork-world` | M1 (shipped) + M2 `WorldOrigin` & `WeaverServiceName` (`M2.3`) | `#![forbid(unsafe_code)]`; depends only on `serde`, `thiserror` (+ `sha2`, `hex` in `M2.2`); zero OS socket/FD/env/fs types | Validated `ProjectSlug`, `WorldTarget { Canonical, UserFork, TestSlot }`, `WorldError`, encapsulated `LocalHostRoute`, M2 `WorldOrigin { Local, Shared }` (`WorldOrigin::local(&LocalHostRoute, ingress_port)` + `WorldOrigin::shared(url)`, `H-010` #2), `WeaverServiceName` (`SERVICE_NAME_MAX = 31`, `derive_weaver_service_name`, `ADR-0008` §6), and pure `TunnelRequest` + `TunnelLease` + `TunnelRegistrar` |
| `crates/slipfork-snow` | M1 (shipped) + M2 `Send + Sync` & `effective_components` (`M2.4`) | `#![forbid(unsafe_code)]`; depends on `slipfork-world` (zero `slipfork-dag` dep); zero ambient `env`/`net`/clock (`FileVaultProvider` `0600` is the sole vault filesystem writer; `M2.4` deletes M1 `OsppBundle` & `from_bundle`) | `SnowFile` v2 AST (`modes.all` + `modes.<m>`, `SnowFile::effective_components(mode)` per `ADR-0001` §1, `SnowError::ForbiddenTopLevelKey { key }` in `M2.4`), `Faceless` v2 (`??` vs `!!`), `ResourceCoordinates` (`WorldOrigin` precedence over `domains::`, redacted `Debug`), `SecretValue`, `SecretProvider: Send + Sync` (`&self`), `EntropySource: Send + Sync` (`&self`) |
| `crates/slipfork-dag` | M1 (shipped) + M2 `[resources]` & `fs` gate (`M2.5`) + `WorldImageConfig` (`M2.26`, shipped in `0.2.0`) | `#![forbid(unsafe_code)]`; depends on `slipfork-world` (zero `slipfork-snow`/`slipfork-engine`/`std::env`/clock dep); domain core is filesystem-free (`default = []`, `FsMerkleHasher`/`FsDigestCache` behind `#[cfg(feature = "fs")]`) | Root `SlipforkConfig` (`project`, `artifacts`, `fixtures`, `test: TestSpec`, M2 `resources: ResourcesSpec`, M2 `world: WorldImageConfig { image, stage }` — two optional keys under `#[serde(deny_unknown_fields)]`, `ADR-0009` §1), `FixtureSpec` (`per_world: bool`), `TestSpec` / `CoverageConfig` / `CoverageThreshold` (shipped in `0.1.0`), M2 `ResourceMode { Local, Test }`, `ResourceStrategy { Local, PreferRemote, Remote }`, `DumpPolicy`, `RemoteResourceOffer`, and single `ResourcePlan::resolve(...)` (`ADR-0008` §3), `SupportRegistry` + `IntegrationTier::required_evidence()` (`support/integrations.toml`, `ADR-0007`), `DagPlanner`, `InputHasher`, and `DigestCache` |
| `crates/slipfork-ospp` | M2 (`M2.13`–`M2.14`) | `#![forbid(unsafe_code)]`; depends on `slipfork-world`, `slipfork-snow`, `slipfork-dag`, `tungstenite` + `rustls` + `webpki-roots` (sync), `p256`, `zstd`, pinned git `tpmd-client` + `tpmd-ipc` (`rev = "90cb2db..."`) + private `tokio` (`rt`/`net`/`time` confined to `TpmdDeviceKeySigner`); zero `std::env`, zero `slipfork-engine` dep | Slipfork-owned `OSPP v1` sync WebSocket client (`ospp-v1` subprotocol, 16 KiB text-frame cap, `"ospp-v1" \|\| nonce_s(32) \|\| nonce_c(32) \|\| server_name`), `DeviceKeySigner` (`KeyId` = `p256:<66 hex>`, 64-byte raw `r \|\| s` `DeviceSignature`; generic `FallbackDeviceKeySigner<P, F>`, `TpmdDeviceKeySigner` [`sealed` floor, `ResumeOrCreate` over `KeyStorePaths`], `SoftwareP256Signer` [`DeviceKeyStore` + `EntropySource`]), fragment-only `PairingUrl` + `BrowserOpener`, `OsppSecretProvider` (zero fs writes), `RemoteResourceBundle` + single `OsppClient::resource_offer() -> Option<slipfork_dag::RemoteResourceOffer>` mapper, and bounded-window `zstd` `DumpStreamer` over `SeedDumpStore` (`ADR-0008` §1–§3) |
| `crates/slipfork-engine` | M1 + M2 (`L0` rootfs + `crun`) + **M3 target state (`M3.20`, `M3.12`–`M3.14`, `M3.21`, `ADR-0010`)** | M3 target state: `unsafe` confined strictly to two audited kernel modules — **`src/btrfs.rs`** (`M3.12` `KernelBtrfsDriver` per `ADR-0010` §§3–5) and **`src/linux_cow.rs`** (`M3.21`: `current_uid_gid`, `signal_process`, and `SCM_RIGHTS` `send_fd`/`recv_fd`), each with a per-call `// SAFETY:` proof (zero `pty.rs`, zero per-file extent-clone helpers, zero test Cargo features per §1); `#![forbid(unsafe_code)]` everywhere else (`src/btrfs_engine.rs`, `src/rootfs/`, `src/bin/slipfork-world-init.rs`) | `WorldEngine` (`M3.20` distinct RO snapshot vs writable clone signatures; zero mode flags, zero capability boolean bags), `SubvolumeDriver` (`M3.12` sole CoW & probe seam: `KernelBtrfsDriver` + `btrfs::probe_with(&dyn SubvolumeDriver, ...)` in prod; test-only `TestTreeDriver` per §1), `EngineError::{BtrfsRequired, ServiceReadinessFailed}`, `WorldSpec` (with injected `control_dir: PathBuf`), `ServiceSpec` / `ReadinessProbe` / `ServiceRuntime` (`ADR-0002` §2), opaque `WorldHandle`, `#![forbid(unsafe_code)]` `src/bin/slipfork-world-init.rs` (see §3.3 & `ADR-0010` §6), `src/rootfs/` (`WorldRootfs`, `WorldRootfsRequest`, `OciRootfs` [carrying `readiness: ReadinessProbe` in `M3.13`/`M3.15`], `trait BaseImageBuilder` + prod `DockerfileStageBuilder`, 6-rule World Image Contract — `ADR-0009` §1–§3 is normative; test-only `FakeBaseImageBuilder` per §1), and M3 sole production adapter `BtrfsEngine` (`M3.13`, `ADR-0002`, `ADR-0010` §1 — `M3.20` aligns `trait WorldEngine` beforehand, and `M3.21` deletes all previously shipped engine adapters, their daemon/REST protocol, storage pools, and per-file extent-clone helpers after `M3.12`–`M3.15` land) |
| `crates/slipfork-edge` | M2 (`M2.8`–`M2.10`, shipped in `0.2.0`) | `#![forbid(unsafe_code)]` (uses safe `listenfd` and `httparse`); depends on `slipfork-world`, `slipfork-engine`; `std::env` confined strictly to `activation.rs` (`LISTEN_PID` / `LISTEN_FDS`) | Unprivileged `slipforkd` HTTP/WS ingress (`adopt_systemd_listener`, `--listen`, `<state_dir>/http-ingress.sock` [§3.3], non-template `UnitSpec` renderer with `User=<developer-user>` & `ReadWritePaths=`, `httparse` `<= 16 KiB` cap, `ADR-0008` §5): `WorldRegistry` (`mint_origin`, `attach_world`, `release_world`, `share_world`, `unshare_world`), `IngressRoute` -> `dial_world_tcp`, `308`/`421` rules (`ADR-0005` §1), `DaemonControl` (`<runtime_dir>/daemon.sock` `0600`), `UnavailableTunnelRegistrar` + test-only `InMemoryTunnelRegistrar` (§1) + isolated `WeavedRegistrar` (`M4.1`) |
| `crates/slipfork-vcs` | M2 (`M2.11`–`M2.12`) | `#![forbid(unsafe_code)]`; depends only on `slipfork-world`, `serde`, `serde_json`, `thiserror` (zero `slipfork-dag` dep); zero `src/bin/*` targets | `ProjectSlugOverride` (injected `<vcs_root>/slipfork.toml` `project` reader), `RepoRootResolver` (`RepoRoot`), `VcsWorkspace` (`WorkspaceRefs`, `WorkspaceChanges`), `VcsWorkspaceFactory` (`WorkspaceRequest`), `GitWorktreeAdapter` (`.worktrees/<fork>`, `refs/heads/<fork>`, and D/F-safe `refs/slipfork/<fork>/wip` via `.slipfork/git/index-<fork>`), `JjWorkspaceAdapter` (`@-` -> `refs/heads/<fork>`, `@` -> `refs/slipfork/<fork>/wip`), and pure `remote_helper` (`git-remote-slipfork` line protocol over `BufRead`/`Write`) |
| `crates/slipfork-test` | **M3 Phase 2 target state (`M3.2`, `ADR-0006` §3)** | `#![forbid(unsafe_code)]`; depends on `slipfork-world`, `slipfork-dag`, `slipfork-edge`, `slipfork-engine`; injected `EntropySource` + `Arc<dyn SubvolumeDriver>` | Language-blind **`STP v1` DAG orchestrator** over inherited `SLIPFORK_PROTOCOL_FD` (`AF_UNIX` `socketpair`, `ADR-0006` §3): orchestrates global DAG nodes including persistent setup/fixture stage nodes frozen into RO Btrfs subvolume snapshots (`ADR-0010` §3), protocol-native coverage & mutation testing (`mutants`, zero hardcoded per-language Rust drivers, zero magic runtime guessing), `trait ArtifactStore` + `BtrfsArtifactStore` (sealing `[artifacts.*]` RO subvolumes via `Arc<dyn SubvolumeDriver>`), surviving `SLIPFORK_OUTPUT_DIR`, and Btrfs-warm `TestSlot` worlds (`ResourceMode::Test`) |
| `crates/slipfork` | M1..M4 (`M2.16`–`M2.17` shipped; **M3 target state `M3.15` & `M3.2`**) | `#![forbid(unsafe_code)]`; **`slipforkd` is sole stateful Rust Composition Root; `slipfork` is thin client** | Binaries `slipforkd` (`--state-dir` + `--runtime-dir` unified user daemon Composition Root assembling `DaemonService: DaemonControl`, constructing M3 target-state `BtrfsEngine::with_driver(..., Arc::new(KernelBtrfsDriver))` [`M3.15`, failing closed with `EngineError::BtrfsRequired` at boot before any world directory exists, `ADR-0010` §4–§5], injecting `WorldSpec.control_dir` [`ADR-0010` §2], and wiring `DockerfileStageBuilder` -> `WorldRootfs` -> `dyn WorldEngine` with `--host-caches`, `ADR-0009`), `slipfork` (thin `clap` + `UserIdentity` + `DaemonControlClient` + auto-spawner + M3 `daemon unit --only socket\|service\|apparmor` renderer [`M3.15`] + M3 `doctor` constructing `KernelBtrfsDriver` in `main.rs` and passing `&dyn SubvolumeDriver` to `slipfork_engine::btrfs::probe_with` [`M3.15`, `ADR-0010` §4] + direct `WorldEntryCommand` `Stdio::inherit()` executor; `HOME`-unset fail-closed guard), and `git-remote-slipfork` |
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
    fn write_from(&self, sha256_compressed: &str, src: &mut dyn std::io::Read) -> Result<PathBuf, OsppError>;
}

impl OsppClient {
    /// Sole conversion owner from wire `RemoteResourceBundle` to `slipfork_dag::RemoteResourceOffer`
    /// (redacting secrets, zero resolution logic); `slipforkd` passes the result directly to `ResourcePlan::resolve`.
    pub fn resource_offer(&self, project: &ProjectSlug, mode: &str) -> Result<Option<slipfork_dag::RemoteResourceOffer>, OsppError>;
}
```

### 3.3 `WorldEngine`, `SubvolumeDriver`, `ReadinessProbe`, Persistent `slipfork-world-init` & Disjoint Control/Socket Paths (`crates/slipfork-engine`, M3 target state `M3.20`, `M3.12`–`M3.15`, `M3.21`; `ADR-0002`, `ADR-0008`, `ADR-0009`, `ADR-0010`, `H-010`, `H-011`)

```rust
// crates/slipfork-engine::btrfs (M3 target state: M3.12) — sole CoW & probe seam (ADR-0010 §3–§5)
pub const BTRFS_SUPER_MAGIC: i64 = 0x9123_683e;

pub trait SubvolumeDriver: Send + Sync {
    /// Structured Btrfs host & mount-option report (`BtrfsProbeReport` fields specified once in `ADR-0010` §4).
    fn probe(&self, path: &Path) -> Result<BtrfsProbeReport, EngineError>;
    /// Fail closed with `EngineError::BtrfsRequired` (naming `mount -o user_subvol_rm_allowed,noatime`)
    /// unless `path` resides on `BTRFS_SUPER_MAGIC` (`ADR-0010` §4).
    fn ensure_supported(&self, path: &Path) -> Result<(), EngineError>;
    fn create_subvolume(&self, parent: &Path, name: &str) -> Result<PathBuf, EngineError>;
    fn snapshot_ro(&self, source_subvol: &Path, dest_parent: &Path, name: &str) -> Result<PathBuf, EngineError>;
    fn snapshot_writable(&self, source_subvol: &Path, dest_parent: &Path, name: &str) -> Result<PathBuf, EngineError>;
    /// Clears read-only flags internally before destroying `<parent>/<name>` (`ADR-0010` §3; zero boolean mode switch).
    fn destroy_subvolume(&self, parent: &Path, name: &str) -> Result<(), EngineError>;
    fn list_children(&self, parent: &Path) -> Result<Vec<PathBuf>, EngineError>;
}

/// Injected pre-daemon CLI probe helper (`M3.12`/`M3.15`, `ADR-0010` §4): `crates/slipfork/src/main.rs`
/// constructs `KernelBtrfsDriver` at the CLI composition boundary and passes `&dyn SubvolumeDriver` here
/// (holding NO `WorldEngine`), while tests pass a test `SubvolumeDriver` via §1 (`tests/support/` or dev-dep testkit).
pub fn probe_with(driver: &dyn SubvolumeDriver, state_dir: &Path) -> Result<BtrfsProbeReport, EngineError>;

// Production (`M3.13`): `BtrfsEngine::new(...)` delegates to `BtrfsEngine::with_driver(..., Arc::new(KernelBtrfsDriver))`.
// Tests (`M3.12`/`M3.14`/`M3.15`): `BtrfsEngine::with_driver(..., Arc<dyn SubvolumeDriver>)` receives test-only `TestTreeDriver`
// per §1's Cargo Feature & Test-Scaffolding Rule.
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
    pub digest: RootfsDigest,                                   // sha256:<64 lowercase hex> (ADR-0009 §3)
    pub app_rootfs: PathBuf,                                    // <state_dir>/cache/rootfs/<hex>/app
    pub crun_bin: PathBuf,                                      // /.slipfork/bin/crun (static ELF)
    pub service_images: BTreeMap<BackingServiceKey, OciRootfs>, // postgres:<v>, redis:<v>, seaweedfs
    pub metadata: ImageContractMetadata,                        // os_release, elf_abi, exposed_ports
    pub host_caches: Vec<WorldMount>,                           // empty unless --host-caches ABI probe passes
}

pub trait BaseImageBuilder: Send + Sync {
    fn materialize(
        &self,
        req: &WorldRootfsRequest,
        on_event: &mut dyn FnMut(RootfsBuildEvent),
    ) -> Result<WorldRootfs, BaseImageError>;
}

pub struct WorldSpec {
    pub target: WorldTarget,                       // slipfork_world::WorldTarget
    pub control_dir: PathBuf,                      // per-world control dir injected by slipforkd (ADR-0010 §2)
    pub rootfs: WorldRootfs,                       // Dockerfile-derived L0 rootfs + crun binary + OCI service images (ADR-0009 §1–§3)
    pub snapshot: SnapshotId,                      // content-addressed origin-independent RO Btrfs snapshot or base snapshot
    pub mounts: Vec<WorldMount>,                   // repo (RW) + ArtifactVolumes/SeedDump (RO) + M3 run dirs (RW)
    pub base_env: ExecEnv,                         // resolved at world boot from single WorldOrigin
    pub local_ingress: Option<LocalIngressBridge>, // Some when WorldOrigin::Local (H-010 #2, H-011 #4)
}

/// Opaque in-process handle owned exclusively inside `slipforkd` (never serialized across processes, H-010 #1).
#[derive(Clone, PartialEq, Eq)]
pub struct WorldHandle(/* private engine-scoped token */);

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
        readonly_mounts: Vec<WorldMount>,    // RO host -> container (e.g. the verified `<sha256>.pgdump`, ADR-0008 §2)
    },
}

/// RW `$UID:$GID` backing-service state mount inside the world's unified Btrfs subvolume (`ADR-0010` §2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateSubdirMount { pub host_subdir: PathBuf, pub container_path: PathBuf }

/// Distinct signatures separate RO snapshot creation (`create_snapshot`, `freeze_world`) from writable
/// world cloning (`clone_world`); no mode flag or capability boolean bag exists (`ADR-0010` §3, `M3.20`).
pub trait WorldEngine: Send + Sync {
    fn create_snapshot(&self, source_dir: &Path, id: &SnapshotId) -> Result<(), EngineError>;
    /// Stops `WorldProcess` services, then `OciContainer` services cleanly (`SIGTERM` -> bounded grace),
    /// then seals the unified world subvolume into an immutable RO snapshot (`ADR-0010` §3).
    fn freeze_world(&self, world: &WorldHandle, id: &SnapshotId) -> Result<(), EngineError>;
    fn clone_world(&self, spec: &WorldSpec) -> Result<WorldHandle, EngineError>;
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
    fn destroy_world(&self, world: WorldHandle) -> Result<(), EngineError>;
}
```

- **Btrfs Substrate, Single Probe Seam & Unified Layout (M3 target state `M3.12`–`M3.15`, `M3.21`; **`ADR-0010` §§2–5 is normative**):** `BtrfsEngine` (`crates/slipfork-engine/src/btrfs_engine.rs`) is the sole production `WorldEngine`, constructed via `BtrfsEngine::new` / `BtrfsEngine::with_driver(..., Arc<dyn SubvolumeDriver>)`. `crates/slipfork-engine::btrfs` (`SubvolumeDriver::{probe, ensure_supported}` implemented by `KernelBtrfsDriver`, plus injected `btrfs::probe_with(&dyn SubvolumeDriver, state_dir)`) is the single owner of Btrfs host probing (`BtrfsProbeReport` & `EngineError::BtrfsRequired`, `ADR-0010` §4), shared by `slipforkd` boot and pre-daemon `slipfork doctor` (`crates/slipfork/src/main.rs` constructs `KernelBtrfsDriver` and coerces it to `&dyn SubvolumeDriver` at the call site; tests pass a §1 test `&dyn SubvolumeDriver`) with zero duplicated probe logic. Unified single-subvolume world layout, disjoint `<runtime_dir>/daemon.sock` vs `<state_dir>/http-ingress.sock` vs `WorldSpec.control_dir` (`ADR-0010` §2), and the ioctl lifecycle follow `ADR-0010` §§2–5; engine-owned `ServiceSpec.readiness: ReadinessProbe` follows `ADR-0002` §2.
- **`WorldRootfs` / `BaseImageBuilder` Ownership & Single `crun` Supervision Surface (`ADR-0009` §1–§3):** `WorldRootfs`, `WorldRootfsRequest`, `BackingServiceKey`, `OciRootfs` (carrying `readiness: ReadinessProbe` in `M3.13`/`M3.15`, passed through unchanged to `ServiceSpec.readiness`), `ImageContractMetadata`, and `trait BaseImageBuilder` live in `#![forbid(unsafe_code)]` `crates/slipfork-engine/src/rootfs/` (zero `slipfork-snow`/`slipfork-dag` dep). `ServiceRuntime::OciContainer` specs flow through `spawn_service` / `stop_service` / `service_logs` alongside `WorldProcess` (`crates/slipfork-engine` owns the single `match runtime` over `WorldProcess | OciContainer` — no `ContainerRuntime` trait, no `dockerd`, no second supervisor). Static `crun` runs daemonless inside `slipfork-world-init`'s namespaces.
- **Persistent `slipfork-world-init` PID 1, `--ipc` (`CLONE_NEWIPC`), AppArmor `userns` & `<state_dir>/http-ingress.sock` Bridge (`H-011`; **`ADR-0010` §§6–7 is normative**):** Because multithreaded `setns` into an unprivileged userns/netns fails (`EINVAL`/`EPERM`), `BtrfsEngine::clone_world` spawns `#![forbid(unsafe_code)]` `crates/slipfork-engine/src/bin/slipfork-world-init.rs` under `ADR-0010` §6's unprivileged `unshare` namespace set (including normative `--ipc` / `CLONE_NEWIPC`, `M3.13`). `slipfork-world-init` brings `lo` up, writes `0` to `/proc/sys/net/ipv4/ip_unprivileged_port_start`, mounts the read-only `L0` root + subvolume state mounts (`ADR-0010` §2) + `spec.mounts` and `pivot_root`s into it, binds `0600` pathname `AF_UNIX` `<control_dir>/netns.sock` (`spec.control_dir`) to service `dial_world_tcp(port)` via `linux_cow.rs`'s audited `sendmsg`/`recvmsg` `SCM_RIGHTS` pair (`send_fd`/`recv_fd`, `MSG_CMSG_CLOEXEC`, rejecting >1 descriptors without leaking), and — when `WorldSpec.local_ingress` is `Some` — binds `127.0.0.1:<port>` (including `:80`) inside the world netns and splices accepted streams to `slipforkd`'s `0600` `<state_dir>/http-ingress.sock`. Ubuntu 24.04 LTS AppArmor `userns` profile rendering (`slipfork daemon unit --only apparmor`, `M3.15`) follows `ADR-0010` §7. Across the workspace, M3 `unsafe` lives solely in `crates/slipfork-engine/src/{btrfs.rs,linux_cow.rs}` (each call carrying a `// SAFETY:` proof; `H-010`'s "zero `SCM_RIGHTS`" rule applies strictly to the CLI <-> `slipforkd` TTY boundary).

### 3.4 Port-Aware `WorldOrigin::local`, `WorldRegistry` & `DaemonControl` (`crates/slipfork-world`, `crates/slipfork-edge`, `ADR-0005`, `ADR-0008`, `H-010`)

`slipforkd` runs unprivileged (`User=<developer-user>`, empty `CapabilityBoundingSet=`, explicit `--state-dir <path> --runtime-dir <path>`). Alongside its TCP HTTP listener(s), `slipforkd` binds a `0600` pathname `AF_UNIX` HTTP ingress listener at `<state_dir>/http-ingress.sock` (`H-011` #4) serving the exact same HTTP/WS dispatch loop for in-netns `$BASE_URL` bridges. In `activation.rs` (`#![forbid(unsafe_code)]` via `listenfd`), when `LISTEN_PID` or `LISTEN_FDS` is set, `adopt_systemd_listener() -> Result<Option<ListenerSet>, EdgeError>` verifies `LISTEN_PID == getpid()`, calls `set_nonblocking(true)` immediately on every inherited socket, and assigns roles by explicit descriptor count/order (`LISTEN_FDS == 1` -> single HTTP ingress + `forward_listener: None`; `LISTEN_FDS >= 2` -> `UnitSpec`'s emitted order `127.0.0.1:80`, `[::1]:80`, `127.0.0.1:<forward_port>`; any other shape fails closed `EdgeError::ActivationBroken`, `ADR-0008` §5). When both activation env vars are absent, `--listen <addr>` binds the HTTP ingress listener **only**; a forward listener exists in `--listen` mode **only** when `--forward-port <port>` is also passed (binding `127.0.0.1:<port>`), otherwise `share_world` / `DaemonControl::share` fails closed with `EdgeError::NoForwardListener` (and absent both activation and `--listen`, startup fails closed `EdgeError::NoListener`). HTTP/1.1 headers are parsed via `httparse` (`<= 16 KiB` cap, bounded read timeout), routing `.localhost` and `shared_hosts` requests through each world's `IngressRoute { prefix, port }` longest-prefix table (`snow.yml` `ingresses`) and enforcing `308`/`421` on `*.localhost` when `Shared`.

```rust
// crates/slipfork-world — pure identity, port-aware single origin, <= 31-byte service label & tunnel port
pub const SERVICE_NAME_MAX: usize = 31; // ADR-0008 §6 is normative for <= 31-byte derivation

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
// crates/slipfork-edge (M2) — activation, unit generator, canonical WorldRegistry & <runtime_dir>/daemon.sock RPC
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

pub struct WorldExecTarget {
    pub world: String,
    pub origin: WorldOrigin,
    pub entry: WorldEntryCommand, // world entry (`nsenter`) or a service-container entry (`crun exec`, ADR-0009 §3)
    pub env: ExecEnv,             // custom Debug redacts secrets; wire format restricted to 0600 <runtime_dir>/daemon.sock
    pub target_argv: Vec<String>,
}

/// Typed user-local RPC contract over `<runtime_dir>/daemon.sock` (`0600`, bounded 64 KiB JSON-line frames;
/// every cwd-sensitive request carries explicit `cwd: PathBuf`; 1:1 mapping: `share` -> `WorldRegistry::share_world`,
/// `unshare` -> `WorldRegistry::unshare_world`, `up`/`fork` -> `mint_origin` + `attach_world`, `down`/`drop_fork` -> `release_world`).
pub trait DaemonControl: Send + Sync {
    fn status(&self) -> Result<DaemonStatus, EdgeError>;
    fn up(&self, req: &UpRequest, on_event: &mut dyn FnMut(BootEvent)) -> Result<WorldUpSummary, EdgeError>; // req.cwd: PathBuf
    fn down(&self, req: &WorldSelector) -> Result<bool, EdgeError>;                                          // req.cwd: PathBuf
    fn fork(&self, req: &ForkRequest, on_event: &mut dyn FnMut(BootEvent)) -> Result<WorldUpSummary, EdgeError>; // req.cwd: PathBuf
    fn drop_fork(&self, req: &DropForkRequest) -> Result<bool, EdgeError>;
    fn list(&self, repo_root: Option<&Path>) -> Result<Vec<WorldSummary>, EdgeError>;
    fn prune(&self, dry_run: bool) -> Result<PruneReport, EdgeError>;
    fn share(&self, req: &WorldSelector) -> Result<SharedWorldInfo, EdgeError>;
    fn unshare(&self, req: &WorldSelector) -> Result<WorldOrigin, EdgeError>;
    fn secret_set(&self, repo_root: &Path, key: &str, value: &str, force: bool) -> Result<(), EdgeError>;
    fn secret_unset(&self, repo_root: &Path, key: &str) -> Result<bool, EdgeError>;
    fn secret_list(&self, repo_root: &Path) -> Result<Vec<String>, EdgeError>;
    fn logs(&self, req: &LogsRequest, on_chunk: &mut dyn FnMut(StreamChunk)) -> Result<(), EdgeError>;       // req.cwd: PathBuf
    fn exec_target(&self, req: &ExecTargetRequest) -> Result<WorldExecTarget, EdgeError>;                    // req.cwd: PathBuf
    // `ExecTargetRequest.service: Option<String>` selects a running backing-service container: `slipforkd`
    // resolves it with `WorldEngine::service_entry_command` and returns `entry` = `crun exec` (`slipfork db` ->
    // `psql`, `slipfork redis` -> `redis-cli`, ADR-0009 §3), failing closed `ServiceNotRunning` when stopped.
    // `None` keeps world entry (`nsenter`) with the daemon-resolved `ExecEnv`.
    fn hook_run(&self, req: &HookRunRequest, on_chunk: &mut dyn FnMut(StreamChunk)) -> Result<i32, EdgeError>; // req.cwd: PathBuf
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
```

- **`FixtureSpec.per_world` Read & Write Non-Sharing Invariant (`ADR-0003` §1):** `per_world` (`"per-world"` vs `"shared"`) is hashed into every fixture digest, cannot be the `parent` of an origin-independent fixture (`DagError::OriginIndependentChildOfPerWorld`), is **never** reported as `CacheAction::Cached` by `DagPlanner::plan`, and **MUST NEVER be inserted into `DigestCache`** by snapshot materializers or the test orchestrator.
- **Slipfork Testing Protocol (`STP v1`) & Language-Blind Orchestrator (Phase 2 `M3.2` target state; **`ADR-0006` §§3, 5 is normative**):** Over an inherited `SLIPFORK_PROTOCOL_FD` (`AF_UNIX` `socketpair`), `crates/slipfork-test` (`M3.2`) orchestrates global DAG nodes — freezing persistent setup/fixture stage nodes into read-only Btrfs subvolume snapshots (`ADR-0010` §3), driving protocol-native coverage (`TestSpec.coverage` / `CoverageThreshold`) and mutation testing (`mutants`), mounting surviving slot-scoped `SLIPFORK_OUTPUT_DIR` (`ADR-0006` §5), and following `ADR-0006` §3's zero-guessing, external-proof-plugin, and Phase 2 specification rules.

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
