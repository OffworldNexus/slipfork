# Slipfork Technical Architecture (`ARCHITECTURE.md`)

This document is the authoritative specification of Slipfork's **Composition Root**, **Inversion of Control / Dependency Injection (IoC/DI) Tree**, crate boundaries, wire protocols, and execution invariants (`ADR-0001`..`ADR-0008`, `H-001`..`H-010`).

Every hypervisor backend, secret store, OSPP capability/dump client, VCS workspace driver, tunnel registrar, or language test runner MUST plug into the trait tree below without global state, ambient OS/env reads, downcasts, opaque extras bags, `match kind` sprawl, or fallback hacks. **All domain traits are synchronous (`std::net`, `UnixStream`, synchronous `tungstenite` over `rustls` + `webpki-roots`, `std::thread`; only `TpmdDeviceKeySigner` encapsulates a private `current_thread` `tokio::runtime::Runtime` internally to drive upstream `tpmd-client`). If a feature cannot fit this DI tree cleanly, stop and file a `direction-challenge` in `plan/HUMAN.md`.**

---

## 1. Composition Root & DI Tree (`H-010` Unified User Daemon)

Only **two entrypoints** in the entire repository wire concrete adapters from ambient environment variables, wall clocks, OS sockets, or real filesystems:

1. **Rust Unified User Daemon Composition Root (`slipforkd` in `crates/slipfork`):** Runs unprivileged as `User=<developer-user>` (`H-010`'s `%i` shorthand denotes the developer's own OS account, resolved once at the CLI composition boundary via `UserIdentity` / `getpwuid_r(geteuid())` into non-template `slipforkd.{socket,service}`, or `--listen 127.0.0.1:0 --state-dir <path> --runtime-dir <path>` auto-spawned by the CLI). `slipforkd` never guesses `$XDG_RUNTIME_DIR` or `current_dir()`, binds `<runtime_dir>/daemon.sock` (`0600`), and owns **all** stateful adapters in-process (`WorldEngine`, `SecretProvider`, persistent `OsppClient`, `ResourcePlan`, `VcsWorkspaceFactory`, `WorldRegistry`, `TunnelRegistrar`). The `slipfork` CLI binary (`crates/slipfork/src/main.rs`) is a **thin client** (`clap` + `UserIdentity` + `DaemonControlClient` + auto-spawn + direct `Command` world entry inheriting `Stdio::inherit()` with zero `SCM_RIGHTS` FD passing, plus compile-time `SupportRegistry::embedded()` and `UnitSpec` rendering).
2. **OpenCode V2 Plugin Composition Root:** `packages/opencode-slipfork/src/index.ts` (injecting explicit `SlipforkProjectState` in `M2.18` and routing shell calls via `slipfork exec --cwd <cwd>` without resolving or injecting `snow.yml` env vars itself, `ADR-0004`).

