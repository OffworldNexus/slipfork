# Result M3.7 — Option A Design of Record (`ADR-0010`, `ARCHITECTURE.md`, `docs/VISION.md`, `ADR-0001..0009` Scorched-Earth Wipe + Attempt 2 Review Sweep + `STP v1` Truthfulness Pass)

## Summary (Attempt 2/3)

Resolved every finding from the Attempt 1 Quad Review Gate (`A`–`L`), Additional Finding `M` (from sibling `M3.1`), the Owner Architectural Directive superseding Finding `A` (banning test-scaffolding Cargo features), and the Owner Ruling Addendum **`STP v1` truthfulness pass** across `ARCHITECTURE.md`, `docs/VISION.md`, and `docs/decisions/ADR-0001..0010`:

1. **Owner Architectural Directive (superseding Finding `A`) — Pure Constructor DI & Zero Test-Scaffolding Cargo Features:** Added the normative Cargo feature rule directly to `ARCHITECTURE.md` §1 (*"Cargo features are reserved for language/platform modularity; test scaffolding uses `#[cfg(test)]` + `tests/common/` + optional `publish = false` testkit crates, never a production feature flag."*). Purged all `test-support` / feature-gated `test_driver` text from `ARCHITECTURE.md`, `ADR-0010` §5, and `ADR-0002` §1; specified `BtrfsEngine::new(...)` (wiring `Arc::new(KernelBtrfsDriver)`) + `BtrfsEngine::with_driver(..., Arc<dyn SubvolumeDriver>) -> Result<Self, EngineError>`.
2. **Single Btrfs Probe Owner, Honest `SubvolumeDriver`, Disjoint Control/Socket Paths & `VolumeCloner` Collapse (`B`–`E`, `L`):** Replaced `filesystem_kind() -> FileSystemKind` with fail-closed `ensure_supported(&Path) -> Result<(), EngineError>` (`EngineError::BtrfsRequired` naming `mount -o user_subvol_rm_allowed,noatime`); dropped `set_readonly(..., bool)` (making `destroy_subvolume` clear `BTRFS_SUBVOL_RDONLY` internally); made `crates/slipfork-engine::btrfs` (`KernelBtrfsDriver::ensure_supported` + `btrfs::probe`) the **single** Btrfs probe owner shared by `slipforkd` boot and pre-daemon `slipfork doctor` (CLI holds **no** `WorldEngine`); standardized `<runtime_dir>/daemon.sock`, `<state_dir>/http-ingress.sock`, and injected `WorldSpec.control_dir` (`<state_dir>/ctl/<world_id>`, disjoint from `<runtime_dir>` / `<state_dir>/run` and outside `<subvol>`); and collapsed `VolumeCloner` into `Arc<dyn SubvolumeDriver>`.
3. **Engine-Owned Protocol Readiness (`M` — Option 1):** Specified `ServiceSpec.readiness: ReadinessProbe { None, TcpPort { port, timeout }, Exec { port, argv, timeout } }` + `EngineError::ServiceReadinessFailed` normatively in `ADR-0002` §2 and `ARCHITECTURE.md` §3.3 so `BtrfsEngine::spawn_service` returns a genuinely SQL/protocol-usable service with zero caller-side `psql`/`redis-cli` polling loops.
4. **Provenance, Deduplication, `M4.1` Alignment, Target-State Labels & AppArmor Citation (`F`–`K`):** Bound Spike A metrics in `ADR-0010` to owner-supplied measurements (`plan/nodes/M3/LOG.md`, `M3.7`/`M3.11` `NODE.md`) with `docs/research/spike-option-a-btrfs-crun.md` marked as a forward reference recorded by `M3.11`; made `ADR-0010` the sole normative home for `BTRFS_IOC_*` and `--ipc` (`CLONE_NEWIPC`) (`§` pointers elsewhere); swept `docs/VISION.md:35` `WeavedRegistrar` to `M4.1`; removed `ADR-0009:8`'s stale `base_instance` reference and `ADR-0002:28`'s self-link; labelled M3 target-state elements with owning node IDs (`M3.20`, `M3.12`–`M3.15`, `M3.21`); and cited Ubuntu 24.04 LTS (`noble`, Linux 6.8+, AppArmor 4.0) without unversioned `domain.c` citations.

## Per-Finding Disposition Table

