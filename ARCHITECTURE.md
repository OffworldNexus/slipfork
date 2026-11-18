# Slipfork Technical Architecture (`ARCHITECTURE.md`)

This document is the authoritative specification of Slipfork's **Composition Root**, **Inversion of Control / Dependency Injection (IoC/DI) Tree**, crate boundaries, wire protocols, and execution invariants (`ADR-0001`..`ADR-0011`, `H-001`..`H-011`).

The single Btrfs world engine (and its injected `SubvolumeDriver` seam, `ADR-0010`), world base-image builder, local `0600` file vault (`FileVaultProvider`), OSPP v1 live subscription/dump client (`docs/design/ospp.md`), VCS workspace drivers, and language-blind `STP v1` test orchestrator MUST plug into the trait tree below without global state, ambient OS/env reads, downcasts, opaque extras bags, `match wire_version` / `match kind` sprawl, stub/unavailable composition-root adapters (`tools/check_stub_adapters.py`, `M3.28.16`), or fallback hacks. **All domain traits are 100% synchronous (`std::net`, `UnixStream`, synchronous `tungstenite` over `rustls` + `webpki-roots`, `std::thread` — zero `tokio`, and remote OSPP secrets never touch disk). If a feature cannot fit this DI tree cleanly, stop and file a `direction-challenge` in `plan/HUMAN.md`.**

---

## 1. Composition Root & DI Tree (`H-010` Unified User Daemon & M3 Target-State `ADR-0010` Btrfs Engine)