```mermaid
flowchart TD
  CLI["Thin CLI Client (crates/slipfork: slipfork, git-remote-slipfork)<br/>clap + UserIdentity + auto-spawn + DaemonControlClient + direct Stdio::inherit() exec<br/>holds NO WorldEngine, NO SecretProvider, NO TunnelLease"]
  CLI -->|0600 Unix JSON-line IPC<br/>&lt;runtime_dir&gt;/daemon.sock| DC["trait DaemonControl<br/>(crates/slipfork-edge, M2)"]
  CLI -->|compile-time read-only| SR["slipfork_dag::SupportRegistry::embedded()"]

  CR["Unified Daemon Composition Root<br/>(slipforkd in crates/slipfork, User=&lt;developer-user&gt;)"]
  DC --> CR
  CR -->|serves HTTP/WS ingress &amp; forward| Edge["HttpIngress &amp; WorldRegistry<br/>(crates/slipfork-edge: LISTEN_FDS / --listen, ingresses prefixes, 308/421)"]
  CR -->|injects| SnowRes["SnowFile / ResourceCoordinates<br/>(crates/slipfork-snow)"]
  CR -->|injects| DagOrch["DagPlanner, SlipforkConfig &amp; ResourcePlan::resolve<br/>(crates/slipfork-dag)"]
  CR -->|holds persistent session| OsppSvc["OsppClient (resource_offer) &amp; DumpStreamer<br/>(crates/slipfork-ospp, M2)"]
  CR -->|injects| VcsSvc["RepoRootResolver &amp; VcsWorkspaceFactory<br/>(crates/slipfork-vcs, M2)"]
  CR -->|injects M3| TestCoord["TestCoordinator<br/>(crates/slipfork-test, M3)"]

  SnowRes --> SP["trait SecretProvider (Send + Sync)"]
  SnowRes --> RG["trait EntropySource (Send + Sync)"]
  SP --- SP1["FileVaultProvider (0600 JSON — sole local vault writer)"]
  SP --- SP2["OsppSecretProvider (ospp-v1 WebSocket, zero fs writes)"]
  SP --- SP3["LayeredSecretProvider&lt;P, F&gt; (reads Primary=FileVault -&gt; Fallback=Ospp; writes FileVault)"]
  SP --- SP4["MemorySecretProvider (tests)"]

  OsppSvc --> DKS["trait DeviceKeySigner (Send + Sync)"]
  OsppSvc --> KSP["KeyStorePaths &amp; trait DeviceKeyStore"]
  OsppSvc --> SDS["trait SeedDumpStore (DiskSeedDumpStore over non-tmpfs CacheRoot)"]
  DKS --- DKS0["FallbackDeviceKeySigner&lt;P, F&gt; (TpmdDeviceKeySigner [pinned tpmd-client/tpmd-ipc] -&gt; SoftwareP256Signer)"]

  DagOrch --> AS["trait ArtifactStore (M3) / DigestCache"]
  DagOrch --> VC["trait VolumeCloner (M3)"]
  DagOrch --> IH["trait InputHasher"]
  AS --- AS1["CowArtifactStore (uses dyn VolumeCloner)"]

  Edge --> WE["trait WorldEngine<br/>(crates/slipfork-engine — in-process inside slipforkd)"]
  Edge --> TR["trait TunnelRegistrar -&gt; Box&lt;dyn TunnelLease&gt;<br/>(crates/slipfork-world)"]
  WE --- WE1["ReflinkNetnsEngine (FICLONE + CLONE_NEW* + nsenter entry + ip_unprivileged_port_start=0 bridge + dialer)"]
  WE --- WE2["IncusEngine (Unix REST /1.0 + incus exec entry; live daemon gated on open H-004)"]
  TR --- TR1["UnavailableTunnelRegistrar (share_world fails closed until M2.22; deleted when WeavedRegistrar ships)"]
  TR --- TR2["WeavedRegistrar (isolated leaf M2.22; gated on weaved IPC spec per closed H-006)"]

  SnowRes --> WN["crates/slipfork-world<br/>ProjectSlug, WorldTarget, LocalHostRoute, WorldOrigin::local(&amp;route, port) &amp; WeaverServiceName (&lt;= 31 bytes)"]
  DagOrch --> WN
  OsppSvc --> WN
  WE --> WN
  Edge --> WN
  VcsSvc --> WN
  TestCoord --> WN

  VcsSvc --> VW["trait VcsWorkspaceFactory -&gt; VcsWorkspace"]
  VW --- VW1["GitWorktreeAdapter (.worktrees/&lt;fork&gt; + .slipfork/git/ scratch index)"]
  VW --- VW2["JjWorkspaceAdapter (jj workspace + @ / @- refs)"]

  TestCoord --> DagOrch
  TestCoord --> DC
  TestCoord --> TA["trait TestAdapter"]
  TA -->|optional capability| CD["trait CoverageDriver (M3)"]
  TA --- TA1["SocketRunnerAdapter (RunnerProtocol v1 -&gt; pytest-slipfork, Playwright, PHPUnit, nextest)"]
  TA --- TA2["CommandTestAdapter (zero-plugin CLI sharding)"]
```

---

## 2. Crate & Package Boundaries