| ID | Lens / Source | Disposition | Resolution & Exact Evidence |
|---|---|---|---|
| **A (Owner Directive)** | Owner Directive (supersedes `review-ioc` A) | **Fixed** | Zero `feature = "test-support"` on any production crate. Added normative feature-policy rule in `ARCHITECTURE.md` §1; specified `BtrfsEngine::new(...)` + `BtrfsEngine::with_driver(..., Arc<dyn SubvolumeDriver>)` in `ARCHITECTURE.md` §1/§3.3, `ADR-0010` §5, and `ADR-0002` §1; scoped test fakes strictly to `#[cfg(test)]` + `tests/common/` + optional `publish = false` dev-dep testkits (binding rule for `M3.12`, `M3.14`, `M3.15`, `M3.20`). |
| **B** | `review-ioc` (MAJOR) | **Fixed** | Deleted `filesystem_kind() -> FileSystemKind`; replaced with `SubvolumeDriver::ensure_supported(&self, path: &Path) -> Result<(), EngineError>` returning `EngineError::BtrfsRequired` (`mount -o user_subvol_rm_allowed,noatime`) in `ARCHITECTURE.md` §3.3 and `ADR-0010` §§4–5. |
| **C** | `review-ioc` + `review-legality` (MAJOR) | **Fixed** | Picked **one** probe owner: `crates/slipfork-engine::btrfs` (`KernelBtrfsDriver::ensure_supported` + `slipfork_engine::btrfs::probe(&Path)` delegating directly to `KernelBtrfsDriver` with zero duplicated `statfs` or ioctl logic). `BtrfsEngine` calls it at `slipforkd` boot and thin-client `slipfork doctor` calls `btrfs::probe` pre-daemon (holding **no** `WorldEngine`, so `doctor` works when `slipforkd` refuses to boot off-Btrfs) — `ARCHITECTURE.md` §1/§2/§3.3, `ADR-0010` §4. |
| **D** | `review-ioc` + `review-legality` (MAJOR) | **Fixed** | Defined three disjoint canonical paths everywhere (`ARCHITECTURE.md` §1/§2/§3.3/§4 `H-011`, `ADR-0010` §2, `ADR-0008` §3/§7): `<runtime_dir>/daemon.sock` (daemon-wide `--runtime-dir`, defaulting to `$XDG_RUNTIME_DIR/slipfork` or `<state_dir>/run`), `<state_dir>/http-ingress.sock`, and `WorldSpec.control_dir` = `<state_dir>/ctl/<world_id>` (injected explicitly on `WorldSpec`, outside `<subvol>`, holding `netns.sock`, `world-init.json`, `init.ready`, `oci/`, `oci-run/`). Removed all `<run_dir>` and `<world_dir>/netns.sock` references. |
| **E** | `review-ioc` (MINOR) | **Fixed** | Dropped `set_readonly(&self, subvol, readonly: bool)` from `SubvolumeDriver`; `destroy_subvolume` clears `BTRFS_SUBVOL_RDONLY` internally before `BTRFS_IOC_SNAP_DESTROY_V2` (`ARCHITECTURE.md` §3.3, `ADR-0010` §§3, 5). Swept all `<state_root>/http-ingress.sock` occurrences to `<state_dir>/http-ingress.sock` (`ARCHITECTURE.md` §2/§3.3/§4, `ADR-0008` §3, `ADR-0002` §3). |
| **F** | `review-legality` (MAJOR) | **Fixed** | Bound `19.5 ms` / `11–24 ms` / `39 ms` / `6.3 MiB` in `ADR-0010` Context §2 to real existing provenance (owner-supplied Spike A measurements in `plan/nodes/M3/LOG.md`, `M3.7/NODE.md`, `M3.11/NODE.md`) and phrased `docs/research/spike-option-a-btrfs-crun.md` explicitly as a forward reference (*"to be recorded in full by `M3.11`"*). |
| **G** | `review-cruft` (MAJOR) | **Fixed** | Added normative-home banner to `ADR-0010` `## Decision` making `ADR-0010` §§2–7 the sole normative home for the `BTRFS_IOC_*` lifecycle and `--ipc` (`CLONE_NEWIPC` / `shmget` key `90000`), replacing restatements in `ADR-0002` §3, `ADR-0006` §4.3, `ADR-0008` §7, `ADR-0009` §3, `docs/VISION.md:16`, and `ARCHITECTURE.md` §2/§3.3 with concise `§` pointers. |
| **H** | `review-cruft` (MAJOR) | **Fixed** | Swept `docs/VISION.md:35` so `TunnelRegistrar` (`0.1.0`/`0.2.0` with `UnavailableTunnelRegistrar`) is distinguished cleanly from the concrete Tunnel Weaver (`weaved`) wire adapter `WeavedRegistrar` isolated in `M4.1` behind closed `H-006`. |
| **I** | `review-cruft` (MAJOR) | **Fixed** | Deleted the dangling `base_instance: String` sentence and `ADR-0002` link from `docs/decisions/ADR-0009-world-base-image-and-image-contract.md:8`. |
| **J** | `review-cruft` + `review-legality` (MINOR) | **Fixed** | Scoped `ARCHITECTURE.md:5` to the single Btrfs world engine (`BtrfsEngine`) + injected `SubvolumeDriver` seam; replaced `ADR-0002`'s self-link in Consequences with `(this ADR)`; and explicitly labelled unshipped M3 target-state items across `ARCHITECTURE.md`, `ADR-0010`, `ADR-0002`, `ADR-0008`, `ADR-0009`, and `docs/VISION.md` with owning node IDs (`M3.20`, `M3.12`, `M3.13`, `M3.14`, `M3.15`, `M3.21`, Phase 2 `M3.2`). |
| **K** | `review-legality` (MINOR) | **Fixed** | Cited Ubuntu 24.04 LTS (`noble`, Linux 6.8+, AppArmor 4.0 `abi <abi/4.0>,`, sysctl `kernel.apparmor_restrict_unprivileged_userns=1`) in `ADR-0010` Context §3 & Decision §7 and deleted the unversioned `security/apparmor/domain.c:profile_transition` citation. |
| **L** | Upward Check (`VolumeCloner` vs `SubvolumeDriver`) | **Fixed (Collapsed)** | **Decision:** `VolumeCloner` collided directly with `SubvolumeDriver` under Option A (where Btrfs subvolumes are the sole CoW primitive and `crates/slipfork-dag` is pure/filesystem-free by default with zero `slipfork-engine` dependency). Collapsed `VolumeCloner` + `CowArtifactStore` into `Arc<dyn SubvolumeDriver>` shared by `BtrfsEngine` and M3's `BtrfsArtifactStore: ArtifactStore` (`crates/slipfork-test`), updating `ARCHITECTURE.md` §1/§2, `ADR-0006` §4.3, and `ADR-0010` §5 so zero parallel cloning pipeline exists. |
| **M** | Sibling `M3.1` Quad Gate (`SPEC_BLIND_SPOT`) | **Fixed (Option 1)** | Adopted **Option 1 (engine owns readiness)** normatively in `ADR-0002` §2 and `ARCHITECTURE.md` §3.3: `ServiceSpec.readiness: ReadinessProbe { None, TcpPort { port, timeout }, Exec { port: Option<u16>, argv, timeout } }` supplied by `slipforkd` from `snow.yml` + `ADR-0009` §2 contracts (`postgres:<v>` -> in-container `psql SELECT 1`, `redis:<v>` -> `redis-cli PING`, `seaweedfs` / `components` with `internal_ports` -> `TcpPort`, workers -> `None`), failing closed on timeout/crash with `EngineError::ServiceReadinessFailed` + captured `stderr`. **Downstream consumers:** `M3.1`'s test-side `psql` poll is a temporary test-local shim until `M3.13` lands and is deleted once `BtrfsEngine::spawn_service` enforces `ReadinessProbe`; `M3.13`/`M3.14` implement `ReadinessProbe` inside `BtrfsEngine::spawn_service` and assert both happy-path `Exec` readiness and fail-closed `stderr` timeout in conformance; Phase 2 `M3.2` (`M3.2.1`+) relies strictly on `spawn_service` returning SQL-ready worlds with zero orchestrator-side `psql` polling. |