> **Cargo Feature & Test-Scaffolding Rule (normative):** Cargo features are reserved for language/platform modularity; test scaffolding (such as `TestTreeDriver`, `FakeHostProbe`, `FakeBaseImageBuilder`, and `MemorySecretProvider`) lives strictly in `#[cfg(test)]` (crate-internal unit tests), `tests/support/` (per-crate integration suites, e.g. `crates/slipfork/tests/support/` supplying a test `SubvolumeDriver` to `doctor` and in-process `slipforkd`, plus `FakeHostProbe` to `doctor`, in `M3.15`), or an optional `publish = false` workspace testkit crate in `[dev-dependencies]` — never behind a Cargo `[features]` flag on a production crate. (The M2 residue — `crates/slipfork-ospp`'s `test-server` feature enabled from `crates/slipfork/Cargo.toml` — was deleted in `M3.22`: those reference-server doubles now live in the unpublished `crates/slipfork-ospp-testkit` crate (`publish = false`), consumed as a `[dev-dependencies]` entry by both protected test suites.) **Dev-dependency double-compile rule (normative):** a `publish = false` testkit crate that itself depends on the crate under test (`slipfork-engine` →dev→ `slipfork-testkit` →normal→ `slipfork-engine`) makes Cargo compile that crate **twice**, so a testkit double implements a different copy of the crate's traits than its own `--lib` unit tests see — such doubles are consumed from integration suites (`tests/`), which link the normal library and can pass `&TestTreeDriver as &dyn SubvolumeDriver`, while `--lib` unit tests use crate-internal fakes.

Only **two entrypoints** in the entire repository wire concrete adapters from ambient environment variables, wall clocks, OS sockets, or real filesystems:

1. **Rust Unified User Daemon Composition Root (`slipforkd` in `crates/slipfork`, shipped in `0.1.0`; M3 Phase 1 `BtrfsEngine` switchover in `M3.20`, `M3.12`–`M3.15`, `M3.21`; `0.1.0` UX pass in `M3.28`, `ADR-0011`; `0.1.0` OSPP v1 target state in `M3.29`, `docs/design/ospp.md`; M3 Phase 2 `STP v1` wiring in `M3.2`):** Runs unprivileged as `User=<developer-user>` (`H-010`'s `%i` shorthand denotes the developer's own OS account, resolved once at the CLI composition boundary via `UserIdentity` into non-template `slipforkd.{socket,service}` plus `/etc/apparmor.d/slipfork` `userns` profile via `slipfork daemon unit` / installed atomically via `slipfork daemon install` [`M3.15`, `M3.28.6`, `ADR-0010` §7, `ADR-0011` §1], or `--listen 127.0.0.1:0 --state-dir <path> --runtime-dir <path>` auto-spawned by the CLI). `slipforkd` receives explicit `--state-dir` and `--runtime-dir` flags (never defaulting or guessing `$XDG_RUNTIME_DIR` or `current_dir()`; optional composition-root helper overrides `--crun <path>` and `--world-init <path>` inject explicit binary paths into `runtime_assets`, `ADR-0009` §1) plus one injected `OsppEndpoint` (the single server identity authority across `connect`, `subscribe`, `dump`, and `auth_pair`), binds `<runtime_dir>/daemon.sock` (`0600`) plus `<state_dir>/http-ingress.sock` (`0600`), provisions `runtime_assets` (`ADR-0009` §1), injects each world's disjoint per-world control directory `WorldSpec.control_dir` (`ADR-0010` §2), constructs `SystemBrowserOpener` (`crates/slipfork/src/browser.rs`, `M3.28.9`), and owns **all** stateful adapters in-process: shipped `0.1.0` `BaseImageBuilder`, injected `SecretVault` (`crates/slipfork-edge`: `get`/`put`/`remove`/`keys`, wired solely at the Composition Root over `FileVaultProvider` for local `0600` backing-service credentials, local `!!` secrets, and `slipfork secret` overrides — **remote OSPP secrets never touch disk**, and `SecretVault::keys` feeds `OsppClaims`), `OsppClient` + live `OsppSession` subscription lifecycle (keeping the `ospp-v1` WebSocket open while worlds run, holding each subscription's ephemeral `hpke-p256` private key `S=` strictly in memory, decrypting server-resolved secrets in memory, updating affected service envs via `WorldEngine::update_service_env` and restarting through `M3.28.17`'s shared boot pipeline on `env_updated`, re-subscribing with the same `id` on config/vault change, unsubscribing on stop, and streaming HPKE-encrypted dumps via `OsppSession::dump` straight into `zstd -d | pg_restore` before `freeze_world` with zero plaintext at rest), `ResourcePlan`, `VcsWorkspaceFactory` (`create_workspace` + `current_branch`, `M3.28.5`) + `RepoRootResolver` (resolving the verbatim Git `remote` string for OSPP `subscribe`), and `WorldRegistry` (untagged `struct WorldOrigin(LocalOrigin)` `.localhost` origins only; one origin kind, tombstone in `ADR-0011`); `BtrfsEngine` (`M3.13`/`M3.15`/`M3.28.4`/`M3.29`) constructed over `Arc<dyn SubvolumeDriver>` = `Arc::new(KernelBtrfsDriver)` (`M3.12`, enforcing §3.3 / `ADR-0010` §4's magic-only fail-closed `SubvolumeDriver::ensure_supported` boot check and an injected monotonic clock stamping `uptime_secs` and per-world `WorldLineage` in `WorldEngine::world_processes` & `world_lineage`); and M3 Phase 2 target-state (`M3.2`) `STP v1` orchestrator + `BtrfsArtifactStore` (`crates/slipfork-test`, sharing `Arc<dyn SubvolumeDriver>`). The `slipfork` CLI binary (`crates/slipfork/src/main.rs`) is a **thin client** (`clap` with optional subcommand defaulting to `status` [`M3.28.7`] + `UserIdentity`/`HostIdentity` [`crates/slipfork::user`, sole reader of `SUDO_USER` via `invoking_account`] + single `Layout::for_account` / `Layout::resolve` [resolving the invoking account + `for_account`, with `<state_dir>/run` as the client's only socket directory (`ADR-0011` §2), before dialing `<state_dir>/run/daemon.sock`, rendering/installing units, or auto-spawning `slipforkd` — which only `up` and `fork` may do] + `DaemonControlClient` + single `resolve_world_selector` parser [`ADR-0005` §3, `M3.15`] + direct `Command` world entry inheriting `Stdio::inherit()` with zero `SCM_RIGHTS` FD passing at the CLI<->daemon boundary, `slipfork open` feeding typed `&WorldOrigin` to `SystemBrowserOpener` [`crates/slipfork/src/browser.rs`, `M3.28.9`], `slipfork daemon install` driving narrow `trait UnitInstaller` [`OsUnitInstaller`, `M3.28.6`, `ADR-0011` §1], and pre-daemon `slipfork doctor` constructing `KernelBtrfsDriver` + `OsHostProbe` + `SystemdIngressProbe` in `crates/slipfork/src/main.rs` and passing `(&dyn SubvolumeDriver, &dyn HostProbe, &dyn IngressProbe)` to `slipfork::doctor::run` [`M3.12`/`M3.15`/`M3.28.6`, `ADR-0010` §4, `ADR-0011` §3] while holding **zero** `WorldEngine`, **zero** `SecretVault`/`SecretProvider`, and **zero** `OsppClient`/HPKE keys — never opening a vault file or receiving a remote OSPP secret outside `WorldExecTarget` child spawning; `BtrfsEngine` spawns its internal PID 1 helper `slipfork-world-init`, §3.3).
2. **OpenCode V2 Plugin Composition Root:** `packages/opencode-slipfork/src/index.ts` (injecting explicit `SlipforkProjectState` in `M2.18` and routing shell calls via `slipfork exec --cwd <cwd>` without resolving or injecting `snow.yml` env vars itself, `ADR-0004`).

```mermaid
flowchart TD
  CLI["Thin CLI Client (crates/slipfork: slipfork, git-remote-slipfork)<br/>clap (None =&gt; status) + UserIdentity/HostIdentity + Layout::for_account (&lt;state_dir&gt;/run) + auto-spawn + DaemonControlClient + Stdio::inherit() exec<br/>holds NO WorldEngine, NO SecretProvider, NO OsppClient"]
  CLI -->|0600 Unix JSON-line IPC<br/>&lt;state_dir&gt;/run/daemon.sock| DC["trait DaemonControl<br/>(crates/slipfork-edge: status, up, restart, world_processes, fork, secret_set/unset/list, auth, shutdown, ...)"]
  CLI -->|main.rs constructs SystemBrowserOpener (src/browser.rs)| BO["trait BrowserOpener<br/>(crates/slipfork-ospp: open_pairing_url + open_origin)"]
  CLI -->|main.rs constructs OsUnitInstaller (M3.28.6, ADR-0011 §1)| UI["trait UnitInstaller<br/>(crates/slipfork::daemon_unit_install: narrow read/write/reload/apparmor/enable/restart)"]
  CLI -->|main.rs constructs KernelBtrfsDriver + OsHostProbe + SystemdIngressProbe| DR["doctor::run(&amp;dyn SubvolumeDriver, &amp;dyn HostProbe, &amp;dyn IngressProbe)<br/>(crates/slipfork::doctor — calls btrfs::probe_with)"]
  DR --> SD
  DR --> HP["trait HostProbe<br/>(crates/slipfork::doctor: probe_userns + probe_netns)"]
  DR --> IP["trait IngressProbe<br/>(crates/slipfork::doctor: probe_ingress :80 socket status)"]
  HP --- HP1["OsHostProbe (sole prod adapter: fixed-PATH unshare + AppArmor EPERM hint)"]
  HP --- HP2["FakeHostProbe (test-only; §1)"]
  IP --- IP1["SystemdIngressProbe (sole prod adapter: fixed-PATH systemctl is-active/show)"]
  UI --- UI1["OsUnitInstaller (atomic temp+rename + systemctl/apparmor_parser)"]

  CR["Unified Daemon Composition Root<br/>(slipforkd in crates/slipfork, User=&lt;developer-user&gt;)"]
  DC --> CR
  CR -->|serves HTTP/WS ingress &amp; forward| Edge["HttpIngress &amp; WorldRegistry<br/>(crates/slipfork-edge: LISTEN_FDS / --listen, Local .localhost ingresses prefixes)"]
  CR -->|injects local vault port| SV["trait SecretVault (get/put/remove/keys)<br/>(crates/slipfork-edge, M3.29: keys() feeds OsppClaims)"]
  CR -->|injects| SnowRes["SnowFile / ResourceCoordinates<br/>(crates/slipfork-snow)"]
  CR -->|injects| DagOrch["SlipforkConfig &amp; ResourcePlan::resolve<br/>(crates/slipfork-dag: snow + claims/pairing + local coordinates)"]
  CR -->|injects single OsppEndpoint &amp; owns live subscriptions| OsppSvc["trait OsppClient -&gt; trait OsppSession<br/>(crates/slipfork-ospp, M3.29 target state: pair, connect -&gt; subscribe/dump/unsubscribe/close;<br/>per-subscription ephemeral hpke-p256 key S= &amp; remote secrets in memory only)"]
  CR -->|injects| VcsSvc["RepoRootResolver (incl. verbatim Git remote) &amp; VcsWorkspaceFactory<br/>(crates/slipfork-vcs, M2 / M3.29)"]
  CR -->|injects Phase 2 M3.2| STPOrch["Language-Blind STP v1 DAG Orchestrator<br/>(crates/slipfork-test, Phase 2 M3.2)"]

  CR -->|parses world.image + world.stage, holds BuildImage| BIB["trait BaseImageBuilder<br/>(crates/slipfork-engine::rootfs)"]
  BIB --- BIB1["DockerfileStageBuilder<br/>(BuildxExporter + OciImageStore + AbiProbe)"]
  BIB --- BIB2["FakeBaseImageBuilder (test-only; §1)"]
  BIB -->|WorldRootfs (§3.3)| WE

  SV -->|wired at CR| SP1
  SnowRes --> SP["trait SecretProvider (Send + Sync: read/ensure resolver input)"]
  SnowRes --> RG["trait EntropySource (Send + Sync)"]
  SP --- SP1["FileVaultProvider (0600 JSON — local credentials/overrides &amp; local !! ONLY; remote OSPP secrets never touch disk)"]
  SP --- SP2["MemorySecretProvider (test-only; §1)"]

  OsppSvc --> OsppProd["WsOsppClient (ospp-v1 all-JSON WebSocket + out-of-band HTTPS SetupBaseR dump reader)"]
  OsppSvc --> DKS["trait DeviceKeySigner &amp; trait DeviceKeyStore<br/>(ECDSA P-256 machine auth identity: SoftwareP256Signer + FileDeviceKeyStore over DeviceKeyPath)"]
  OsppSvc --> BO
  BO --- BO1["SystemBrowserOpener (crates/slipfork/src/browser.rs; fails closed headless on open_origin)"]

  Edge --> WE["trait WorldEngine<br/>(crates/slipfork-engine — in-process inside slipforkd)"]
  WE --- WE1["BtrfsEngine (M3.13 sole prod WorldEngine, ADR-0010: one unified Btrfs subvolume per world + WorldSpec.control_dir + slipfork-world-init PID 1 + crun OCI services + ReadinessProbe)"]
  WE1 -->|constructor DI Arc&lt;dyn SubvolumeDriver&gt;| SD["trait SubvolumeDriver<br/>(crates/slipfork-engine::btrfs, M3.12 — sole CoW &amp; probe seam)"]
  SD --- SD1["KernelBtrfsDriver (sole prod SubvolumeDriver: probe + ensure_supported -&gt; BtrfsRequired)"]
  SD --- SD2["TestTreeDriver (test-only; §1)"]

  SnowRes --> WN["crates/slipfork-world<br/>ProjectSlug (local only), WorldTarget, WorldId, LocalHostRoute, WorldOrigin::local(&amp;route, port) &amp; WeaverServiceName (&lt;= 31 bytes)"]
  DagOrch --> WN
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
| `crates/slipfork-world` | M1 (shipped) + M2 `WorldOrigin` & `WeaverServiceName` (`M2.3`) + **`M3.28.15` untagged `struct WorldOrigin`** (typed `WorldId` is `0.2.0` target state) | `#![forbid(unsafe_code)]`; depends only on `serde`, `thiserror` (+ `sha2`, `hex` in `M2.2`); zero OS socket/FD/env/fs types | Validated local-only `ProjectSlug` (never sent on the OSPP wire), `WorldTarget { Canonical, UserFork, TestSlot }`, `0.1.0` world ids as the canonical `<project>` / `<project>:<fork>` strings (`crates/slipfork-edge/src/registry.rs::world_id`; typed `WorldId` `Display` + `FromStr` + `WorldId::parse_selector` is `0.2.0`, `ADR-0005` §3), `WorldError`, encapsulated `LocalHostRoute`, untagged `struct WorldOrigin(LocalOrigin)` (`WorldOrigin::local(&LocalHostRoute, ingress_port)`, `H-010` #2; one origin kind — tombstone in `ADR-0011`), and `WeaverServiceName` (`SERVICE_NAME_MAX = 31`, `derive_weaver_service_name`, `ADR-0008` §6) |
| `crates/slipfork-snow` | M1 (shipped) + M2 `Send + Sync` & `effective_components` (`M2.4`) + **`M3.29` target state (`M3.29.3`)** | `#![forbid(unsafe_code)]`; depends on `slipfork-world` (zero `slipfork-dag` dep); zero ambient `env`/`net`/clock (`FileVaultProvider` `0600` is the sole local-vault filesystem writer, wired at the Composition Root — remote OSPP secrets never touch it) | `SnowFile` v2 AST (`modes.all` + `modes.<m>`, `SnowFile::effective_components(mode)` per `ADR-0001` §1, `SnowError::ForbiddenTopLevelKey { key }` in `M2.4`), `Faceless` v2 (`??` vs `!!`), `ResourceCoordinates` (`WorldOrigin` precedence over `domains::`, redacted `Debug`), `SecretValue`, read/ensure resolver input `SecretProvider: Send + Sync` (`&self`), `FileVaultProvider` (`0600` local credentials/overrides & local `!!` only), `EntropySource: Send + Sync` (`&self`) |
| `crates/slipfork-dag` | M1 (shipped) + M2 `[resources]` (`M2.5`) + `WorldImageConfig` (`M2.26`, shipped in `0.1.0`) + `M3.28.11` `fs` sweep + **`M3.29` target state** | `#![forbid(unsafe_code)]`; depends on `slipfork-world` (zero `slipfork-snow`/`slipfork-engine`/`std::env`/`std::fs`/clock dep; `M3.28.11` deletes `src/fs.rs` `FsMerkleHasher`/`FsDigestCache` and the `fs` feature alongside `slipfork dag`) | Root `SlipforkConfig` (`project`, `artifacts`, `fixtures`, `test: TestSpec`, M2 `resources: ResourcesSpec`, M2 `world: WorldImageConfig { image, stage }` — two optional keys under `#[serde(deny_unknown_fields)]`, `ADR-0009` §1), `ArtifactSpec` + `FixtureSpec` (`per_world: bool`), `TestSpec` / `CoverageConfig` / `CoverageThreshold` (shipped in `0.1.0`), `ResourceMode { Local, Test }`, `ResourceStrategy { Local, PreferRemote, Remote }`, `DumpPolicy`, and single `ResourcePlan::resolve(...)` (`M3.29` target state: resolves from `snow` + `claims`/pairing state + local coordinates only, `ADR-0008` §3); `DagPlanner`/`InputHasher`/`DigestCache` and `SupportRegistry` are not wired into the `0.1.0` composition root (retained because protected `critical_dag.rs` / `critical_support.rs` pin them and `0.2.0`'s `slipfork test` consumes the planner) |
| `crates/slipfork-ospp` | **`M3.29` target state (`M3.29.2` clean-sheet OSPP v1 rewrite, `docs/design/ospp.md`)** + `open_origin` (`M3.28.9`) | `#![forbid(unsafe_code)]`; **`M3.29.2` target-state Cargo deps:** `slipfork-world`, `slipfork-snow`, `tungstenite` + `rustls` + `webpki-roots` (sync), `p256` (`0.14.0`), `hpke = { version = "0.15.1", default-features = false, features = ["alloc", "nistp", "aes", "hkdfsha2"] }`, `sha2` (`0.10.9`), `hex` (`0.4.3`), `base64` (`0.22.1`), `zstd` (`0.14.4`), removing `tokio` and `tpmd-*` in `M3.29.2`; zero `std::env`, zero `slipfork-engine`/`slipfork-dag` dep; **zero OSPP secret or plaintext-dump filesystem writes** (`FileDeviceKeyStore` writes only the `0600` machine authentication P-256 scalar at the injected `DeviceKeyPath`) | Clean-sheet **`OSPP v1`** sync WebSocket + HTTPS dump client (`docs/design/ospp.md` is sole normative wire spec: `Sec-WebSocket-Protocol: ospp-v1`, 100% JSON text frames discriminated by `#[serde(tag = "type", rename_all = "snake_case")]`, zero `#[serde(flatten)]` / `extra` bag / `match wire_version`, zero binary WS frames): single injected `OsppEndpoint`, single client capability `trait OsppClient` + `trait OsppSession` (`pair`, `connect` -> `subscribe`, `dump`, `unsubscribe`, `close`; prod `WsOsppClient`), `DeviceKeySigner` + `DeviceKeyStore` + `DeviceKeyPath` (`SoftwareP256Signer` + `FileDeviceKeyStore` signing `b"ospp-v1" \|\| server_nonce \|\| client_nonce` on auth and `b"ospp-v1-pair" \|\| server_nonce \|\| client_nonce` on ALTCHA+PoP pairing), server-supplied `<url>#handle=<handle>` fragment-only `LivePairingUrl` + env-free `BrowserOpener` (`open_pairing_url` + `open_origin`), verbatim Git `remote` wire identity, uniform ID-only `OsppClaims`, per-subscription **in-memory** ephemeral `hpke-p256` keypair (`S=` zeroized on `Drop`; RFC 9180 Base `info = "ospp-v1-secret"`, `aad = "<component>:<env_key>"`), and out-of-band HTTPS stateful `hpke-p256` streaming dump reader (`SetupBaseR` recipient context, `info = "ospp-v1-dump"`, `aad = "<subscription_id>:<resource>"`, `len = 0` sealed EOF) |
| `crates/slipfork-engine` | M1 + M2 (`L0` rootfs + `crun`) + **M3 target state (`M3.20`, `M3.12`–`M3.14`, `M3.21`, `M3.28.4`, `M3.29`, `M3.2`, `ADR-0010`)** | M3 target state: `unsafe` confined strictly to two audited kernel modules — **`src/btrfs.rs`** (`M3.12` `KernelBtrfsDriver` per `ADR-0010` §§3–5) and **`src/linux_cow.rs`** (`M3.21`: `current_uid_gid`, `signal_process`, and `SCM_RIGHTS` `send_fd`/`recv_fd`), each with a per-call `// SAFETY:` proof (zero `pty.rs`, zero per-file extent-clone helpers, zero test Cargo features per §1); `#![forbid(unsafe_code)]` everywhere else (`src/btrfs_engine.rs`, `src/rootfs/`, `src/bin/slipfork-world-init.rs`) | `WorldEngine` (`M3.20` distinct RO snapshot vs writable clone signatures + `ensure_base` + `M3.29` `update_service_env` + `M3.28.4` `world_processes` & `world_lineage` + Phase 2 `M3.2` `prune_snapshots`; zero mode flags, zero capability boolean bags), `SubvolumeDriver` (`M3.12` sole CoW & Btrfs probe seam: `KernelBtrfsDriver` + `BTRFS_MOUNT_HINT` + `BtrfsProbeReport` + `btrfs::probe_with(&dyn SubvolumeDriver, ...)` in prod; test-only `TestTreeDriver` per §1), `WorldRef` + `EngineError` + `userns_remediation_hint` (§3.3), `BaseKey` (`derive` + `snapshot_id`) + clean-only `BaseSeedSpec` + `WorldSpec` (`world` + `control_dir` + `state_subdirs`), `ServiceSpec` / `ServiceKind` / `ServiceState` / `WorldServiceInfo` / `WorldLineage` / `ReadinessProbe` / `ServiceRuntime` (`ADR-0002` §2, `M3.28.4`), opaque `WorldHandle`, `#![forbid(unsafe_code)]` `src/bin/slipfork-world-init.rs` (see §3.3 & `ADR-0010` §6), `src/rootfs/` (`WorldRootfs` & `OciRootfs` returning cache outcome + `millis` alongside `crun_bin = /.slipfork/bin/crun` and `readiness: ReadinessProbe`, `trait BaseImageBuilder` [`materialize` + Phase 2 `M3.2` `prune_cache(&CacheKeepSet, dry_run)` -> `CachePruneReport`] + prod `DockerfileStageBuilder` stopping at plain `cache/{rootfs,oci}` trees, 6-rule World Image Contract — `ADR-0009` §1–§3 is normative; test-only `FakeBaseImageBuilder` per §1), and M3 sole production adapter `BtrfsEngine` (`M3.13`, `ADR-0002`, `ADR-0010` §1–§2: implements `WorldEngine::ensure_base`, `update_service_env`, `world_processes`, and `world_lineage` stamping `uptime_secs` and `millis` from its injected monotonic clock) |
| `crates/slipfork-edge` | M2 (`M2.8`–`M2.10`, shipped in `0.1.0`) + `0.1.0` UX (`M3.28.3`–`M3.28.6`, `M3.28.15`, `M3.28.20`) + **`M3.29` target state** | `#![forbid(unsafe_code)]` (uses safe `listenfd` and `httparse`); depends on `slipfork-world`, `slipfork-engine` (zero `slipfork-snow` dep); `std::env` confined strictly to `activation.rs` (`LISTEN_PID` / `LISTEN_FDS`) | Unprivileged `slipforkd` HTTP/WS ingress (`adopt_systemd_listener`, `--listen`, `<state_dir>/http-ingress.sock` [§3.3], non-template `UnitSpec` renderer with `User=<developer-user>`, `ReadWritePaths=`, `RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX AF_NETLINK` [`AF_NETLINK` required for `slipfork-world-init`'s `ip link set lo up` in-netns loopback bring-up, `M3.28.20`], and `M3.28.6` `Requires=slipforkd.socket` + `After=slipforkd.socket` + `WantedBy=multi-user.target`, `httparse` `<= 16 KiB` cap, `ADR-0008` §5, `ADR-0011` §1): `WorldRegistry` (`mint_origin`, `attach_world`, `release_world`; `Local` `.localhost` routing only, one origin kind — tombstone in `ADR-0011`), `IngressRoute` -> `dial_world_tcp`, `trait SecretVault` (`get`/`put`/`remove`/`keys` — sole local-vault port injected into `DaemonService`, feeding `OsppClaims`), and `DaemonControl` (`<runtime_dir>/daemon.sock` `0600`: `DaemonStatus { pid, ingress: IngressInfo, forward_port, worlds }`, `WorldSummary`, `WorldUpSummary`, `WorldProcessReport`, `WorldServiceSummary`, `restart`, `world_processes`, local-vault `secret_set`/`secret_unset`/`secret_list`, `auth_pair`, `shutdown`) |
| `crates/slipfork-vcs` | M2 (`M2.11`–`M2.12`) + `0.1.0` UX (`M3.28.5`, `M3.29` target state) | `#![forbid(unsafe_code)]`; depends only on `slipfork-world`, `serde`, `serde_json`, `thiserror` (zero `slipfork-dag` dep); zero `src/bin/*` targets | `ProjectSlugOverride` (injected `<vcs_root>/slipfork.toml` `project` reader), `RepoRootResolver` (`RepoRoot` + verbatim Git `origin` remote discovery for OSPP `subscribe`), `VcsWorkspace` (`WorkspaceRefs`, `WorkspaceChanges`), `VcsWorkspaceFactory` (`create_workspace` + `current_branch`, `WorkspaceRequest`), `sanitize_fork_name`, `GitWorktreeAdapter` (`.worktrees/<fork>`, `refs/heads/<fork>`, and D/F-safe `refs/slipfork/<fork>/wip` via `.slipfork/git/index-<fork>` with pinned `-c user.name=slipfork -c user.email=slipfork@localhost`), `JjWorkspaceAdapter` (`@-` -> `refs/heads/<fork>`, `@` -> `refs/slipfork/<fork>/wip` with pinned `slipfork` identity), single-line `VcsError` formatting, and pure `remote_helper` (`git-remote-slipfork` line protocol over `BufRead`/`Write`) |
| `crates/slipfork-test` | **M3 Phase 2 target state (`M3.2`, `ADR-0006` §3)** | `#![forbid(unsafe_code)]`; depends on `slipfork-world`, `slipfork-dag`, `slipfork-edge`, `slipfork-engine`; injected `EntropySource` + `Arc<dyn SubvolumeDriver>` | Language-blind **`STP v1` DAG orchestrator** over inherited `SLIPFORK_PROTOCOL_FD` (`STP v1` wire framing in `ADR-0006` §3): orchestrates global DAG nodes including persistent setup/fixture stage nodes frozen into RO Btrfs subvolume snapshots (`ADR-0010` §3), protocol-native coverage & mutation testing (`mutants`, zero hardcoded per-language Rust drivers, zero magic runtime guessing), `trait ArtifactStore` (`ensure_artifact` + `prune_artifacts`, §3.6) + `BtrfsArtifactStore` (sealing `[artifacts.*]` RO subvolumes via `Arc<dyn SubvolumeDriver>`), surviving `SLIPFORK_OUTPUT_DIR`, and Btrfs-warm `TestSlot` worlds (`ResourceMode::Test`) |
| `crates/slipfork` | M1..M4 (`M2.16`–`M2.17` shipped; **M3 `0.1.0` `M3.15`, `M3.28` & `M3.29` target state; Phase 2 `M3.2`**) | `#![forbid(unsafe_code)]`; **`slipforkd` is sole stateful Rust Composition Root; `slipfork` is thin client** | Binaries `slipforkd` (`--state-dir` + `--runtime-dir` unified user daemon Composition Root assembling `DaemonService: DaemonControl`, injecting one `OsppEndpoint` authority, wiring `FileVaultProvider` behind `trait SecretVault` [no concrete vault outside the Composition Root], owning live `OsppSession` subscriptions, deriving ID-only `claims` from `SecretVault::keys` + local resources/domains, decrypting `hpke-p256` secrets in memory, applying `env_updated` via `WorldEngine::update_service_env` + `M3.28.17`'s shared boot/restart pipeline, and owning streaming `OsppSession::dump -> zstd -d \| pg_restore -> freeze_world` dump restore — remote OSPP secrets and plaintext dumps never touch disk [`M3.29`, `docs/design/ospp.md`], provisioning `runtime_assets` [`ADR-0009` §1], constructing `SystemBrowserOpener` [`src/browser.rs`] and `BtrfsEngine::with_driver_and_clock(..., Arc::new(KernelBtrfsDriver::new()), clock)` [`M3.15`, failing closed with magic-only `EngineError::BtrfsRequired` at boot before any world directory exists, `ADR-0010` §4–§5], injecting `WorldSpec.world` + `WorldSpec.control_dir` + `state_subdirs` and clean `BaseSeedSpec` [`ADR-0010` §2], and wiring `DockerfileStageBuilder` -> `WorldRootfs` -> `dyn WorldEngine` with `--host-caches`, `ADR-0009`), `slipfork` (thin `clap` defaulting bare `slipfork` to `status` [`M3.28.7`] + `UserIdentity`/`HostIdentity` [`crates/slipfork::user`, `invoking_account`] + `Layout::for_account` / `Layout::resolve` [`<state_dir>/run` sole default runtime dir, `ADR-0011` §2] + the shipped private `daemon::resolve_world_selector -> WorldTarget` parser [`ADR-0005` §3: `<project>`, `<project>:<fork>`, or `<fork>`]; typed `WorldId` plus a shared `project::` location is `0.2.0` target state) + `DaemonControlClient` + auto-spawner + `status`/`restart`/`ps`/`open` [`SystemBrowserOpener`]/`fork [FORK]`/`secret`/`auth <URL>` + `daemon unit` [flat repeatable `--bin-dir`] & `daemon install` [`UnitInstaller` / `OsUnitInstaller`, `M3.28.6`, `ADR-0011` §1] + `doctor::run(&dyn SubvolumeDriver, &dyn HostProbe, &dyn IngressProbe, ...)` wired in `main.rs` over `KernelBtrfsDriver` + `OsHostProbe` + `SystemdIngressProbe` [`M3.15`, `M3.28.6`, `ADR-0010` §4, `ADR-0011` §3] + direct `WorldEntryCommand` `Stdio::inherit()` executor; `HOME`-unset fail-closed guard), and `git-remote-slipfork` |
| `packages/opencode-slipfork` | M1 (shipped) + M2 e2e (`M2.18`) | Type-only `@opencode/plugin`; injected `SlipforkProjectState` + `SlipforkGateway` (zero `existsSync` inside gateway in M2) | OpenCode V2 `worktree.transform`, `shell.hook("create.before")` (routes to `slipfork exec --cwd <cwd>`, never injects `snow.yml` envs), and `session.hook("context")` |

---

## 3. Core Trait Contracts & Wire Protocols

### 3.1 Two-Tier Secret Model: `SecretVault` (`slipfork-edge`) + Read/Ensure `SecretProvider` (`slipfork-snow`) vs In-Memory Remote OSPP Secrets (`ADR-0001`, `ADR-0005`, `ADR-0008`, `docs/design/ospp.md`)

```rust
// crates/slipfork-snow — narrow read/ensure resolver input passed into SnowFile::resolve / render_template
pub trait SecretProvider: Send + Sync {
    fn get(&self, namespace: Namespace, key: &str) -> Result<Option<String>, ProviderError>;
    /// Persist `proposed` under `secrets::<key>` in the local vault iff absent (`put_if_absent` / CAS).
    fn ensure_secret(&self, key: &str, proposed: &str) -> Result<String, ProviderError>;
}

pub trait EntropySource: Send + Sync {
    fn fill_bytes(&self, buf: &mut [u8]) -> Result<(), ProviderError>;
}

/// Sole disk secret writer (`<data_home>/slipfork/vaults/<project>.json`, `0600`, `flock` + `O_EXCL` temp + atomic `rename`),
/// constructed ONLY at `slipforkd`'s Composition Root (`crates/slipfork`): holds ONLY locally-provisioned
/// backing-service credentials, local `slipfork secret` overrides, and local-mode `!!` secrets.
pub struct FileVaultProvider { /* private vault path; redacting Debug */ }

// crates/slipfork-edge (`M3.29` target state, zero `slipfork-snow` dep) — capability-honest local vault port
// injected into `DaemonService` for `DaemonControl::secret_{set,unset,list}` AND `OsppClaims` key enumeration:
pub trait SecretVault: Send + Sync {
    fn get(&self, project: &ProjectSlug, key: &str) -> Result<Option<String>, EdgeError>;
    fn put(&self, project: &ProjectSlug, key: &str, value: &str, force: bool) -> Result<(), EdgeError>;
    fn remove(&self, project: &ProjectSlug, key: &str) -> Result<bool, EdgeError>;
    /// Sole source of local vault secret/var IDs feeding `OsppClaims` on `subscribe` (IDs only, never values).
    fn keys(&self, project: &ProjectSlug) -> Result<Vec<String>, EdgeError>;
}
```

| Tier | Backing Store & Lifetime | Scope & Wire Rule |
|---|---|---|
| **1. Local vault** (`trait SecretVault` -> `FileVaultProvider`, `0600` disk file) | `<data_home>/slipfork/vaults/<project>.json` (`0600`), surviving `slipforkd` and host restarts | Locally-provisioned backing-service credentials (local PostgreSQL/Redis passwords that must reconnect to persistent Btrfs world state), `slipfork secret set\|unset\|list` overrides, and local-mode `!!` secrets; **`SecretVault::keys(project)` automatically populates OSPP `subscribe.claims`** (IDs only, never values) |
| **2. Remote OSPP secrets** (`OsppSession`, memory only) | Ephemeral `hpke-p256` decryption **strictly in `slipforkd` memory** (`ExecEnv`) | Unclaimed `snow.yml` secrets and sensitive remote resource credentials returned in `env_resolved`/`env_updated` (`{ "encrypted": "<base64>" }`) plus encrypted `dump_ready` HTTPS streams — **never written to `FileVaultProvider` or any file on disk** |

- **Two-Tier Boundary, Single `SecretVault` Port & Template-Free Unclaimed Guarantee (`M3.29`, `docs/design/ospp.md`):** `DaemonService` holds only `Arc<dyn SecretVault>` (wired over `FileVaultProvider` solely at `slipforkd`'s Composition Root — zero concrete provider outside `crates/slipfork`), keeping `SecretProvider` as the narrow read/ensure input to `SnowFile` evaluation, and **remote OSPP secrets never touch the local vault or any file on disk**. `SecretVault::keys(project)` plus local `!!` entries and locally-provisioned `resources`/`domains`/`components` automatically feed `OsppClaims` on `subscribe` ("do not resolve these; leave `${{ ... }}` untouched"). Per `docs/design/ospp.md`, **unclaimed** entries are guaranteed template-free plaintext (the server resolves them completely or refuses with `error`), whereas **claimed** entries keep `${{ ... }}` and are substituted locally solely through `SnowFile` / `render_template` fed by the local `SecretProvider` (backed by `SecretVault`) + local `ResourceCoordinates` — `slipforkd` never re-scans server-resolved unclaimed outputs for templates. **OSPP is never a prerequisite for local worlds** (`ADR-0005` §1).
- **Redaction, Forbidden Keys & Mode-Scoped Components (`M2.4`):** Top-level `envs:` and `project:` return `SnowError::ForbiddenTopLevelKey { key }` (`M2.4`, key name only, never values); `SecretValue`, `S3Coordinates`, and `ResourceCoordinates` implement redacting `Debug`; `SnowFile::effective_components(mode)` implements `ADR-0001` §1's single add/remove/override contract.
- **Origin-Aware `ResourceCoordinates`:** `ResourceCoordinates` sets `app_url`/`app_domain` strictly from the world's untagged `WorldOrigin` (`WorldOrigin::local(&route, ingress_port)`, overriding provider `domains::` in local worlds; one origin kind — tombstone in `ADR-0011`) and substitutes world-local `pg_databases` (`127.0.0.1:5432`), `redis_clusters` (`127.0.0.1:6379`), and `Local`-strategy `s3_buckets` (`127.0.0.1:9000` SeaweedFS) for claimed `${{ ... }}` expressions in memory (zero `slipfork-dag` dependency in `slipfork-snow`).

### 3.2 `OSPP v1` Capability Traits (`OsppClient` / `OsppSession`), Single `OsppEndpoint`, Per-Subscription In-Memory `hpke-p256` & `ResourcePlan` (`crates/slipfork-ospp`, `crates/slipfork-dag`, `M3.29` target state, `docs/design/ospp.md`, `ADR-0008`)

> **Single Normative Wire Specification (`docs/design/ospp.md`):** **`docs/design/ospp.md` is the sole normative OSPP v1 wire specification** for `0.1.0`; this document defines the Rust DI ports and lifecycle invariants and does **not** maintain a second frame table. Every WebSocket message on `Sec-WebSocket-Protocol: ospp-v1` is a UTF-8 JSON text frame discriminated by `"type"` (`hello` / `auth_challenge` / `auth_response` / `auth_ok`; `pair_hello` / `pair_challenge` / `pair_begin` / `pair_handle` / `pair_approved`; `subscribe` / `env_resolved` / `env_updated` / `unsubscribe`; `dump_request` / `dump_ready`; and `error`), parsed into closed Rust structs (`#[serde(tag = "type", rename_all = "snake_case")]`) with **zero** `#[serde(flatten)]` catch-all, **zero** `extra: Map`, **zero** `match wire_version`, and **zero** binary WebSocket frames. Unknown frame types or unsupported `auth_methods` / `encryption_methods` fail closed (`OsppError::Protocol`) and close the socket. **"Remote resource offer" is not a concept in OSPP v1:** remote resource coordinates arrive directly as component environment values in `env_resolved` / `env_updated`, and remote resource dumps use the explicit `dump_request` / `dump_ready` path (`OsppSession::dump`).

```rust
// crates/slipfork-dag — [resources] & single ResourcePlan::resolve owner (M3.29 target state; zero slipfork-snow dep)
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
    PreferRemote, // use OSPP when paired (unclaimed S3 env + explicit dump_request), else Local with [resources] diagnostic
    Remote,       // require paired OSPP session; fail closed if unpaired/offline or server refuses
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DumpPolicy {
    Default,     // governed by effective ResourceStrategy
    NoDump,      // --no-dump (mutually exclusive with --refresh-dump): clean initdb
    RefreshDump, // --refresh-dump: require live OSPP dump_request; fail closed if unpaired/offline or Local
}

impl ResourcePlan {
    /// Sole owner of S3 + PG strategy resolution consumed by `slipforkd` (`up`/`fork`/`restart`) and M3 `crates/slipfork-test`:
    /// resolves strictly from `ResourcesSpec`, `ResourceMode`, `DumpPolicy`, and whether an OSPP session is paired
    /// (feeding local `claims` + local `ResourceCoordinates` vs unclaimed remote env + explicit `OsppSession::dump`).
    pub fn resolve(
        spec: &ResourcesSpec,
        mode: ResourceMode,
        dump_policy: DumpPolicy,
        ospp_paired: bool,
    ) -> Result<Self, DagError>;
}
```

```rust
// crates/slipfork-ospp (M3.29.2 target state) — clean-sheet OSPP v1 sync client (`tungstenite` + `rustls` + `webpki-roots`, `p256`, `hpke` 0.15.1, `zstd`)
pub struct OsppEndpoint(/* validated wss:// endpoint URL, or ws:// strictly on loopback; single server authority injected at slipforkd boot */);
pub struct DeviceKeyPath(pub PathBuf); // injected `0600` machine-auth key path owned at slipforkd Composition Root
pub struct DeviceSignature([u8; 64]);  // raw fixed-width ECDSA P-256 r || s (never ASN.1 DER)
pub struct SoftwareP256Key(/* private 32-byte machine-auth scalar, zeroized on Drop, redacted Debug */);

pub trait DeviceKeyStore: Send + Sync {
    fn load_or_create_p256(&self, entropy: &dyn EntropySource) -> Result<SoftwareP256Key, OsppError>;
}

/// Machine authentication key port (`p256` auth & pairing only — never used for secret encryption):
/// `public_key_hex()` is the compressed SEC1 33-byte P-256 public key (66 lowercase hex digits);
/// `sign()` computes ECDSA P-256 + SHA-256 over `b"ospp-v1" || server_nonce || client_nonce` (`auth_response`)
/// or `b"ospp-v1-pair" || server_nonce || client_nonce` (`pair_begin`).
pub trait DeviceKeySigner: Send + Sync {
    fn public_key_hex(&self) -> &str;
    fn sign(&self, message: &[u8]) -> Result<DeviceSignature, OsppError>;
}

/// Uniform ID-only claim set across all `snow.yml` namespaces (`docs/design/ospp.md` §Environment Resolution):
/// carries identifier names ONLY (never secret or variable values), derived from `SecretVault::keys(project)` +
/// locally-provisioned `resources`/`domains`/`components`. Any claimed entry is left unresolved by the server with
/// its `${{ ... }}` expression preserved verbatim as text; everything unclaimed is resolved by the server to
/// template-free plaintext (or refused with `error`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OsppClaims {
    pub vars: Vec<String>,
    pub secrets: Vec<String>,
    pub resources: BTreeMap<String, Vec<String>>, // e.g. "pg_databases" -> ["main"], "s3_buckets" -> ["media"]
    pub domains: Vec<String>,
    pub components: Vec<String>,
}

pub struct SubscribeRequest {
    pub id: String,          // stable per-project subscription id (re-used on config/claims update)
    pub remote: String,      // verbatim Git `remote` string ONLY (zero slug, hash, mode, or canonicalization)
    pub snow: String,        // raw `snow.yml` content
    pub claims: OsppClaims,  // ID-only local claims
}

/// Decrypted per-component environment delivered by `env_resolved` or server-pushed `env_updated`:
/// `{ "encrypted": "<base64>" }` values have already been opened in memory using the subscription's
/// ephemeral `hpke-p256` private key (`S=`), leaving server-resolved template-free plaintexts and claimed
/// `${{ ... }}` templates ready for `slipforkd`'s local substitution (custom `Debug` redacts every value).
pub struct SubscriptionEnv {
    pub id: String,
    pub components: BTreeMap<String, BTreeMap<String, SecretValue>>,
}

pub struct PairingApproval {
    pub public_key: String,
    pub account: String,
    pub machine: String,
}

/// Neutral browser-launch port (`crates/slipfork-ospp/src/pairing.rs`, keeping `slipfork-ospp` 100% env-free);
/// sole production implementation `SystemBrowserOpener` lives in `crates/slipfork/src/browser.rs` and is
/// constructed from resolved env inputs by both `crates/slipfork/src/main.rs` and `crates/slipfork/src/bin/slipforkd.rs`.
pub trait BrowserOpener: Send + Sync {
    /// Open `<url>#handle=<handle>` in the developer's browser (`LivePairingUrl` appends `#handle=` to the
    /// server-supplied `pair_handle.url` — fragment-only, never computed from hostname — and zeroizes on `Drop`).
    fn open_pairing_url(&self, url: LivePairingUrl) -> Result<(), OsppError>;
    /// Open a world's validated `WorldOrigin` (`slipfork open`, `M3.28.9`); headless sessions fail closed
    /// ending with the actionable line naming `slipfork open --remote`.
    fn open_origin(&self, origin: &WorldOrigin) -> Result<(), OsppError>;
}

/// Single client-side OSPP v1 adapter port injected into `DaemonService` (`WsOsppClient` in prod; fake in tests),
/// bound to `slipforkd`'s single boot-injected `OsppEndpoint`.
pub trait OsppClient: Send + Sync {
    fn endpoint(&self) -> &OsppEndpoint;

    /// Execute `pair_hello -> pair_challenge -> pair_begin -> pair_handle -> pair_approved` (`docs/design/ospp.md` §Device Pairing)
    /// against `self.endpoint()`: solves ALTCHA PoW, signs `b"ospp-v1-pair" || server_nonce || client_nonce`,
    /// delivers the server-supplied `<url>#handle=<handle>` fragment URL to `on_handle` while keeping the socket open,
    /// and awaits `pair_approved`.
    fn pair(
        &self,
        hostname: &str,
        on_handle: &mut dyn FnMut(&LivePairingUrl) -> Result<(), OsppError>,
    ) -> Result<PairingApproval, OsppError>;

    /// Open an authenticated `hello -> auth_challenge -> auth_response -> auth_ok` persistent session against
    /// `self.endpoint()`, dispatching server-pushed `env_updated` frames (HPKE-decrypted in memory) to `on_update`.
    fn connect(
        &self,
        on_update: Box<dyn Fn(SubscriptionEnv) + Send + Sync>,
    ) -> Result<Box<dyn OsppSession>, OsppError>;
}

/// Live authenticated `ospp-v1` session owned exclusively by `slipforkd` (never the CLI).
/// Generates and holds one ephemeral `hpke-p256` private scalar (`S=`) **in memory per subscription**
/// (never written to disk or env; zeroized on `unsubscribe`, `close`, or `Drop`).
pub trait OsppSession: Send + Sync {
    fn subscribe(&self, req: &SubscribeRequest) -> Result<SubscriptionEnv, OsppError>;
    /// Send `dump_request { id, subscription_id, resource }`, await `dump_ready { id, url }`, and open an
    /// out-of-band `HTTPS GET` stream decoded on the fly with RFC 9180 recipient context `SetupBaseR`
    /// (`info = "ospp-v1-dump"`, `aad = "<subscription_id>:<resource>"`, failing closed on truncation or missing `len = 0` EOF frame).
    fn dump(&self, subscription_id: &str, resource: &str) -> Result<Box<dyn std::io::Read + Send>, OsppError>;
    fn unsubscribe(&self, subscription_id: &str) -> Result<(), OsppError>;
    fn close(&self) -> Result<(), OsppError>;
}
```

- **Single Boot-Injected `OsppEndpoint` Authority:** One `OsppEndpoint` is injected into `slipforkd` at boot for `connect`, `subscribe`, and `dump`; `DaemonControl::auth_pair` **refuses** (fails closed) any CLI-supplied `<URL>` that differs from `slipforkd`'s injected `OsppEndpoint` — one server identity per daemon, never two.
- **Verbatim Git `remote` Wire Identity (Local `ProjectSlug` Never Crosses the Wire):** `SubscribeRequest.remote` carries the repository's **verbatim Git `remote` string** (e.g. `"git@github.com:acme/web.git"`), resolved through `slipforkd`'s injected `RepoRootResolver` port with **zero** client-side slugging, hashing, mode tuple, or URL canonicalization; `ProjectSlug` (`crates/slipfork-world`) remains strictly a local world/routing identifier.
- **Per-Subscription In-Memory `hpke-p256` Encryption (`ospp-v1-secret` & `ospp-v1-dump`, Zero OSPP Secrets or Plaintext Dumps on Disk):**
  - **Ephemeral recipient keypair (`S=` in memory only):** On every `subscribe`, `OsppSession` draws a fresh ephemeral P-256 keypair from `EntropySource` in memory, sends `"encryption": { "method": "hpke-p256", "public_key": "<hex>" }`, retains the private scalar `S=` solely in memory for the lifetime of that subscription, and zeroizes `S=` on `unsubscribe`, `close`, or `Drop`.
  - **Per-variable secret sealing (`info = "ospp-v1-secret"`):** Any component variable whose server evaluation touched an unclaimed `secrets::` key or sensitive server-resolved resource credential arrives as `{ "encrypted": "<base64>" }`, sealed via RFC 9180 HPKE Base mode (`DHKEM(P-256, HKDF-SHA256)`, `HKDF-SHA256`, `AES-256-GCM`, `info = "ospp-v1-secret"`, `aad = "<component>:<env_key>"`, RFC 4648 base64 `enc || ciphertext`). Opening binds `aad = "<component>:<env_key>"` so ciphertexts cannot be swapped across components or variable names.
  - **Daemon-owned out-of-band streaming dump restore (`info = "ospp-v1-dump"`, `SetupBaseR` on client):** `slipforkd` owns dump restore end-to-end (`WorldEngine` has zero `slipfork-ospp` dependency and sees only a sealed base snapshot). `OsppSession::dump` issues `dump_request { id, subscription_id, resource }`, receives `dump_ready { id, url }`, streams `HTTPS GET <url>`, and opens the server's `SetupBaseS`-sealed stream on the client with RFC 9180 stateful recipient context `SetupBaseR` (`info = "ospp-v1-dump"`, `65`-byte uncompressed P-256 `enc` header + `4-byte big-endian plaintext length` + `ctx.Open(aad = "<subscription_id>:<resource>", ciphertext)` frames + mandatory empty sealed chunk `len = 0` EOF) while streaming directly into bounded `zstd -d | pg_restore` inside the transient seed world's running `postgres:<v>` `crun` container (`CHECKPOINT;` -> `WorldEngine::freeze_world`), writing **zero plaintext dump bytes to disk**.
- **Daemon-Owned Live Subscription Lifecycle, `WorldEngine::update_service_env` & CLI Isolation:** `slipforkd` keeps its `OsppSession` WebSocket open while worlds run. When the server pushes `env_updated`, `slipforkd` decrypts remote `{ "encrypted": "<base64>" }` values in memory, substitutes only claimed `${{ ... }}` expressions through the local `SecretProvider` (`SecretVault`) + local `ResourceCoordinates` (never writing remote OSPP secrets to disk and never re-scanning unclaimed plaintexts for templates), updates affected services via `WorldEngine::update_service_env(&ServiceHandle, &ExecEnv)` (so `DaemonService` never re-authors `ServiceSpec`), and restarts through `M3.28.17`'s single shared boot/restart pipeline. Editing `snow.yml`, mutating the local vault (`slipfork secret`), or changing local `claims` (`slipfork restart`) re-sends `subscribe` with the **same `id`**; stopping the project's last running world (`down` / `drop` / `shutdown`) sends `unsubscribe { id }`; and an unexpected socket drop or `error` frame fails closed with an actionable diagnostic rather than silently falling back to unauthenticated secrets. The thin `slipfork` CLI never owns `OsppClient`, `SecretVault`, `SecretProvider`, or any HPKE key.

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

/// Advisory `:80` socket-activation state on `DoctorReport` (`M3.28.6`, `ADR-0011` §3; never affects `DoctorReport.ok` or `fixes`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngressCheck {
    ActivePort80 { listen: Vec<String> }, // `:80 active (port-less origins)`
    Inactive,                             // `installed but inactive (run `sudo systemctl start slipforkd.socket`)`
    NotInstalled,                         // `not installed (ephemeral port; run `slipfork daemon install`)`
}

/// Single `doctor` report owner (`crates/slipfork::doctor`, M3.15 + M3.28.6); text & flattened `--json` projection rules in `ADR-0010` §4 & `ADR-0011` §3.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorReport {
    pub ok: bool,
    pub uid: u32,
    pub gid: u32,
    pub btrfs: BtrfsProbeReport,       // fields: ADR-0010 §4
    pub filesystem: String,            // mapped once from `btrfs.fstype` (`ADR-0010` §4)
    pub unshare_userns: HostCheckOutcome,
    pub netns: NetnsCheck,
    pub ingress: IngressCheck,         // advisory `:80` unit probe (`M3.28.6`, `ADR-0011` §3)
    pub fixes: Vec<String>,            // emission order & `ok` invariant: `ADR-0010` §4
}

/// Sole pre-daemon namespace probe seam (`OsHostProbe` constructed in `crates/slipfork/src/main.rs`,
/// injected into `doctor::run` only — never `slipforkd` boot; `ADR-0010` §4).
pub trait HostProbe: Send + Sync {
    fn probe_userns(&self, uid: u32, gid: u32) -> HostCheckOutcome; // fixed PATH + LC_ALL=C unshare --user; EPERM -> userns_remediation_hint
    fn probe_netns(&self, uid: u32, gid: u32) -> HostCheckOutcome;  // fixed PATH + LC_ALL=C unshare ... --net --ipc --pid -- ip link set lo up
}

/// Sole pre-daemon `:80` systemd unit probe seam (`SystemdIngressProbe` constructed in `crates/slipfork/src/main.rs`,
/// fixed `PATH` + `env_clear` + `LC_ALL=C` over `systemctl is-active` / `systemctl show -p Listen`; `M3.28.6`, `ADR-0011` §3).
pub trait IngressProbe: Send + Sync {
    fn probe_ingress(&self) -> IngressCheck;
}

/// Pure `doctor` composer (`crates/slipfork::doctor`, M3.15 + M3.28.6): calls `btrfs::probe_with(driver, state_dir)` +
/// `host.probe_userns` (and `NetnsCheck::Executed(host.probe_netns)` iff `userns == Ok`, else `NetnsCheck::Skipped`) +
/// `ingress.probe_ingress()` -> `DoctorReport`.
pub fn run(
    driver: &dyn SubvolumeDriver,
    host: &dyn HostProbe,
    ingress: &dyn IngressProbe,
    state_dir: &Path,
    uid: u32,
    gid: u32,
) -> Result<DoctorReport, EngineError>;

// crates/slipfork::{user, layout, daemon_unit_install} (`#![forbid(unsafe_code)]`, M3.28.6 & M3.28.9; ADR-0011 §§1–2)
pub struct ResolvedAccount { pub name: String, pub uid: u32, pub gid: u32, pub home: PathBuf }

/// Single OS-account & hostname adapter family (`crates/slipfork/src/user.rs`, fixed `PATH`, `env_clear`, zero `$USER`/`$HOSTNAME` reads).
pub trait UserIdentity: Send + Sync {
    fn username(&self) -> Result<String, String>;
    fn lookup_account(&self, account: &str) -> Result<ResolvedAccount, String>; // rejects unknown accounts, `%`, and `uid == 0`
    /// Sole reader of `SUDO_USER` (under the adapter's fixed `PATH`), validated through `lookup_account`;
    /// nothing outside `crates/slipfork/src/user.rs` touches `SUDO_USER`.
    fn invoking_account(&self) -> Result<Option<String>, String>;
}

pub trait HostIdentity: Send + Sync {
    fn hostname(&self) -> Result<String, String>; // feeds `slipfork open --remote` SSH target (`M3.28.9`)
}

impl Layout {
    /// Pure single-source layout derivation for any `ResolvedAccount` (`runtime_dir` defaults unconditionally to `<state_dir>/run`).
    pub fn for_account(
        account: &ResolvedAccount,
        state_dir: Option<PathBuf>,
        runtime_dir: Option<PathBuf>,
        xdg_cache_home: Option<&Path>,
        xdg_data_home: Option<&Path>,
    ) -> Self;
    /// Resolves the invoking account + delegates to `Layout::for_account` (never a second layout formula).
    pub fn resolve(/* invoking account + `--state-dir` + XDG cache/data; no runtime-dir override */) -> Self;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedUnit { Socket, Service } // slipforkd.socket, slipforkd.service

/// Narrow filesystem + unit-management port for `slipfork daemon install` (`OsUnitInstaller` wired in `crates/slipfork/src/main.rs`;
/// pure install orchestration takes `is_root: bool` and `explicit_user: Option<&str>` — zero stringly command bag, zero `--force`; `M3.28.6`, `ADR-0011` §1).
pub trait UnitInstaller: Send + Sync {
    fn read_file(&self, path: &Path) -> Result<Option<Vec<u8>>, String>;
    fn write_file_atomic(&self, path: &Path, contents: &[u8]) -> Result<(), String>;
    fn daemon_reload(&self) -> Result<(), String>;
    fn apparmor_replace(&self, profile_path: &Path) -> Result<(), String>;
    fn enable_now(&self, units: &[ManagedUnit]) -> Result<(), String>;
    fn restart_service(&self) -> Result<(), String>;
}

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
/// `BackingServiceKey`/`OciRootfs.digest` × sorted `BaseDatabaseSeed` `(database, service)` pairs; `ADR-0010` §2, `M3.13`/`M3.29` target state).
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

/// Explicit clean base-seeding carrier injected by `slipforkd` into `WorldEngine::ensure_base`
/// (`M3.20`/`M3.13`/`M3.29` target state; never embedded in `WorldSpec`, and carries NO remote stream variant —
/// remote dump restore is owned end-to-end by `slipforkd` via `OsppSession::dump -> zstd -d | pg_restore -> freeze_world`,
/// so `WorldEngine` sees only a clean `initdb` seed or an already-sealed base snapshot).
pub struct BaseSeedSpec {
    pub control_dir: PathBuf,             // seed control dir created & cleaned up by slipforkd (`ADR-0010` §2)
    pub project: ProjectSlug,
    pub base_key: BaseKey,                // typed base key; `snapshot_id()` names the sealed base slot (`ADR-0010` §2)
    pub rootfs: RootfsDigest,
    pub app_rootfs: PathBuf,              // <state_dir>/cache/rootfs/<l0_merkle_sha256>/app
    pub state_subdirs: Vec<PathBuf>,      // cloned from the single `slipforkd`-authored list passed to `WorldSpec.state_subdirs`
    pub service_specs: Vec<ServiceSpec>,
    pub databases: Vec<BaseDatabaseSeed>,
}

pub struct BaseDatabaseSeed {
    pub database: String,
    pub service: String,
}

// Production (`M3.13`): `BtrfsEngine::with_driver_and_clock(..., Arc::new(KernelBtrfsDriver::new()), clock)` — the only constructor.
// Tests (`M3.12`/`M3.14`/`M3.15`): the same constructor receives a test-only `TestTreeDriver` per §1 and a deterministic clock.
impl BtrfsEngine {
    pub fn with_driver_and_clock(
        state_dir: PathBuf,
        host_uid: u32,
        host_gid: u32,
        world_init_bin: PathBuf,
        driver: Arc<dyn SubvolumeDriver>,
        clock: Arc<dyn MonotonicClock>,
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

// crates/slipfork-engine::rootfs (#![forbid(unsafe_code)], zero slipfork-snow/slipfork-dag dep; ADR-0009 §1–§3, M3.28.4)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OciRootfs {
    pub digest: String,            // sha256:<64 lowercase hex>
    pub rootfs: PathBuf,           // <state_dir>/cache/oci/<sha256>/rootfs ($UID:$GID, RO in crun)
    pub config: PathBuf,           // upstream image config.json (ENV/PATH)
    pub readiness: ReadinessProbe, // M3.13/M3.15 target state: passed unchanged to ServiceSpec.readiness (ADR-0002 §2)
    pub cache: CacheOutcome,        // the store's own verdict for this digest (`Hit | Miss`, `M3.28.4`)
    pub millis: Option<u64>,       // elapsed pull/extract millis when measured (`None` on `Hit`)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldRootfs {
    pub digest: RootfsDigest,                                   // sha256:<64 lowercase hex>; hex-string `Serialize`/`Deserialize` via `hex()`/`from_hex` in `M3.20` (`ADR-0009` §3)
    pub app_rootfs: PathBuf,                                    // <state_dir>/cache/rootfs/<hex>/app
    pub crun_bin: PathBuf,                                      // /.slipfork/bin/crun (static ELF)
    pub service_images: BTreeMap<BackingServiceKey, OciRootfs>, // postgres:<v>, redis:<v>, seaweedfs
    pub metadata: ImageContractMetadata,                        // os_release, elf_abi, exposed_ports
    pub host_caches: Vec<WorldMount>,                           // empty unless --host-caches ABI probe passes
    pub cache: CacheOutcome,                                    // recorded from `RootfsBuildEvent::{CacheHit, CacheMiss}` (`M3.28.4`)
    pub millis: Option<u64>,                                    // elapsed buildx millis on `Miss` (`None` on `Hit`)
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
    pub mounts: Vec<WorldMount>,                   // repo (RW) + ArtifactVolumes (RO) + M3 run dirs (RW)
    pub base_env: ExecEnv,                         // resolved at world boot from single WorldOrigin
    pub local_ingress: Option<LocalIngressBridge>, // Some when the world's single local origin needs the in-netns :80 bridge
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
        readonly_mounts: Vec<WorldMount>,    // RO in-world -> container (never plaintext DB dumps on disk)
    },
}

/// RW `$UID:$GID` backing-service state mount inside the world's unified Btrfs subvolume (`ADR-0010` §2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateSubdirMount { pub host_subdir: PathBuf, pub container_path: PathBuf }

/// Lifecycle state of one world-owned service (`crates/slipfork-engine`, serialized directly on the wire).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceState { Running, Exited, Stopped }

/// Supervised process classification derived from `ServiceRuntime` (`M3.28.3`–`M3.28.4`; zero `match engine_name`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceKind { Component, BackingService }

/// Engine-supervised process record returned by `WorldEngine::world_processes` (`M3.28.4`; deterministic registration order,
/// `uptime_secs` stamped by the engine from its injected monotonic clock — zero `Instant` on the wire, zero second daemon clock).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldServiceInfo {
    pub id: String,
    pub kind: ServiceKind,
    pub state: ServiceState,
    pub pid: Option<u32>,             // `None` when `Stopped` or `Exited`
    pub port: Option<u16>,            // serving readiness port (`None` renders `-` in human `ps`, never `0`)
    pub uptime_secs: Option<u64>,     // `None` when not `Running`
}

/// The one cache verdict per materialized tree (`hit` served from the content-addressed cache,
/// `miss` rebuilt/fetched it). `M3.28.4` collapsed the former `LineageCache`/`ServiceCache` pair
/// into this enum; the old `miss_offline` value was deleted with no production path (a host that
/// cannot fetch or build fails closed instead of recording a third verdict).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheOutcome { Hit, Miss }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootfsLineage { pub digest: String, pub cache: CacheOutcome, pub millis: Option<u64> }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseLineage { pub key: String, pub millis: Option<u64> }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeedLineage { pub digest: Option<String>, pub rebuilt: Option<bool>, pub millis: Option<u64> }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceImageLineage { pub id: String, pub cache: CacheOutcome, pub millis: Option<u64> }

/// Per-world boot lineage recorded on that world's handle from `WorldSpec.rootfs` (`WorldRootfs` + `OciRootfs`) and
/// `ensure_base` / `clone_world` measurements on the engine's injected monotonic clock (`M3.28.3`–`M3.28.4`,
/// `docs/design/dx-0.1.0-ux.md` §8.2; unknown facts stay `None` / JSON `null` and render `-` in human output, never `0`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldLineage {
    pub rootfs: Option<RootfsLineage>,
    pub base: Option<BaseLineage>,
    pub seed: Option<SeedLineage>,
    pub services: Vec<ServiceImageLineage>,
}

impl WorldLineage {
    /// Interim constructor: no engine facts recorded yet (`M3.28.4` fills them); every optional
    /// fact is `None` and `services` is empty, so a renderer prints `-`, never `0`.
    pub fn unknown() -> Self;
}

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
    /// Provided handle constructor for out-of-crate `spawn_service` impls (`Self: Sized` excludes
    /// `dyn WorldEngine`); `state` is the adapter's own supervision cell, read back by `ServiceHandle::state`.
    fn mint_service_handle(&self, world: &WorldHandle, id: &str, state: Arc<AtomicU8>) -> ServiceHandle where Self: Sized { /* provided via pub(crate) ServiceHandle::mint */ }
    /// Update a supervised service's overlay `ExecEnv` in place (`M3.29` target state for live `env_updated`,
    /// so `DaemonService` never re-authors `ServiceSpec` as a second owner before running `M3.28.17`'s shared restart path).
    fn update_service_env(&self, service: &ServiceHandle, env: &ExecEnv) -> Result<(), EngineError>;
    fn stop_service(&self, service: &ServiceHandle) -> Result<(), EngineError>;
    /// Stop EVERY service the engine supervises in `world` (components first, then containers) with the
    /// engine's bounded grace — the one owner of "what this world runs", so a `restart` never replays
    /// caller-held handles. Idempotent; destroyed/foreign `world` returns `EngineError::WorldGone`.
    fn stop_world_services(&self, world: &WorldHandle) -> Result<(), EngineError>;
    fn service_logs(&self, world: &WorldHandle, id: &str) -> Result<ServiceLogs, EngineError>;
    /// Snapshot of the world's supervised components and `crun` backing services (`M3.28.4`);
    /// destroyed or foreign `world` returns `EngineError::WorldGone`.
    fn world_processes(&self, world: &WorldHandle) -> Result<Vec<WorldServiceInfo>, EngineError>;
    /// Per-world `WorldLineage` recorded on `world`'s handle (`M3.28.4`); destroyed or foreign `world` returns `EngineError::WorldGone`.
    fn world_lineage(&self, world: &WorldHandle) -> Result<WorldLineage, EngineError>;
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

- **Btrfs Substrate, Diagnostic Seam Ownership & Unified Layout (M3 target state `M3.12`–`M3.15`, `M3.21`, `M3.28.6`; **`ADR-0010` §§2–5 & `ADR-0011` §§2–3 are normative**):** `BtrfsEngine` (`crates/slipfork-engine/src/btrfs_engine.rs`) is the sole production `WorldEngine`, constructed only via `BtrfsEngine::with_driver_and_clock(..., Arc<dyn SubvolumeDriver>, Arc<dyn MonotonicClock>)` and implementing `WorldEngine::ensure_base(&BaseSeedSpec)` (`ADR-0010` §2), `WorldEngine::world_processes` (`M3.28.4`), plus Phase 2 `M3.2` `WorldEngine::prune_snapshots` (`ADR-0010` §3). `crates/slipfork-engine::btrfs` (`SubvolumeDriver::{probe, ensure_supported}` implemented by `KernelBtrfsDriver`, `BTRFS_MOUNT_HINT`, and injected `btrfs::probe_with(&dyn SubvolumeDriver, state_dir)`) is the single owner of Btrfs host probing (`BtrfsProbeReport` & `EngineError::{BtrfsRequired, SubvolumeDestroyDenied, Ioctl}`, `ADR-0010` §4): `slipforkd` boot calls magic-only `ensure_supported`, pre-daemon `slipfork doctor` calls `doctor::run(&dyn SubvolumeDriver, &dyn HostProbe, &dyn IngressProbe, ...)` -> `DoctorReport` (running `btrfs::probe_with`'s scratch-subvolume `user_subvol_rm_allowed` round-trip iff `btrfs_super_magic == true` alongside `OsHostProbe` and `SystemdIngressProbe` in `main.rs`, `ADR-0010` §4, `ADR-0011` §3), and `destroy_subvolume` maps runtime `BTRFS_IOC_SNAP_DESTROY_V2` `EPERM` to `EngineError::SubvolumeDestroyDenied` when `doctor` was skipped. Unified single-subvolume world layout, disjoint `<state_dir>/run/daemon.sock` (`--runtime-dir`) vs `<state_dir>/http-ingress.sock` vs `WorldSpec.control_dir` (`ADR-0010` §2, `ADR-0011` §2), and the ioctl lifecycle follow `ADR-0010` §§2–5; engine-owned `ServiceSpec.readiness: ReadinessProbe` follows `ADR-0002` §2.
- **`WorldRootfs` / `BaseImageBuilder` Ownership & In-World `crun` Supervision (`ADR-0009` §1–§3):** `WorldRootfs`, `WorldRootfsRequest`, `BackingServiceKey`, `OciRootfs` (carrying `readiness: ReadinessProbe` in `M3.13`/`M3.15`, passed through unchanged to `ServiceSpec.readiness`), `ImageContractMetadata`, `CacheKeepSet` + `CachePruneReport` (`M3.2`), and `trait BaseImageBuilder` (`materialize` shipped in `0.1.0`; `prune_cache` added in Phase 2 `M3.2`) live in `#![forbid(unsafe_code)]` `crates/slipfork-engine/src/rootfs/` (zero `slipfork-snow`/`slipfork-dag`/`SubvolumeDriver` dep; `DockerfileStageBuilder` owns `<state_dir>/cache/{rootfs,oci}`). Static `crun` provisioning, staging, and in-world execution follow `ADR-0009` §1 normatively. `ServiceRuntime::OciContainer` specs flow through `spawn_service` / `stop_service` / `service_logs` alongside `WorldProcess` (`crates/slipfork-engine` owns the single `match runtime` over `WorldProcess | OciContainer` — no `ContainerRuntime` trait, no `dockerd`, no second supervisor).
- **Persistent `slipfork-world-init` PID 1, `--ipc` (`CLONE_NEWIPC`), AppArmor `userns` & `<state_dir>/http-ingress.sock` Bridge (`H-011`; **`ADR-0010` §§6–7 is normative**):** Because multithreaded `setns` into an unprivileged userns/netns fails (`EINVAL`/`EPERM`), `BtrfsEngine::clone_world` spawns `#![forbid(unsafe_code)]` `crates/slipfork-engine/src/bin/slipfork-world-init.rs` under `ADR-0010` §6's unprivileged `unshare` namespace set (including normative `--ipc` / `CLONE_NEWIPC`, `M3.13`). `slipfork-world-init` brings `lo` up, writes `0` to `/proc/sys/net/ipv4/ip_unprivileged_port_start`, mounts the read-only `L0` root + subvolume state mounts (`ADR-0010` §2) + `spec.mounts` and `pivot_root`s into it, binds `0600` pathname `AF_UNIX` `<control_dir>/netns.sock` (`spec.control_dir`) to service `dial_world_tcp(port)` via `linux_cow.rs`'s audited `sendmsg`/`recvmsg` `SCM_RIGHTS` pair (`send_fd`/`recv_fd`, `MSG_CMSG_CLOEXEC`, rejecting >1 descriptors without leaking), and — when `WorldSpec.local_ingress` is `Some` — binds `127.0.0.1:<port>` (including `:80`) inside the world netns and splices accepted streams to `slipforkd`'s `0600` `<state_dir>/http-ingress.sock`. Ubuntu 24.04 LTS AppArmor `userns` profile rendering (`slipfork daemon install`, `M3.15`, `M3.28.6`) follows `ADR-0010` §7 and `ADR-0011` §1. Across the workspace, M3 `unsafe` lives solely in `crates/slipfork-engine/src/{btrfs.rs,linux_cow.rs}` (each call carrying a `// SAFETY:` proof; `H-010`'s "zero `SCM_RIGHTS`" rule applies strictly to the CLI <-> `slipforkd` TTY boundary).

### 3.4 Port-Aware `WorldOrigin::local`, `WorldRegistry` & `DaemonControl` (`crates/slipfork-world`, `crates/slipfork-edge`, `ADR-0005`, `ADR-0008`, `H-010`)

`slipforkd` runs unprivileged (`User=<developer-user>`, empty `CapabilityBoundingSet=`, explicit `--state-dir <path> --runtime-dir <path>`). Alongside its TCP HTTP listener(s), `slipforkd` binds a `0600` pathname `AF_UNIX` HTTP ingress listener at `<state_dir>/http-ingress.sock` (`H-011` #4) serving the exact same HTTP/WS dispatch loop for in-netns `$BASE_URL` bridges. In `activation.rs` (`#![forbid(unsafe_code)]` via `listenfd`), when `LISTEN_PID` or `LISTEN_FDS` is set, `adopt_systemd_listener() -> Result<Option<ListenerSet>, EdgeError>` verifies `LISTEN_PID == getpid()`, calls `set_nonblocking(true)` immediately on every inherited socket, and assigns roles by explicit descriptor count/order (`LISTEN_FDS == 1` -> single HTTP ingress + `forward_listener: None`; `LISTEN_FDS >= 2` -> `UnitSpec`'s emitted order `127.0.0.1:80`, `[::1]:80`, `127.0.0.1:<forward_port>`; any other shape fails closed `EdgeError::ActivationBroken`, `ADR-0008` §5). When both activation env vars are absent, `--listen <addr>` binds the HTTP ingress listener **only**; a forward listener exists in `--listen` mode **only** when `--forward-port <port>` is also passed (binding `127.0.0.1:<port>`; absent both activation and `--listen`, startup fails closed `EdgeError::NoListener`). HTTP/1.1 headers are parsed via `httparse` (`<= 16 KiB` cap, bounded read timeout), routing `.localhost` requests through each world's `IngressRoute { prefix, port }` longest-prefix table (`snow.yml` `ingresses`; one origin type — untagged `struct WorldOrigin(LocalOrigin)`, tombstone in `ADR-0011`).

```rust
// crates/slipfork-world — pure identity, single WorldId grammar owner, port-aware single Local origin & <= 31-byte service label
pub const SERVICE_NAME_MAX: usize = 31; // ADR-0008 §6 is normative for <= 31-byte derivation

/// Typed identity of one routable world (`ADR-0005` §3). `TestSlot` is distinct from a
/// `UserFork("test-3")`: only the test coordinator mints slots, and `test-*` labels are reserved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorldTarget {
    Canonical,
    UserFork(String), // validated single lowercase DNS label, never `test-*`
    TestSlot(u32),    // ephemeral `slipfork test` pool slot
}

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

/// Single `0.1.0` origin type: untagged newtype over validated `LocalOrigin` (never a one-variant enum;
/// `Serialize` emits `origin.url()`, fail-closed `Deserialize` validates `http://<host>[:<port>]`).
/// Constructed solely via `WorldOrigin::local(&LocalHostRoute, ingress_port: u16)` (`H-010` #2),
/// omitting `:80` and formatting `:<port>` when `ingress_port != 80`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldOrigin(LocalOrigin);
```

```rust
// crates/slipfork-edge (M2 + M3 WorldId & M3.28 target state) — activation, unit generator, canonical WorldRegistry & <state_dir>/run/daemon.sock RPC
pub struct ListenerSet {
    pub http_listeners: Vec<std::net::TcpListener>,
    pub forward_listener: Option<std::net::TcpListener>, // None unless LISTEN_FDS >= 2 or --forward-port <port>
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngressRoute { pub prefix: String, pub port: u16 }

impl WorldRegistry {
    pub fn mint_origin(&self, route: &LocalHostRoute, ingress_port: u16) -> Result<WorldOrigin, EdgeError>;
    /// A second attach for an already-registered world fails closed with
    /// `EdgeError::WorldAlreadyRegistered`; it never replaces or leaks the previous handle.
    pub fn attach_world(
        &self,
        project: &ProjectSlug,
        target: &WorldTarget,
        snow_domains: Vec<String>,
        world: WorldHandle,
        ingresses: Vec<IngressRoute>,
    ) -> Result<(), EdgeError>;
    /// Swap an attached world's `snow.yml`-derived domains and ingress table **in place** — no
    /// handle is touched, so a `restart` re-reading `ingresses:`/`domains:`/`internal_ports` keeps
    /// the world's CoW state. The table is re-validated (`EdgeError::InvalidIngressRoute`) and an
    /// unknown world fails closed with `EdgeError::WorldNotAttached`; the world's share is
    /// untouched.
    pub fn replace_world(
        &self,
        project: &ProjectSlug,
        target: &WorldTarget,
        snow_domains: Vec<String>,
        ingresses: Vec<IngressRoute>,
    ) -> Result<(), EdgeError>;
    pub fn release_world(&self, project: &ProjectSlug, target: &WorldTarget) -> Result<bool, EdgeError>;
}

pub struct UnitSpec {
    pub user: String,                            // explicit non-empty unprivileged account name (rejects empty, "root", or '%'-template)
    pub listen_addrs: Vec<std::net::SocketAddr>, // e.g. 127.0.0.1:80, [::1]:80
    pub forward_port: u16,                       // must not collide with listen_addrs (EdgeError::UnitPortCollision)
    pub executable: PathBuf,
    pub state_dir: PathBuf,                      // passed as --state-dir & emitted into ReadWritePaths=
    pub runtime_dir: PathBuf,                    // passed as --runtime-dir (<state_dir>/run) & emitted into ReadWritePaths=
} // render_socket_unit(&UnitSpec) -> String (WantedBy=sockets.target);
  // render_service_unit(&UnitSpec) -> String (`M3.28.6` adds Requires=slipforkd.socket, After=slipforkd.socket, WantedBy=multi-user.target; ADR-0011 §1)

/// Actually bound / `LISTEN_FDS`-adopted HTTP listeners plus the one origin port (`M3.28.3`).
/// Fields are private: `IngressInfo::new(addrs)` derives `port` from the listeners and fails closed
/// (`EdgeError::IngressMissing` / `IngressPortInvalid`) on an empty set, an unbound `:0`, or
/// listeners that disagree on the port; `#[serde(try_from = "IngressInfoFrame")]` re-validates a
/// forged frame, so a daemon can never report a guessed `:0`. `addrs` order is non-semantic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "IngressInfoFrame")]
pub struct IngressInfo {
    addrs: Vec<std::net::SocketAddr>,
    port: u16,
}

impl IngressInfo {
    pub fn new(addrs: Vec<std::net::SocketAddr>) -> Result<Self, EdgeError>;
    pub fn addrs(&self) -> &[std::net::SocketAddr];
    pub fn port(&self) -> u16;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldSummary {
    pub world: String,           // canonical `<project>` / `<project>:<fork>` (`registry::world_id`);
                                 // the typed `WorldId` newtype is `0.2.0` target state (`ADR-0005` §3)
    pub parent: Option<String>,  // canonical: `None`; fork: `Some(<project>)` (`registry::parent_world`)
    pub state: WorldState,
    pub origin: WorldOrigin,
    pub worktree: PathBuf,
    pub lineage: WorldLineage,   // recorded on this world's handle only (never daemon-wide)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub pid: u32,
    pub ingress: IngressInfo,
    pub forward_port: Option<u16>,
    pub worlds: Vec<WorldSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldServiceSummary {
    pub world: String,                  // canonical world id the row belongs to
    #[serde(flatten)]
    pub info: WorldServiceInfo,         // the engine's own row (`slipfork-engine::lineage`), flattened into the same JSON object
}

// `slipfork-engine` (single model; `slipfork-edge` re-exports it for the wire):
//   pub struct WorldServiceInfo { id: String, kind: ServiceKind, state: ServiceState,
//                                 pid: Option<u32>, port: Option<u16>, uptime_secs: Option<u64> }
// `state` is the engine's `ServiceState` (`Serialize`/`Deserialize` snake_case), never `String`;
// `pid`/`port`/`uptime_secs` are `None` while the engine has no such record and render `-`, never `0`.

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldProcessReport {
    pub state: WorldState,          // the world's own state, read by the same call (`M3.28.3`; `docs/design/dx-0.1.0-ux.md` §5)
    pub lineage: WorldLineage,
    pub services: Vec<WorldServiceSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldUpSummary {
    pub world: WorldId,
    pub state: WorldState,
    pub changed: bool,              // `false` on idempotent `up` of a `Running` world; `true` on boot, `fork`, or `restart` (`M3.28.4`)
    pub restarted: Vec<String>,     // ids of this world's `services[]` that restarted (`empty` when `!changed`; ids are unique per world, `docs/design/dx-0.1.0-ux.md` §3)
    pub worktree: PathBuf,
    pub origin: WorldOrigin,
    pub lineage: WorldLineage,
    pub services: Vec<WorldServiceSummary>, // the booted world's process rows, so no second call is needed (`docs/design/dx-0.1.0-ux.md` §10)
    pub resources: Vec<ResourceSummary>,
    pub next: Vec<String>,          // this world state's `next` commands (`docs/design/dx-0.1.0-ux.md` §1)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldSelector {
    pub cwd: PathBuf,
    pub world: Option<String>,  // canonical world id; `None` resolves the nearest world from `cwd` (`ADR-0005` §3)
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
    pub fork: Option<String>,   // `None` derives via `VcsWorkspaceFactory::current_branch` + `sanitize_fork_name`, then validates `!= repo.project` and `validate_fork_name(&fork, &snow_domains)` with the checkout's real `snow.yml` `domains[].id` (`M3.28.5` / `M3.28.10`)
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
/// the single `0.1.0` formatter (`<project>` / `<project>:<fork>`); zero second formatter.
pub fn world_id(project: &ProjectSlug, target: &WorldTarget) -> String;

/// the single target -> parent relation (`None` canonical, `Some(<project>)` fork).
pub fn parent_world(project: &ProjectSlug, target: &WorldTarget) -> Option<String>;

/// Single CLI `--world` selector entry point (`crates/slipfork::project`, M3.15 target state; `ADR-0005` §3):
/// callers resolve `cwd` via `RepoRootResolver` first, then delegate here (`WorldId::parse_selector(selector, &repo.project)`).
pub fn resolve_world_selector(selector: &str, repo: &RepoRoot) -> Result<WorldId, WorldError>;

/// Typed user-local RPC contract over `<runtime_dir>/daemon.sock` (`0600`, bounded 64 KiB JSON-line frames;
/// every cwd-sensitive request carries explicit `cwd: PathBuf` directly or via `WorldSelector`; 1:1 mapping:
/// `up`/`fork` -> `mint_origin` + `attach_world`, `down`/`drop_fork` -> `release_world` with `drop_fork` refusing
/// `Canonical` daemon-side naming `slipfork down`, `prune` reaps orphaned worlds in Phase 1 and unreferenced
/// snapshots/artifacts/caches in Phase 2 `M3.2` per `ADR-0010` §3).
pub trait DaemonControl: Send + Sync {
    fn status(&self) -> Result<DaemonStatus, EdgeError>;
    /// Runs the world boot pipeline when unbooted, `Stopped`, or `Failed`; idempotent when `Running` (`changed: false`).
    fn up(&self, req: &UpRequest, on_event: &mut dyn FnMut(BootEvent)) -> Result<WorldUpSummary, EdgeError>;
    /// Stops components + `crun` backing services in place while keeping worktree & CoW state, then runs the exact
    /// same boot pipeline as `up`; `changed` is observed (true only when a service started or a hook ran), `Failed`
    /// rescues, and an unbooted/`Stopped` world fails closed naming `slipfork up`.
    fn restart(&self, req: &WorldSelector, on_event: &mut dyn FnMut(BootEvent)) -> Result<WorldUpSummary, EdgeError>;
    fn down(&self, req: &WorldSelector) -> Result<bool, EdgeError>;
    fn fork(&self, req: &ForkRequest, on_event: &mut dyn FnMut(BootEvent)) -> Result<WorldUpSummary, EdgeError>;
    fn drop_fork(&self, req: &WorldSelector) -> Result<bool, EdgeError>;
    fn list(&self, repo_root: Option<&Path>) -> Result<Vec<WorldSummary>, EdgeError>;
    fn world_processes(&self, req: &WorldSelector) -> Result<WorldProcessReport, EdgeError>;
    fn prune(&self, dry_run: bool) -> Result<PruneReport, EdgeError>;
    /// Mutate/list the project's LOCAL `0600` vault through injected `trait SecretVault` ONLY; remote OSPP secrets never enter it.
    fn secret_set(&self, repo_root: &Path, key: &str, value: &str, force: bool) -> Result<(), EdgeError>;
    fn secret_unset(&self, repo_root: &Path, key: &str) -> Result<bool, EdgeError>;
    fn secret_list(&self, repo_root: &Path) -> Result<Vec<String>, EdgeError>;
    fn logs(&self, req: &LogsRequest, on_chunk: &mut dyn FnMut(StreamChunk)) -> Result<(), EdgeError>;
    fn exec_target(&self, req: &ExecTargetRequest) -> Result<WorldExecTarget, EdgeError>;
    fn hook_run(&self, req: &HookRunRequest, on_chunk: &mut dyn FnMut(StreamChunk)) -> Result<i32, EdgeError>;
    /// `slipforkd` binds host `127.0.0.1:0` listeners in-process and bridges each to `dial_world_tcp` for the
    /// lifetime of this control connection (client holds connection open; disconnect/Ctrl-C tears bridges down).
    fn forward(&self, req: &ForwardRequest, wait_disconnect: &mut dyn FnMut(ForwardSummary)) -> Result<(), EdgeError>;
    /// Pair the machine's P-256 key over `ospp-v1` (`docs/design/ospp.md` §Device Pairing), refusing any
    /// `endpoint_url` that differs from `slipforkd`'s single boot-injected `OsppEndpoint`, streaming the
    /// server-supplied `<url>#handle=<handle>` fragment URL via `on_handle`, and awaiting `pair_approved`.
    fn auth_pair(&self, endpoint_url: &str, on_handle: &mut dyn FnMut(&str)) -> Result<AuthSummary, EdgeError>;
    /// Cleanly tears down live worlds and exits `slipforkd` (`ADR-0011` §1 `:0 -> :80` handover during `daemon install`).
    fn shutdown(&self) -> Result<(), EdgeError>;
}
```

- **Daemon Wire vs CLI `--json` Documents (`docs/design/dx-0.1.0-ux.md` §3 & §10):** Over `<state_dir>/run/daemon.sock`, `DaemonControl` exchanges the typed values above (`WorldId`, `WorldOrigin`, `ServiceState`, full 64-hex digests in `WorldLineage`), and `slipfork open` passes `&WorldOrigin` directly to `BrowserOpener::open_origin`. Each CLI verb owns its own top-level `--json` document — there is **no** shared envelope type, wrapper, or projection layer: object verbs carry `"ok": true`, `list` / `support` / `snow resolve` / `secret list` emit their bare collection payload, `logs --json` is the single NDJSON stream, `exec` / `shell` / `db` / `redis` reject `--json`, and a failure document carries `code` + `error` + `next` with the per-code extras listed in `docs/design/dx-0.1.0-ux.md` §10.

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
    /// Verbatim Git `origin` remote URL string for `vcs_root` (passed unchanged to OSPP `subscribe.remote`;
    /// never canonicalized, hashed, or derived from `ProjectSlug`).
    fn origin_remote(&self, vcs_root: &Path) -> Result<Option<String>, VcsError>;
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
    /// Active branch (`git`) or working-copy bookmark (`jj`) for `checkout`, or `None` when detached (`M3.28.5`).
    fn current_branch(&self, checkout: &Path) -> Result<Option<String>, VcsError>;
}

/// Single branch-to-fork sanitiser (`crates/slipfork-vcs`, `M3.28.5`): lowercase ASCII, `/` -> `-`,
/// drop non-`[a-z0-9-]`, collapse/trim `-`, and reject empty results.
pub fn sanitize_fork_name(branch: &str) -> Result<String, VcsError>;
```

- **D/F-Safe Ref Namespace, Pinned Snapshot Identity & Single Parser (`M3.28.5`):** Git forbids `refs/heads/<fork>` and `refs/heads/<fork>/wip` from coexisting; keeping the branch at `refs/heads/<fork>` and placing the dirty view at `refs/slipfork/<fork>/wip` (`git fetch slipfork '+refs/slipfork/<fork>/wip:refs/slipfork/<fork>/wip'`, pointing to `base` when clean) avoids all refname collisions. Synthesizing `refs/slipfork/<fork>/wip` passes `-c user.name=slipfork -c user.email=slipfork@localhost` before `commit-tree` (and equivalent explicit author/committer overrides in `jj`), never reading or requiring user `git config`, and non-zero VCS tool exits format as single-line `VcsError` values (failing subcommand + last stderr line). Zero `AdapterKind` enum outside `slipforkd`'s factory, zero mutation of the live `.git/index`, zero second `slipfork.toml` parser, and zero binary targets in `crates/slipfork-vcs`.

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
pub enum IntegrationCategory { WorldEngine, Secrets, Vcs, TestRunner, Cli }

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
3. **`H-003` [Validated + `M3.29` Two-Tier Clarification]:** `FileVaultProvider` (`0600` local vault) persists locally-provisioned backing-service credentials, `slipfork secret` overrides, and local-mode `!!` secrets across restarts and automatically populates `OsppClaims` IDs on `subscribe`, whereas remote OSPP secrets (`{ "encrypted": "<base64>" }`) and encrypted dump streams live strictly in `slipforkd` memory and are never written to disk (`docs/design/ospp.md`, `ADR-0001` §3, `ADR-0008` §§1–2); **OSPP is not a prerequisite for M2 or M3**, and Slipfork-assigned `WorldOrigin` always overrides provider `domains::` in local worlds (`ADR-0005` §1, `ADR-0008` §3).
4. **`H-004` [Closed by Owner Ruling — Superseded by `ADR-0010`]:** Closed by owner ruling (2026-11-16) — superseded by `ADR-0010` (`BtrfsEngine` is Slipfork's sole `WorldEngine`). Zero open item remains.
5. **`H-005` [Validated + Gate Amendment]:** Canonical VCS `ProjectSlug` resolution (`ADR-0005` §3, strictly local — OSPP `subscribe` sends the verbatim Git `remote` string), single untagged `struct WorldOrigin(LocalOrigin)` per world in `0.1.0` (one origin kind — tombstone in `ADR-0011`), and reserved `test-*` prefix across forks and `domains[].id`.
6. **`H-006` [Closed by Deferral — Isolated Leaf `M4.1`]:** `weaved`'s registration wire payload remains gated on `tunnel-weaver` publishing `weaved`'s IPC specification; `M3.28.15` removes the unimplemented outbound-registrar surface from `0.1.0` (worklist: `plan/nodes/M3/M3.28/NODE.md`), while `WeaverServiceName` (`SERVICE_NAME_MAX = 31` bytes, `ADR-0008` §6) and `UnitSpec.forward_port` remain in `slipfork-world`/`slipfork-edge`.
7. **`H-008` [Validated]:** Unprivileged `LISTEN_FDS` `slipforkd` is the **sole HTTP/WS ingress** (`snow.yml` `ingresses` longest-prefix table -> `dial_world_tcp` with `Host` + `X-Forwarded-*` preserved; `ADR-0005` §1, `ADR-0008` §5, `ADR-0011` §1).
8. **`H-009` [Validated — User-Reachable Scope]:** `ADR-0007`'s 5-tier scale (`none`..`hardened`) is scoped strictly to user-reachable integrations; pre-CLI internal library adapters are omitted from `support/integrations.toml` until wired + documented at `declared` (`plan/HUMAN.md`).
9. **`H-010` [Validated + `M3.29` OSPP v1 Amendment]:** Unified user daemon `slipforkd` (`User=<developer-user>`, `--state-dir`, `--runtime-dir`, `<runtime_dir>/daemon.sock`) is the single stateful Rust Composition Root owning worlds, live `OSPP v1` subscriptions (`docs/design/ospp.md`), the local-only `0600` `FileVaultProvider`, in-memory HPKE-decrypted remote secrets, and daemon-owned `forward` listeners while thin-client `slipfork` enters worlds directly inheriting native terminal stdio (zero `SCM_RIGHTS` across the CLI<->daemon TTY boundary, zero `pty.rs` `unsafe`) and auto-spawns `slipforkd --listen 127.0.0.1:0` on `up`/`fork` when `daemon.sock` is absent; and `WorldOrigin::local(&route, ingress_port)` formats `:<port>` when `!= 80` (`ADR-0008` §§1–2).
10. **`H-011` [Validated]:** Persistent per-world PID 1 (`slipfork-world-init`), `<control_dir>/netns.sock` (`WorldSpec.control_dir`, `ADR-0010` §2) `SCM_RIGHTS` dialer, and `<state_dir>/http-ingress.sock` `$BASE_URL` bridge — normative detail in §3.3, `ADR-0008` §3/§7, and `ADR-0010` §2/§6.