| Crate / Package | Milestone | Purity & `unsafe` Policy | Owned Domain Ports (Traits) & Responsibilities |
|---|---|---|---|
| `crates/slipfork-world` | M1 (shipped) + M2 `WorldOrigin` & `WeaverServiceName` (`M2.3`) | `#![forbid(unsafe_code)]`; depends only on `serde`, `thiserror` (+ `sha2`, `hex` in `M2.2`); zero OS socket/FD/env/fs types | Validated `ProjectSlug`, `WorldTarget { Canonical, UserFork, TestSlot }`, `WorldError`, encapsulated `LocalHostRoute`, M2 `WorldOrigin { Local, Shared }` (`WorldOrigin::local(&LocalHostRoute, ingress_port)` + `WorldOrigin::shared(url)`, `H-010` #2), `WeaverServiceName` (`SERVICE_NAME_MAX = 31`, `derive_weaver_service_name`, `ADR-0008` §6), and pure `TunnelRequest` + `TunnelLease` + `TunnelRegistrar` |
| `crates/slipfork-snow` | M1 (shipped) + M2 `Send + Sync` & `effective_components` (`M2.4`) | `#![forbid(unsafe_code)]`; depends on `slipfork-world` (zero `slipfork-dag` dep); zero ambient `env`/`net`/clock (`FileVaultProvider` `0600` is the sole vault filesystem writer; `M2.4` deletes M1 `OsppBundle` & `from_bundle`) | `SnowFile` v2 AST (`modes.all` + `modes.<m>`, `SnowFile::effective_components(mode)` per `ADR-0001` §1, `SnowError::ForbiddenTopLevelKey { key }` in `M2.4`), `Faceless` v2 (`??` vs `!!`), `ResourceCoordinates` (`WorldOrigin` precedence over `domains::`, redacted `Debug`), `SecretValue`, `SecretProvider: Send + Sync` (`&self`), `EntropySource: Send + Sync` (`&self`) |
| `crates/slipfork-dag` | M1 (shipped) + M2 `[resources]` & `fs` gate (`M2.5`) + M3 | `#![forbid(unsafe_code)]`; depends on `slipfork-world` (zero `slipfork-snow`/`slipfork-engine`/`std::env`/clock dep); domain core is filesystem-free (`default = []`, `FsMerkleHasher`/`FsDigestCache` behind `#[cfg(feature = "fs")]`) | Root `SlipforkConfig` (`project`, `artifacts`, `fixtures`, `test: TestSpec`, M2 `resources: ResourcesSpec`), `FixtureSpec` (`per_world: bool`), `TestSpec` / `CoverageConfig` / `CoverageThreshold` (shipped in `0.1.0`), M2 `ResourceMode { Local, Test }`, `ResourceStrategy { Local, PreferRemote, Remote }`, `DumpPolicy`, `RemoteResourceOffer`, and single `ResourcePlan::resolve(...)` (`ADR-0008` §3), `SupportRegistry` + `IntegrationTier::required_evidence()` (`support/integrations.toml`, `ADR-0007`), `DagPlanner`, `InputHasher`, `DigestCache`, and M3 `ArtifactStore` + `VolumeCloner` |
| `crates/slipfork-ospp` | M2 (`M2.13`–`M2.14`) | `#![forbid(unsafe_code)]`; depends on `slipfork-world`, `slipfork-snow`, `slipfork-dag`, `tungstenite` + `rustls` + `webpki-roots` (sync), `p256`, `zstd`, pinned git `tpmd-client` + `tpmd-ipc` (`rev = "90cb2db..."`) + private `tokio` (`rt`/`net`/`time` confined to `TpmdDeviceKeySigner`); zero `std::env`, zero `slipfork-engine` dep | Slipfork-owned `OSPP v1` sync WebSocket client (`ospp-v1` subprotocol, 16 KiB text-frame cap, `"ospp-v1" \|\| nonce_s(32) \|\| nonce_c(32) \|\| server_name`), `DeviceKeySigner` (`KeyId` = `p256:<66 hex>`, 64-byte raw `r \|\| s` `DeviceSignature`; generic `FallbackDeviceKeySigner<P, F>`, `TpmdDeviceKeySigner` [`sealed` floor, `ResumeOrCreate` over `KeyStorePaths`], `SoftwareP256Signer` [`DeviceKeyStore` + `EntropySource`]), fragment-only `PairingUrl` + `BrowserOpener`, `OsppSecretProvider` (zero fs writes), `RemoteResourceBundle` + single `OsppClient::resource_offer() -> Option<slipfork_dag::RemoteResourceOffer>` mapper, and bounded-window `zstd` `DumpStreamer` over `SeedDumpStore` (`ADR-0008` §1–§3) |
| `crates/slipfork-engine` | M1 (shipped) + M2 full spec (`M2.2`, `M2.6`–`M2.7`) | `unsafe` confined strictly to **one** Linux syscall module — **`linux_cow.rs`** (`ioctl(FICLONE)`, `geteuid`/`getegid`, `getpwuid_r` (`current_username`), `setns(CLONE_NEWNET)`, with per-call `// SAFETY:` proofs; **zero `pty.rs` module**, `H-010` #1); `M2.2` adds `slipfork-world` dep (zero `slipfork-snow`/`slipfork-ospp` dep) | `WorldEngine` (`create_snapshot`, `freeze_world`, `clone_world`, `spawn_service`, `stop_service`, `service_logs`, streaming `exec_in_world`, `entry_command` -> `WorldEntryCommand` for direct CLI `Stdio::inherit()` entry, `dial_world_tcp` [daemon-internal only], `destroy_world`; `EngineCapabilities` deleted in `M2.6`), `ReflinkNetnsEngine`-owned in-netns loopback TCP bridge (`net.ipv4.ip_unprivileged_port_start=0`, `H-010` #2), `WorldSpec` (`WorldMount` RO/RW + self-contained redacted `ExecEnv`), `SnapshotMode`, opaque in-process `WorldHandle`; adapters: `ReflinkNetnsEngine` and `IncusEngine` (live daemon gated on `H-004`) |
| `crates/slipfork-edge` | M2 (`M2.8`–`M2.10`) | `#![forbid(unsafe_code)]` (uses safe `listenfd`, `socket2`, and `httparse`); depends on `slipfork-world`, `slipfork-engine`; `std::env` confined strictly to `activation.rs` (`LISTEN_PID` / `LISTEN_FDS`) | Unprivileged `slipforkd` HTTP/WS ingress (`adopt_systemd_listener` at `fd >= 3` with immediate `set_nonblocking(true)` + explicit fd-order `ListenerSet`, `--listen`, non-template `UnitSpec` `.socket`/`.service` renderer with `User=<developer-user>` & `ReadWritePaths=`, `httparse` with `<= 16 KiB` header cap, `ADR-0008` §5): `WorldRegistry` (`mint_origin`, `attach_world`, `release_world`, `share_world`, `unshare_world`) routing `*.localhost` and `shared_hosts` through `IngressRoute { prefix, port }` -> `dial_world_tcp`, enforcing `308`/`421` on `*.localhost` when `Shared` (`ADR-0005` §1, `H-008`); full `trait DaemonControl` (incl. daemon-owned `forward`) + `0600` `<runtime_dir>/daemon.sock` server/`DaemonControlClient` + `WorldExecTarget`; `UnavailableTunnelRegistrar` + `InMemoryTunnelRegistrar` (tests) + isolated `WeavedRegistrar` (`M2.22`) |
| `crates/slipfork-vcs` | M2 (`M2.11`–`M2.12`) | `#![forbid(unsafe_code)]`; depends on `slipfork-world`, `serde`, `serde_json`, `thiserror`; zero `src/bin/*` targets | `RepoRootResolver` (`RepoRoot`), `VcsWorkspace` (`WorkspaceRefs`, `WorkspaceChanges`), `VcsWorkspaceFactory` (`WorkspaceRequest`), `GitWorktreeAdapter` (`.worktrees/<fork>` + scratch `GIT_INDEX_FILE` under `.slipfork/git/`), `JjWorkspaceAdapter` (`@` -> `<fork>/wip`, `@-` -> `<fork>`), and pure `remote_helper` (`git-remote-slipfork` line protocol over `BufRead`/`Write`) |
| `crates/slipfork-test` | M3 | `#![forbid(unsafe_code)]`; depends on `slipfork-world`, `slipfork-dag`, `slipfork-edge`; injected `EntropySource` | `TestCoordinator` (consumes `ResourcePlan::resolve` via `DaemonControl`), canonical `.slipfork/runs/<run_id>/<slot>/{runner.sock,coverage,output}` run tree, `RunnerProtocol v1` (`SocketRunnerAdapter` + `CommandTestAdapter`), typed `CoverageDriver` (`ADR-0006` §5), and pre-warmed `TestSlot` world pool |
| `crates/slipfork` | M1..M4 (`M2.16`–`M2.17`) | `#![forbid(unsafe_code)]`; **`slipforkd` is sole stateful Rust Composition Root; `slipfork` is thin client** | Binaries `slipforkd` (`--state-dir` + `--runtime-dir` unified user daemon Composition Root assembling `DaemonService: DaemonControl`), `slipfork` (thin `clap` + `UserIdentity` + `DaemonControlClient` + auto-spawner + direct `WorldEntryCommand` `Stdio::inherit()` executor; `HOME`-unset fail-closed guard), and `git-remote-slipfork` |
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
    Local,        // world-local ResourceCoordinates (127.0.0.1:9000 in world netns) / clean initdb
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
    /// Sole owner of S3 + PG seed resolution consumed by `slipforkd` (`up`/`fork`), `OsppClient`, and `TestCoordinator`.
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

### 3.3 `WorldEngine`, Engine-Owned In-Netns Bridge & Direct `WorldEntryCommand` (`crates/slipfork-engine`, `ADR-0002`, `ADR-0008`, `H-010`)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotMode {
    DiskCow,
    StatefulRam, // unsupported engines fail closed with EngineError::StatefulRamUnsupported (no boolean bag)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountMode { ReadOnly, ReadWrite }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldMount {
    pub host_path: PathBuf,
    pub guest_path: PathBuf,
    pub mode: MountMode, // overlapping RW mounts fail closed (EngineError::OverlappingMounts)
}

/// Self-contained child environment (`Command::env_clear()`): restricted inherited allowlist
/// (`PATH`, `TERM`, `LANG` + world-scoped `HOME`) plus plain/secret vars (`effective = base ⊕ overlay`).
/// Custom `Debug` redacts every secret value; serialized over IPC ONLY inside `WorldExecTarget` on `0600` `<runtime_dir>/daemon.sock`.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ExecEnv { /* private allowlist + plain/secret entries; custom redacting Debug */ }

pub struct WorldSpec {
    pub target: WorldTarget,             // slipfork_world::WorldTarget (added in M2.2)
    pub snapshot: SnapshotId,            // content-addressed origin-independent fixture digest or base snapshot
    pub mounts: Vec<WorldMount>,         // repo (RW) + ArtifactVolumes/SeedDump (RO) + M3 run dirs (RW)
    pub base_env: ExecEnv,               // resolved at world boot from single WorldOrigin
    pub local_ingress_port: Option<u16>, // ReflinkNetnsEngine sets net.ipv4.ip_unprivileged_port_start=0 and binds 127.0.0.1:<port> -> slipforkd
}

/// Opaque in-process handle owned exclusively inside `slipforkd` (never serialized across processes, H-010 #1).
#[derive(Clone, PartialEq, Eq)]
pub struct WorldHandle(/* private engine-scoped token */);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamSource { Stdout, Stderr }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamChunk { pub source: StreamSource, pub bytes: Vec<u8> }

/// Engine-supplied argv prefix (`nsenter --target <init_pid> ...` or `incus exec <instance> ...`) allowing
/// the thin `slipfork` CLI to enter a running world directly inheriting native `Stdio::inherit()` (zero `SCM_RIGHTS`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldEntryCommand {
    pub program: PathBuf,
    pub args_prefix: Vec<String>,
    pub cwd: PathBuf,
}