## `STP v1` Truthfulness Pass (Owner Ruling Addendum)

Applied the bounded Phase 2 (`M3.2`) truthfulness sweep strictly from the owner-supplied `STP v1` facts (zero invented protocol frames):

- **Replaced in `docs/decisions/ADR-0006-polyglot-runner-protocol-and-world-owned-stack.md` (§3, §5), `ARCHITECTURE.md` (§1 Mermaid + prose, §2 `crates/slipfork-test`, §3.6), `docs/VISION.md` (§2, §3, Milestone table M3 & M4), `ADR-0003` (§3), and `ADR-0007` (Context):**
  - Replaced normative `RunnerProtocol v1` / NDJSON-over-`.slipfork/runs/<run_id>/<slot>/runner.sock` with the owner-ratified **`STP v1`** direction over an inherited shared file descriptor (`SLIPFORK_PROTOCOL_FD`, an `AF_UNIX` `socketpair`), explicitly stating that the full normative `STP v1` specification and rewritten `M3.2` plan land in the Phase 2 design pass.
  - Recorded framework-exposed global DAG nodes including **persistent setup/fixture stage nodes** frozen into **read-only Btrfs subvolume snapshots** (`ADR-0010` §3), first-class **coverage and mutation testing (`mutants`) inside `STP v1`**, the **language-blind `STP v1` DAG orchestrator** (`crates/slipfork-test`), and **ad-hoc, unpublished `pytest` / `vitest` / `phpunit` proof plugins**.
  - **Purged stale terms** (`0` matches remaining across `ARCHITECTURE.md`, `docs/VISION.md`, and `docs/decisions/`): `RunnerProtocol v1`, `runner.sock`, `CommandTestAdapter`, `SocketRunnerAdapter`, `TestAdapter`, `CoverageDriver`, `TestCoordinator`, `pytest-slipfork`, `PytestAdapter`, `PlaywrightVitestAdapter`, `PhpUnitAdapter`, and hardcoded per-language `COVERAGE_FILE`/`NODE_V8_COVERAGE`/`LLVM_PROFILE_FILE` driver claims; removed premature M4 published-runner-plugin promises in `docs/VISION.md`.
  - **Preserved intact:** shipped `0.1.0` typed `[test]` coverage schema/thresholds (`CoverageConfig`, `CoverageThreshold`), surviving `SLIPFORK_OUTPUT_DIR`, hermetic `ResourceMode::Test`, and Btrfs-warm `TestSlot` worlds.

## Out-of-Scope Sibling Nodes (Untouched per Scope Lock)

- Published `README.md`, `docs/index.md`, `docs/getting-started.md`, `docs/reference/cli.md` → owned by **`M3.18`**.
- `docs/support-matrix.md` + `support/integrations.toml` → owned by **`M3.14` / `M3.17`**.
- `CHANGELOG.md` → owned by **`M3.2.17`**.
- `docs/research/spike-option-a-btrfs-crun.md` (new) + `docs/research/spike-option-b-kvm-microvm.md` historical retention note → owned by **`M3.11`**.

## Acceptance Verification

1. **PASS** — `test -f docs/decisions/ADR-0010-btrfs-only-world-engine.md` (exit 0)
2. **PASS** — `grep -n '0x9123683e\|BTRFS_IOC_SNAP_CREATE_V2\|BTRFS_SUBVOL_RDONLY\|BTRFS_IOC_SUBVOL_SETFLAGS\|BTRFS_IOC_SNAP_DESTROY_V2' docs/decisions/ADR-0010-btrfs-only-world-engine.md` (all 5 constants present, exit 0)
3. **PASS** — `grep -n 'CLONE_NEWIPC' docs/decisions/ADR-0010-btrfs-only-world-engine.md ARCHITECTURE.md` (matches in both files, exit 0)
4. **PASS** — `grep -n 'BtrfsEngine\|SubvolumeDriver\|KernelBtrfsDriver\|BtrfsRequired' ARCHITECTURE.md` (all 4 symbols present, exit 0)
5. **PASS** — `test -f docs/decisions/ADR-0002-world-engine-ioc-btrfs.md` (exit 0)
6. **PASS** — `test ! -e docs/decisions/ADR-0002-world-engine-ioc-incus-and-reflink.md` (exit 0)
7. **PASS** — `! grep -rniE 'incus|zfs|ficlone|qemu|kvm|statefulram|reflink' ARCHITECTURE.md docs/VISION.md docs/decisions/` (0 matches, exit 0)
8. **PASS** — `uvx --offline zensical build --strict` (`No issues found`, exit 0)
9. **PASS** — `uv run --no-project tools/check_docs_honesty.py` (`OK (9 published doc(s) match real CLI subcommands)`, exit 0)
10. **PASS** — `uv run --no-project tools/plan.py validate` (`plan OK`, exit 0)
11. **PASS** — `uv run --no-project tools/guard.py` (`OK`, exit 0)

---

## Attempt 3/3 — Quad Gate Wording/Contract-Ownership Fixes (`L1..L6`, `I1..I6`), Cruft Dedupe (`C1..C8`), and Owner Terminology Rule

### 1. Per-Finding Disposition (`L1..L6`, `I1..I6`)

| ID | Severity | Disposition | Final Text Evidence |
|---|---|---|---|
| **L1** | BLOCKER | **Fixed** | `docs/decisions/ADR-0006-stp-v1-and-world-owned-stack.md:32`: *"In M3 Phase 2 (`M3.2`), `crates/slipfork-test` will ship a **language-blind `STP v1` DAG orchestrator**, to be proven in `M3.2` with external **ad-hoc, unpublished `pytest`, `vitest`, and `phpunit` plugins**."* |
| **L2** | BLOCKER | **Fixed** | `ARCHITECTURE.md:580`: *"in M3 Phase 2 (`M3.2`), `crates/slipfork-test` will ship a **language-blind `STP v1` DAG orchestrator**, to be proven in `M3.2` with external **ad-hoc, unpublished `pytest`, `vitest`, and `phpunit` plugins**"* |
| **L3** | MAJOR | **Fixed** | `ARCHITECTURE.md:15`: separates shipped `0.2.0` adapters from *"M3 Phase 1 target-state `BtrfsEngine` (`M3.13`/`M3.15`)"* and *"M3 Phase 2 target-state (`M3.2`) `STP v1` orchestrator + `BtrfsArtifactStore` (`crates/slipfork-test`, sharing `Arc<dyn SubvolumeDriver>`)"*. |
| **L4** | MINOR | **Fixed** | `ARCHITECTURE.md:77`–`80`: `AS["trait ArtifactStore (crates/slipfork-test, Phase 2 M3.2)"]`, `AS1["BtrfsArtifactStore (Phase 2 M3.2; ...)"]`, and `STP["... to be proven in M3.2 via external ad-hoc unpublished pytest / vitest / phpunit plugins"]`. |
| **L5** | MINOR | **Fixed** | `docs/VISION.md:31` & `:50`: *"to be proven in `M3.2` via ad-hoc, unpublished `pytest`, `vitest`, and `phpunit` plugins"*. |
| **L6** | MINOR | **Fixed** | `docs/decisions/ADR-0010-btrfs-only-world-engine.md:9`: cites `plan/nodes/M3/M3.11/NODE.md` (owner-supplied measurements) + forward reference `docs/research/btrfs-crun-benchmarks.md` (`recorded by M3.11`), dropping `plan/nodes/M3/LOG.md`. |
| **I1** | MAJOR | **Fixed** | `docs/decisions/ADR-0008-ospp-v1-websocket-tpmd-resource-strategy-and-world-ux.md:53`: *"`up` returns `[origin]` after `WorldEngine::spawn_service` (`ReadinessProbe`, `ADR-0002` §2) reports each component protocol-ready"*. |
| **I2** | MAJOR | **Fixed** | `docs/decisions/ADR-0009-world-base-image-and-image-contract.md:23` & `:59`–`64` add `pub readiness: ReadinessProbe` on `OciRootfs` (`postgres:<v>` `psql SELECT 1`, `redis:<v>` `redis-cli PING`, `seaweedfs` `TcpPort 9000`); `docs/decisions/ADR-0002-world-engine-ioc-btrfs.md:22` and `ARCHITECTURE.md:302`–`305`, `:383` state `slipforkd` passes `oci_rootfs.readiness` through unchanged with zero `match BackingServiceKey` ladder. |
| **I3** | MINOR | **Fixed** | `ARCHITECTURE.md:15` states once that the thin `slipfork` CLI calls `Layout::resolve` at its composition boundary (`$XDG_RUNTIME_DIR/slipfork` or `<state_dir>/run` when unset) and passes explicit `--state-dir` + `--runtime-dir` to `slipforkd`; `ADR-0010:25` & `:27` point to `ARCHITECTURE.md` §1 / `ADR-0008` §4 and delete the daemon-side `<state_dir>/run` alias. |
| **I4** | MINOR | **Fixed** | `ARCHITECTURE.md:218`, `:232`, `:23`, `:58`, `:382` and `ADR-0010:34`, `:38`: moved `fn probe(&self, path: &Path) -> Result<BtrfsProbeReport, EngineError>` onto `trait SubvolumeDriver`, with free `slipfork_engine::btrfs::probe(state_dir)` delegating to `KernelBtrfsDriver.probe(state_dir)`. |
| **I5** | MINOR | **Fixed** | Removed `pytest`/`vitest`/`phpunit` tokens from the language-blind `crates/slipfork-test` crate row (`ARCHITECTURE.md:96`) and DAG ADR (`docs/decisions/ADR-0003-orthogonal-artifact-and-fixture-dag.md:22`); kept them only where explicitly labelled external unpublished proof plugins under `M3.2`. |
| **I6** | MINOR | **Fixed** | Deleted vestigial `fn name(&self) -> &'static str;` from single-implementation `trait WorldEngine` (`ARCHITECTURE.md:342`). |