pub trait WorldEngine: Send + Sync {
    fn name(&self) -> &'static str;
    fn create_snapshot(&self, source_dir: &Path, id: &SnapshotId, mode: SnapshotMode) -> Result<(), EngineError>;
    fn freeze_world(&self, world: &WorldHandle, id: &SnapshotId, mode: SnapshotMode) -> Result<(), EngineError>;
    fn clone_world(&self, spec: &WorldSpec) -> Result<WorldHandle, EngineError>;
    fn spawn_service(&self, world: &WorldHandle, spec: &ServiceSpec) -> Result<ServiceHandle, EngineError>;
    fn stop_service(&self, service: &ServiceHandle) -> Result<(), EngineError>;
    fn service_logs(&self, world: &WorldHandle, id: &str) -> Result<ServiceLogs, EngineError>;
    fn exec_in_world(
        &self,
        world: &WorldHandle,
        argv: &[String],
        cwd: &Path,
        overlay_env: &ExecEnv,
        on_output: &mut dyn FnMut(StreamChunk),
    ) -> Result<ExecOutput, EngineError>;
    fn entry_command(&self, world: &WorldHandle, cwd: &Path) -> Result<WorldEntryCommand, EngineError>;
    /// Daemon-internal only (`slipforkd` HTTP/WS ingress and `DaemonControl::forward` — never exposed over IPC).
    fn dial_world_tcp(&self, world: &WorldHandle, port: u16) -> Result<std::net::TcpStream, EngineError>;
    fn destroy_world(&self, world: WorldHandle) -> Result<(), EngineError>;
}
```

- **Single `unsafe` Module (`linux_cow.rs`), Engine-Owned Loopback Bridge & Shared `$UID`:** Because the thin CLI enters worlds directly via `WorldEntryCommand` + `Stdio::inherit()`, **zero `pty.rs` `unsafe` code exists**; `unsafe` across the entire workspace is confined solely to `crates/slipfork-engine/src/linux_cow.rs` (`ioctl(FICLONE)`, `current_uid_gid`, `current_username` via `getpwuid_r(geteuid())`, and `setns(CLONE_NEWNET)`). At world boot (`WorldSpec.local_ingress_port`), `ReflinkNetnsEngine` writes `0` to the per-netns sysctl `/proc/sys/net/ipv4/ip_unprivileged_port_start` (probe-verified in `M2.7`) and binds `127.0.0.1:<ingress_port>` inside the world `netns`, piping to `slipforkd`.

### 3.4 Port-Aware `WorldOrigin::local`, `WorldRegistry` & `DaemonControl` (`crates/slipfork-world`, `crates/slipfork-edge`, `ADR-0005`, `ADR-0008`, `H-010`)

`slipforkd` runs unprivileged (`User=<developer-user>`, empty `CapabilityBoundingSet=`, explicit `--state-dir <path> --runtime-dir <path>`). In `activation.rs` (`#![forbid(unsafe_code)]` via `listenfd`), when `LISTEN_PID` or `LISTEN_FDS` is set, `adopt_systemd_listener() -> Result<Option<ListenerSet>, EdgeError>` verifies `LISTEN_PID == getpid()`, calls `set_nonblocking(true)` immediately on every inherited socket, and assigns roles by explicit descriptor count/order (`LISTEN_FDS == 1` -> single HTTP ingress + `forward_listener: None`; `LISTEN_FDS >= 2` -> `UnitSpec`'s emitted order `127.0.0.1:80`, `[::1]:80`, `127.0.0.1:<forward_port>`; any other shape fails closed `EdgeError::ActivationBroken`, `ADR-0008` §5). When both activation env vars are absent, `--listen <addr>` binds the HTTP ingress listener **only**; a forward listener exists in `--listen` mode **only** when `--forward-port <port>` is also passed (binding `127.0.0.1:<port>`), otherwise `share_world` / `DaemonControl::share` fails closed with `EdgeError::NoForwardListener` (and absent both activation and `--listen`, startup fails closed `EdgeError::NoListener`). HTTP/1.1 headers are parsed via `httparse` (`<= 16 KiB` cap, bounded read timeout), routing `.localhost` and `shared_hosts` requests through each world's `IngressRoute { prefix, port }` longest-prefix table (`snow.yml` `ingresses`) and enforcing `308`/`421` on `*.localhost` when `Shared`.

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
    pub entry: WorldEntryCommand,
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
    fn hook_run(&self, req: &HookRunRequest, on_chunk: &mut dyn FnMut(StreamChunk)) -> Result<i32, EdgeError>; // req.cwd: PathBuf
    /// `slipforkd` binds host `127.0.0.1:0` listeners in-process and bridges each to `dial_world_tcp` for the
    /// lifetime of this control connection (client holds connection open; disconnect/Ctrl-C tears bridges down).
    fn forward(&self, req: &ForwardRequest, wait_disconnect: &mut dyn FnMut(ForwardSummary)) -> Result<(), EdgeError>;
    fn auth_login(&self, pairing_url: &str) -> Result<AuthSummary, EdgeError>;
}
```

### 3.5 `RepoRootResolver`, `VcsWorkspace` (`Git` + `Jujutsu`) & `git-remote-slipfork` (`crates/slipfork-vcs`, `ADR-0005`, `ADR-0008`)

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
    pub base: String, // refs/heads/<fork> in Git; @- in Jujutsu
    pub wip: String,  // synthesized via scratch .slipfork/git/index-<fork> in Git; @ in Jujutsu
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceChanges {
    pub tracked: Vec<PathBuf>,
    pub untracked: Vec<PathBuf>,
    pub truncated: bool,
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

- Zero `AdapterKind` enum outside `slipforkd`'s factory (`JjWorkspaceAdapter` when `.jj/` detected, else `GitWorktreeAdapter`), zero mutation of the live `.git/index`, and zero binary targets in `crates/slipfork-vcs` (`git-remote-slipfork` lives in `crates/slipfork` and drives pure `slipfork_vcs::remote_helper` over `BufRead`/`Write`).

### 3.6 World-Owns-The-Stack, `RunnerProtocol v1`, Typed `CoverageDriver` & `SLIPFORK_OUTPUT_DIR` (`crates/slipfork-dag`, `crates/slipfork-test` M3, `ADR-0003`, `ADR-0006`)

The CoW world — **never** the test runner — migrates PostgreSQL, seeds snapshots under hermetic `ResourceMode::Test` defaults (`ResourcePlan::resolve`, `ADR-0008` §3), assigns `WorldOrigin`, and boots services before tests run. `FixtureSpec.per_world: bool` and the typed `[test]` schema **shipped in `0.1.0`** in `crates/slipfork-dag`:

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

- **`FixtureSpec.per_world` Read & Write Non-Sharing Invariant (`ADR-0003` §1):** `per_world` (`"per-world"` vs `"shared"`) is hashed into every fixture digest, cannot be the `parent` of an origin-independent fixture (`DagError::OriginIndependentChildOfPerWorld`), is **never** reported as `CacheAction::Cached` by `DagPlanner::plan`, and **MUST NEVER be inserted into `DigestCache`** by M2/M3 snapshot materializers or `TestCoordinator`.

```rust
// crates/slipfork-test (M3) — canonical .slipfork/runs/<run_id>/<slot>/{runner.sock,coverage,output}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationTier { Batch, World }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredTest {
    pub id: String,
    pub fixture: String,
    pub isolation: IsolationTier,
}