### 2. Per-Finding Disposition (`C1..C8` Cruft Dedupe)

| ID | Severity | Disposition | Final Text Evidence |
|---|---|---|---|
| **C1** | MAJOR | **Fixed** | Kept ONE normative Cargo Feature & Test-Scaffolding Rule in `ARCHITECTURE.md:11` (`#[cfg(test)]`, `tests/common/`, or optional `publish = false` dev-dep testkit crate); cited `ARCHITECTURE.md` §1 from `ADR-0010:40` and `ADR-0002:19` and deleted the `(or tests/support/)` branch. |
| **C2** | MINOR | **Fixed** | Kept the single prose copy of `statfs == BTRFS_SUPER_MAGIC (0x9123683e)` + `mount -o user_subvol_rm_allowed,noatime` in `ADR-0010` §4 (`:33`–`36`, plus concise Rust port doc in `ARCHITECTURE.md:214`–`219`), replacing restatements in `ARCHITECTURE.md:15`, `ARCHITECTURE.md:382`, and `docs/VISION.md:50` with § pointers. |
| **C3** | MINOR | **Fixed** | Kept the unified-subvolume path string (`<subvol>/rootfs` + `/var/lib/slipfork/state/{pgdata,redis,s3}`) solely in `ADR-0010` §2 (`:22`); replaced restatements in `ADR-0002:25`, `ADR-0009:3/:22/:23/:27/:97/:100`, `ADR-0003:3`, `ARCHITECTURE.md:384`, and `docs/VISION.md:50` with `ADR-0010` §2 pointers. |
| **C4** | MINOR | **Fixed** | `docs/decisions/ADR-0008-...md:39`: renamed `Engine-Agnostic Restore` → `In-Container Restore`. |
| **C5** | MINOR | **Fixed** | `docs/decisions/ADR-0006-stp-v1-and-world-owned-stack.md:18`: banner distinguishes shipped `0.1.0` §1 & §5 `[test]` schema from M3 target-state §2 & §4 (`M3.13`–`M3.15` & `M3.2`) and Phase 2 `M3.2` §3. |
| **C6** | MINOR | **Fixed** | `docs/decisions/ADR-0006-stp-v1-and-world-owned-stack.md:13`–`14`: dropped deleted M1 host-shared-`/usr` `352 ms`/`118 ms` citation and re-anchored to `M3.11` measurements (`19.5 ms` RO snapshot, `11–24 ms` writable clone, `39 ms` to warm SQL; `plan/nodes/M3/M3.11/NODE.md`, `docs/research/btrfs-crun-benchmarks.md`). |
| **C7** | MINOR | **Fixed** | Renamed `docs/decisions/ADR-0006-polyglot-runner-protocol-and-world-owned-stack.md` (deleted) → **`docs/decisions/ADR-0006-stp-v1-and-world-owned-stack.md`** (created). Updated path cross-references in `ARCHITECTURE.md` (`:96`, `:551`), `docs/decisions/ADR-0007-integration-support-tiers.md` (`:8`), and `docs/decisions/ADR-0009-world-base-image-and-image-contract.md` (`:23`) (`lead`: please update `M3.7` `NODE.md` `paths` entry accordingly). |
| **C8** | MINOR | **Fixed** | Labelled `FakeBaseImageBuilder`, `MemorySecretProvider`, `TestTreeDriver`, and `InMemoryTunnelRegistrar` explicitly as test-only per `ARCHITECTURE.md` §1 across `ARCHITECTURE.md:36`, `:44`, `:59`, `:93`, `:94` and `ADR-0009:42`. |

### 3. Owner Docs/Terminology Rule Sweep