pub struct WorldEndpoint {
    pub origin: WorldOrigin,
    pub env: BTreeMap<String, SecretValue>,
}

pub struct WorldSlotContext<'a> {
    pub target: &'a WorldTarget,
    pub endpoint: &'a WorldEndpoint,
    pub output_dir: &'a Path,           // .slipfork/runs/<run_id>/<slot>/output (SLIPFORK_OUTPUT_DIR)
    pub coverage_dir: Option<&'a Path>, // .slipfork/runs/<run_id>/<slot>/coverage (slot-stable at pre-warm boot)
}

pub trait CoverageDriver: Send + Sync {
    fn world_env(&self, world_coverage_dir: &Path) -> BTreeMap<String, String>;
    fn collect(
        &self,
        shard_dirs: &[PathBuf],
        formats: &BTreeSet<CoverageReportFormat>,
        report_dir: &Path,
    ) -> Result<CoverageSummary, TestError>;
}

pub trait TestAdapter: Send + Sync {
    fn name(&self) -> &'static str;
    fn coverage(&self) -> Option<&dyn CoverageDriver> { None }
    fn discover(&self, workspace: &Path, filters: &[String]) -> Result<Vec<DiscoveredTest>, TestError>;
    fn run_items(
        &self,
        slot: &WorldSlotContext<'_>,
        items: &[DiscoveredTest],
        on_result: &mut dyn FnMut(TestOutcome),
    ) -> Result<(), TestError>;
}
```

### 3.7 Integration Support Registry (`support/integrations.toml`, `crates/slipfork-dag`, `ADR-0007`, `H-009` Option B)

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

- **User-Reachable Scope & Total Pointer Verification (`H-009` Option B):** Only user-reachable integrations appear in `support/integrations.toml`; pre-CLI internal adapters (`incus-engine` while gated on open `H-004`, `WeavedRegistrar` while gated on deferred `weaved` IPC per closed `H-006`) are omitted until `declared` (`M2.5` deletes `0.1.0`'s temporary `incus-engine` `none` record). Both `critical_support.rs` (`M2.5`) and `tools/check_support.py --check` (`M2.15`) verify **all present** `EvidenceField` file/symbol pointers on every entry (not only tier-required ones), while `tools/check_support.py` additionally verifies `git` `signoff_commit` existence/attestation, `.github/workflows/*.yml` `ci_job` keys, and byte-exact parity between `docs/support-matrix.md` and the rendered Markdown table from `SupportRegistry::embedded()` using `required_evidence` from `slipfork support --json`.

---

## 4. Validated Architectural Decisions & Open Host Items (`plan/HUMAN.md`)

1. **`H-001` [Validated + Gate Confirmation]:** Mode-scoped components (`modes.<mode>.components.<id>` add/remove/override via `disabled: true`, `ADR-0001` §1) live in `snow.yml`, while `[artifacts]`, `[fixtures]` (`per_world`), `[resources]` (`ResourceStrategy`), and the typed `[test]` table (`coverage`, `coverage_report`, `coverage_fail_under`) live in `slipfork.toml` (`ADR-0006`, `ADR-0008`).
2. **`H-002` [Validated]:** Locked `::` namespace separator, unquoted `all:` universal mode, and hard rejection of top-level `envs:` and `project:` (`SnowError::ForbiddenTopLevelKey` in `M2.4`).
3. **`H-003` [Validated + Gate Addendum]:** `LayeredSecretProvider` persists newly generated `!!` secrets to the local `0600` vault in `local` mode and to cloud `EnvGroup` secrets in `cloud` mode; **OSPP is not a prerequisite for M2 or M3**, and Slipfork-assigned `WorldOrigin` always overrides provider `domains::` in local worlds (`ADR-0005` §1, `ADR-0008` §2–§3).
4. **`H-004` [Open — Isolated Leaf `M2.21`]:** This container host runs XFS (`reflink=1`) + unprivileged `netns`/`userns` (`ReflinkNetnsEngine` proven live); live `IncusEngine` conformance and `declared` promotion stay isolated in `M2.21` awaiting a project-scoped `incusd` socket (`plan/HUMAN.md`).
5. **`H-005` [Validated + Gate Amendment]:** Canonical VCS `ProjectSlug` resolution (`ADR-0005` §3), single `WorldOrigin = Local { url } | Shared { url }` per world, and reserved `test-*` prefix across forks and `domains[].id`.
6. **`H-006` [Closed by Deferral — Isolated Leaf `M2.22`]:** `weaved`'s registration wire payload remains deferred until `tunnel-weaver` publishes `weaved`'s IPC specification; `UnavailableTunnelRegistrar` makes `share_world` fail closed in the meantime (while `unshare_world` is an idempotent no-op per `ADR-0005` §2, and `UnavailableTunnelRegistrar` is deleted once `WeavedRegistrar` lands in `M2.22`), `weaved` forwards to `slipforkd`'s planned static `forward_port`, and service names obey `SERVICE_NAME_MAX = 31` bytes (`ADR-0008` §6).
7. **`H-008` [Validated]:** Unprivileged `LISTEN_FDS` `slipforkd` is the **sole local + shared ingress** (`shared_hosts` map + `snow.yml` `ingresses` prefixes -> `dial_world_tcp` with `Host` + `X-Forwarded-*` preserved; `*.localhost` -> `308`/`421` when `Shared`, `ADR-0005` §1–§2, `ADR-0008` §5).
8. **`H-009` [Validated — Option B]:** `ADR-0007`'s 5-tier scale (`none`..`hardened`) is scoped strictly to user-reachable integrations; pre-CLI internal library adapters are omitted from `support/integrations.toml` until wired + documented at `declared` (`plan/HUMAN.md`).
9. **`H-010` [Validated]:** Unified user daemon `slipforkd` (`User=<developer-user>`, `--state-dir`, `--runtime-dir`, `<runtime_dir>/daemon.sock`) is the single stateful Rust Composition Root owning worlds, persistent `OSPP v1` session, secrets, and daemon-owned `forward` listeners while thin-client `slipfork` enters worlds directly inheriting native terminal stdio (zero `SCM_RIGHTS`, zero `pty.rs` `unsafe`) and auto-spawns `slipforkd --listen 127.0.0.1:0` when `daemon.sock` is absent; `WorldOrigin::local(&route, ingress_port)` formats `:<port>` when `!= 80` with `ReflinkNetnsEngine`'s `ip_unprivileged_port_start=0` loopback TCP bridge; and `slipfork-ospp` links pinned git crates `tpmd-client` + `tpmd-ipc` (`rev = "90cb2db45b3ea6bc065918f11c7e0fd23b4ce837"`).