- Removed every occurrence of `Option A`, `Option B`, `Spike A`, `Spike B`, `Variant a`, `Scorched-earth`, and "we chose X over Y" / `## Rejected Alternatives` prose across `ARCHITECTURE.md`, `docs/VISION.md`, and `docs/decisions/ADR-0001..0010` (`ADR-0009`'s `## Rejected Alternatives` deleted outright; `BtrfsEngine` stated directly as what Slipfork is and does).
- Updated benchmark forward references in `ADR-0010:9` and `ADR-0006:13`–`14` to the canonical report path **`docs/research/btrfs-crun-benchmarks.md`** (recorded by `M3.11`, from owner-supplied measurements in `plan/nodes/M3/M3.11/NODE.md`).

### 4. Attempt 3 Acceptance & Guard Verification

1. **PASS** — `test -f docs/decisions/ADR-0010-btrfs-only-world-engine.md` (exit 0)
2. **PASS** — `grep -n '0x9123683e|BTRFS_IOC_SNAP_CREATE_V2|BTRFS_SUBVOL_RDONLY|BTRFS_IOC_SUBVOL_SETFLAGS|BTRFS_IOC_SNAP_DESTROY_V2' docs/decisions/ADR-0010-btrfs-only-world-engine.md` (all 5 constants present, exit 0)
3. **PASS** — `grep -n 'CLONE_NEWIPC' docs/decisions/ADR-0010-btrfs-only-world-engine.md ARCHITECTURE.md` (matches in both files, exit 0)
4. **PASS** — `grep -n 'BtrfsEngine|SubvolumeDriver|KernelBtrfsDriver|BtrfsRequired' ARCHITECTURE.md` (all 4 symbols present, exit 0)
5. **PASS** — `test -f docs/decisions/ADR-0002-world-engine-ioc-btrfs.md` (exit 0)
6. **PASS** — `test ! -e docs/decisions/ADR-0002-world-engine-ioc-incus-and-reflink.md` (exit 0)
7. **PASS** — `! grep -rniE 'incus|zfs|ficlone|qemu|kvm|statefulram|reflink' ARCHITECTURE.md docs/VISION.md docs/decisions/` (0 matches, exit 0)
8. **PASS** — `! grep -rniE 'option [ab]|spike [ab]' ARCHITECTURE.md docs/VISION.md docs/decisions/` (0 matches, exit 0)
9. **PASS** — `uvx --offline zensical build --strict` (`No issues found`, exit 0)
10. **PASS** — `uv run --no-project tools/check_docs_honesty.py` (`OK (9 published doc(s) match real CLI subcommands)`, exit 0)
11. **PASS** — `uv run --no-project tools/plan.py validate` (`plan OK`, exit 0)
12. **PASS** — `uv run --no-project tools/guard.py` (`OK`, exit 0)

---

## Attempt 4/4 — Final Polish Pass (`L-a`, `L-b`, `I-a`, `I-b` + Cruft Dedupe `X1..X7`)

### 1. Per-Finding Disposition (`L-a`, `L-b`, `I-a`, `I-b`)

| ID | Lens / Severity | Disposition | Final Text Evidence |
|---|---|---|---|
| **L-a** | `review-legality` (MINOR) | **Fixed** | `docs/decisions/ADR-0010-btrfs-only-world-engine.md:58`: labelled explicitly as `M3.10`/`M3.16` target state (*"In `M3.10`/`M3.16` target state, dedicated VM test harnesses running unlinked `cargo test` binaries under `kernel.apparmor_restrict_unprivileged_userns=1` explicitly permit `/usr/bin/unshare` and `slipfork-world-init` in their `/etc/apparmor.d/slipfork` `userns` profile."*). |
| **L-b** | `review-legality` (MINOR) | **Fixed** | `docs/decisions/ADR-0002-world-engine-ioc-btrfs.md:12`: appended `(`M3.2` target state)` to `crates/slipfork-test`. |
| **I-a** | `review-ioc` (MINOR) | **Fixed** | `ARCHITECTURE.md:15`, `:23`, `:93`, `:97`, `:230`–`233`, `:410` + `ADR-0010:34`, `:41` + `ADR-0008:69`: replaced un-injected `btrfs::probe(state_dir)` with `pub fn probe_with(driver: &dyn SubvolumeDriver, state_dir: &Path) -> Result<BtrfsProbeReport, EngineError>`, with `crates/slipfork/src/main.rs` constructing `KernelBtrfsDriver` at the CLI composition boundary and passing `&dyn SubvolumeDriver` so `doctor` stays conformance-testable against a test `SubvolumeDriver` supplied via `ARCHITECTURE.md` §1's test-only vehicles. |
| **I-b** | `review-ioc` (MINOR) | **Fixed** | `docs/decisions/ADR-0010-btrfs-only-world-engine.md:35`–`40`: specified `BtrfsProbeReport` fields **once** (`state_dir`, `fstype`, `btrfs_super_magic`, `user_subvol_rm_allowed: Result<(), i32>`, `noatime: bool`), with `ARCHITECTURE.md:217`–`218` pointing directly to `ADR-0010` §4. |

### 2. Per-Finding Disposition (`X1..X7` Cruft Dedupe)

| ID | Lens / Severity | Disposition | Final Text Evidence |
|---|---|---|---|
| **X1** | `review-cruft` (MAJOR) | **Fixed** | `docs/decisions/ADR-0003-orthogonal-artifact-and-fixture-dag.md:1,21-22`: deleted `+ Runner Zygote` from title and deleted the `fork()` zygote clause from §3 (`0` `zygote` matches remain). |
| **X2** | `review-cruft` (MAJOR) | **Fixed** | `docs/VISION.md:16`: dropped `fork()` runner zygotes and replaced the misattributed `~11–24 ms` figure with a pointer to `ADR-0010` Context §2. |
| **X3** | `review-cruft` (MAJOR) | **Fixed** | `ARCHITECTURE.md:604` + `docs/VISION.md:31,50`: kept the single normative `STP v1` orchestrator / external proof-plugin / Phase 2 spec promise in `ADR-0006` §3 (`:32`) and replaced restatements with `ADR-0006` §3 pointers. |
| **X4** | `review-cruft` (MINOR) | **Fixed** | `ARCHITECTURE.md:360,365`: replaced `<subvol>/state/<host_subdir>` path re-spellings with `ADR-0010` §2 pointers. |
| **X5** | `review-cruft` (MINOR) | **Fixed** | `docs/decisions/ADR-0009-world-base-image-and-image-contract.md:58` & `ARCHITECTURE.md:274`–`299`, `:411`: consolidated `OciRootfs`, `WorldRootfs`, `ServiceRuntime`, and `trait BaseImageBuilder` Rust port signatures solely in `ARCHITECTURE.md` §3.3 and replaced `ADR-0009`'s duplicate struct block with a `§3.3` pointer (keeping `[world]` `WorldImageConfig` and image-contract rules in `ADR-0009`). |
| **X6** | `review-cruft` (MINOR) | **Fixed** | `docs/decisions/ADR-0008-ospp-v1-websocket-tpmd-resource-strategy-and-world-ux.md:3`: replaced status-banner `--ipc` / AppArmor restatement with `ADR-0010` §§6–7 pointer. |
| **X7** | `review-cruft` (MINOR) | **Fixed** | Kept `19.5 ms` / `11–24 ms` / `39 ms` / `6.3 MiB` **solely** in `ADR-0010` Context §2 (`:9`) and pointed to `ADR-0010` Context §2 from `ADR-0010:62`, `ADR-0006:14`, and `docs/VISION.md:16`. |

### 3. Per-Finding Disposition (`D1..D7` Cruft Follow-Up)

| ID | Lens / Severity | Disposition | Final Text Evidence |
|---|---|---|---|
| **D1** | `review-cruft` (MAJOR) | **Fixed** | `ARCHITECTURE.md:80`: shrunk Mermaid `STP` node label to `STP["STP v1 (ADR-0006 §3)"]`. |
| **D2** | `review-cruft` (MINOR) | **Fixed** | `docs/decisions/ADR-0006-stp-v1-and-world-owned-stack.md:3,18,32`: kept the Phase 2 design-pass sentence **once** in `§3` (`:32`) and pointed Status (`:3`) and Decision banner (`:18`) at `§3`. |
| **D3** | `review-cruft` (MINOR) | **Fixed** | `docs/decisions/ADR-0010-btrfs-only-world-engine.md:9`: cited `plan/nodes/M3/M3.11/NODE.md` + future-tense forward reference `to be recorded in full by M3.11 in docs/research/btrfs-crun-benchmarks.md` **once** in Context §2. |
| **D4** | `review-cruft` (MINOR) | **Fixed** | `docs/decisions/ADR-0006-stp-v1-and-world-owned-stack.md:13–14`: dropped duplicate forward-reference pair from Context #3 title/body and cited `ADR-0010` Context §2 once. |
| **D5** | `review-cruft` (MINOR) | **Fixed** | `docs/decisions/ADR-0009-world-base-image-and-image-contract.md:43` (+ `ARCHITECTURE.md:37`): replaced re-spelled `WorldRootfs` field list in Mermaid with `WR["WorldRootfs (ARCHITECTURE.md §3.3)"]`. |
| **D6** | `review-cruft` (MINOR) | **Fixed** | Normalized all `ADR-0006` / `ADR-0002` references across `ARCHITECTURE.md:93,96,210,579`, `ADR-0007:8`, `ADR-0009:10,23,33`, and `ADR-0010:3` to short `ADR-NNNN` IDs (`0` `docs/decisions/ADR-*` path duplicates remain). |
| **D7** | `review-cruft` (MINOR) | **Fixed** | Spelled `<state_dir>/ctl/<world_id>` **solely** in `ADR-0010` §2 (`:27`) and replaced all re-spellings (`ARCHITECTURE.md:15,97,303,410,665`, `ADR-0002:17`, `ADR-0008:69`, `ADR-0009:62`) with `ADR-0010` §2 pointers. |

### 4. Per-Finding Disposition (`E1..E4` IoC Follow-Up)

| ID | Lens / Severity | Disposition | Final Text Evidence |
|---|---|---|---|
| **E1** | `review-ioc` (MAJOR) | **Fixed** | `docs/decisions/ADR-0010-btrfs-only-world-engine.md:20` + `ARCHITECTURE.md:93`: added explicit generic deletion ruling stating `M3.20` aligns `trait WorldEngine` ahead of `M3.12`–`M3.15` and `M3.21` deletes all previously shipped engine adapters, their daemon/REST protocol, storage pools, and per-file extent-clone helpers once `BtrfsEngine` lands (`M3.12`–`M3.15`). |
| **E2** | `review-ioc` (MAJOR) | **Fixed** | `ARCHITECTURE.md:11`: named `M3.22` in §1's Cargo Feature & Test-Scaffolding Rule as deleting M2's `crates/slipfork-ospp` `test-server` feature residue and moving those reference-server doubles into `crates/slipfork/tests/support/`. |
| **E3** | `review-ioc` (MINOR) | **Fixed** | `ARCHITECTURE.md:11,232,410` + `ADR-0010:41`: specified `tests/support/` (e.g. `crates/slipfork/tests/support/` in `M3.15`) or an optional `publish = false` `[dev-dependencies]` testkit crate in §1 as the vehicle supplying a test `SubvolumeDriver` to `doctor`/`probe_with` and in-process `slipforkd`. |
| **E4** | `review-ioc` (MINOR) | **Fixed** | `ARCHITECTURE.md:23`: labelled the `CLI` Mermaid edge `main.rs constructs KernelBtrfsDriver -> probe_with(&dyn SubvolumeDriver) (M3.15)` targeting `SD` (refined with `G3` below). |

### 5. Per-Finding Disposition (`F1..F5` + Legality Tense Note)

| ID | Lens / Severity | Disposition | Final Text Evidence |
|---|---|---|---|
| **F1** | `review-cruft` / `review-ioc` (MAJOR) | **Fixed** | `ARCHITECTURE.md:11`: kept the single canonical integration-test directory `tests/support/` (plus `#[cfg(test)]` for crate-internal units and the optional `publish = false` `[dev-dependencies]` testkit crate); `0` `tests/common` occurrences remain across `ARCHITECTURE.md` and `docs/decisions/`. |
| **F2** | `review-cruft` (MINOR) | **Fixed** | `ARCHITECTURE.md:36,44,59`: shrunk Mermaid test-double labels to `(test-only; §1)`. |
| **F3** | `review-cruft` (MINOR) | **Fixed** | `docs/decisions/ADR-0009-world-base-image-and-image-contract.md:42`: shrunk Mermaid `FakeBaseImageBuilder` label to `(test-only; ARCHITECTURE.md §1)`. |
| **F4** | `review-cruft` (MINOR) | **Fixed** | `ARCHITECTURE.md:32,69,75–77,80`: renamed Mermaid node ID `TestCoord` → `STPOrch` (`0` `TestCoord` matches remain). |
| **F5** | `review-cruft` (MINOR) | **Fixed** | `docs/VISION.md:31`: pointed `ResourceMode::Test` to `ADR-0006` §2 / `ADR-0008` §3 instead of `ADR-0006` §3. |
| **Legality Note** | `review-legality` (MINOR) | **Fixed** | `docs/decisions/ADR-0010-btrfs-only-world-engine.md:9`: phrased `docs/research/btrfs-crun-benchmarks.md` explicitly in future tense (*"to be recorded in full by `M3.11` in `docs/research/btrfs-crun-benchmarks.md`"*), and synced `RESULT.md` disposition entries (`I-a`, `D3`) with the exact final ADR wording. |

### 6. Per-Finding Disposition (`G1..G3` IoC Accuracy Micro-Corrections)

| ID | Lens / Severity | Disposition | Final Text Evidence |
|---|---|---|---|
| **G1** | `review-ioc` (MINOR) | **Fixed** | `docs/decisions/ADR-0010-btrfs-only-world-engine.md:20` + `ARCHITECTURE.md:93`: separated `M3.20` (pre-Btrfs `trait WorldEngine` alignment ahead of `M3.12`–`M3.15`) from `M3.21` (sole deleter of all previously shipped engine adapters, their daemon/REST protocol, storage pools, and per-file extent-clone helpers once `M3.12`–`M3.15` land). |
| **G2** | `review-ioc` (MINOR) | **Fixed** | `ARCHITECTURE.md:410`: unified wording — `crates/slipfork/src/main.rs` constructs `KernelBtrfsDriver` and coerces it to `&dyn SubvolumeDriver` at the `probe_with` call site; tests pass a §1 test `&dyn SubvolumeDriver`. |
| **G3** | `review-ioc` (MINOR) | **Fixed** | `ARCHITECTURE.md:23`: pointed Mermaid `CLI -->|main.rs constructs KernelBtrfsDriver -> probe_with(&dyn SubvolumeDriver) (M3.15)| SD` at `SD` (`trait SubvolumeDriver`). |

### 7. Attempt 4 Acceptance & Guard Verification

1. **PASS** — `test -f docs/decisions/ADR-0010-btrfs-only-world-engine.md` (exit 0)
2. **PASS** — `grep -n '0x9123683e\|BTRFS_IOC_SNAP_CREATE_V2\|BTRFS_SUBVOL_RDONLY\|BTRFS_IOC_SUBVOL_SETFLAGS\|BTRFS_IOC_SNAP_DESTROY_V2' docs/decisions/ADR-0010-btrfs-only-world-engine.md` (all 5 constants present, exit 0)
3. **PASS** — `grep -n 'CLONE_NEWIPC' docs/decisions/ADR-0010-btrfs-only-world-engine.md ARCHITECTURE.md` (matches in both files, exit 0)
4. **PASS** — `grep -n 'BtrfsEngine\|SubvolumeDriver\|KernelBtrfsDriver\|BtrfsRequired' ARCHITECTURE.md` (all 4 symbols present, exit 0)
5. **PASS** — `test -f docs/decisions/ADR-0002-world-engine-ioc-btrfs.md` (exit 0)
6. **PASS** — `test ! -e docs/decisions/ADR-0002-world-engine-ioc-incus-and-reflink.md` (exit 0)
7. **PASS** — `! grep -rniE 'incus|zfs|ficlone|qemu|kvm|statefulram|reflink' ARCHITECTURE.md docs/VISION.md docs/decisions/` (0 matches, exit 0)
8. **PASS** — `! grep -rniE 'option [ab]|spike [ab]' ARCHITECTURE.md docs/VISION.md docs/decisions/` (0 matches, exit 0)
9. **PASS** — `uvx --offline zensical build --strict` (`No issues found`, exit 0)
10. **PASS** — `uv run --no-project tools/check_docs_honesty.py` (`OK`, exit 0)
11. **PASS** — `uv run --no-project tools/plan.py validate` (`plan OK`, exit 0)
12. **PASS** — `uv run --no-project tools/guard.py` (`OK`, exit 0)
