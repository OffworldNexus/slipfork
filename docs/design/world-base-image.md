# `L0` World Base Image, `crun` Backing-Service Containers & the Unified Btrfs World Subvolume (`docs/design/world-base-image.md`)

- **Status:** Accepted Design Specification (`M3.8`, companion to `ADR-0009` and `ADR-0010`)
- **Visibility:** Internal design spec (`exclude_docs` in `zensical.toml`). Per **Core Rule #4 (Zero Vaporware in Published Docs)**, nothing here appears in published `docs/` or `README.md` until the implementing nodes ship and pass the Quad Review Gate.

---

## 1. Decision Summary & Zero-Menu User Experience

A Slipfork world is **exactly one Btrfs subvolume**: the `L0` application root filesystem tree and the mutable PostgreSQL / Redis / S3 state directories live side by side inside it (`ADR-0010` §2), so a single `BTRFS_IOC_SNAP_CREATE_V2` seals or forks the whole world — rootfs and data together — in constant time.

The `L0` application root filesystem **always** derives from a `Dockerfile` declared beside a `Forkfile` `service`. There is **no** Slipfork-maintained language base image and **no** host-`/usr` fallback (Iron Law #3).

### 1.1 Per-Service Rootfs (Zero Extra Config)

Each `service <id> ./dir` names its own build context, so no image selector is needed:

```text title="Forkfile"
postgres pg { database app }

service api ./api {
    port 8000
    route /api
    run /app/.venv/bin/uvicorn app.main:app --host 0.0.0.0 --port $port
}

service worker ./worker {
    run /app/.venv/bin/python -m app.worker
}
```

Services that share an identical `(context, dockerfile, stage, build_env)` build and cache one rootfs.

```dockerfile title="api/Dockerfile"
FROM python:3.12-slim AS api
WORKDIR /workspace
COPY pyproject.toml uv.lock ./
RUN uv sync --frozen --no-dev
```

### 1.2 Multi-Stage Compiled Services

`service <id> ./dir` builds `<dir>/Dockerfile`; `dockerfile <relpath>` and `stage <name>` select another file or an earlier stage. A compiled service names the stage that still carries its toolchain:

```dockerfile title="api/Dockerfile"
FROM rust:1.85-slim-bookworm AS builder
WORKDIR /workspace
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
```

```text title="Forkfile"
service api ./api {
    dockerfile Dockerfile
    stage builder
    port 8000
    run /app/target/release/acme-api
}
```

---

## 2. Host Contract: One Btrfs Subvolume per World (`ADR-0010` §§2–4; `M3.12`/`M3.13` target state)

Everything in this section is `M3.12`–`M3.13` target state: `SubvolumeDriver`, `KernelBtrfsDriver`, `BtrfsEngine`, `EngineError::BtrfsRequired`, and the seed/clone/destroy lifecycle do not exist in `crates/slipfork-engine/src/` yet.

### 2.1 The Host Prerequisite: One Btrfs Mount, Enforced in Three Stages

The state directory must reside on a **Btrfs** filesystem mounted with unprivileged subvolume deletion enabled and atime updates disabled (`ADR-0010` §4):

```bash
sudo mkfs.btrfs -L slipfork /dev/<device>
sudo mount -o user_subvol_rm_allowed,noatime /dev/disk/by-label/slipfork /var/lib/slipfork
```

That requirement is enforced in three distinct stages, and nothing below claims more than its own stage proves:

1. **`slipforkd` boot — magic only.** `slipforkd` constructs its engine from an explicit `--state-dir` through the one constructor, `BtrfsEngine::with_driver_and_clock(..., Arc<dyn SubvolumeDriver>, Arc<dyn MonotonicClock>)`, which calls `SubvolumeDriver::ensure_supported(&state_dir)` **before** any `<state_dir>/worlds/<id>` directory, snapshot, or control directory exists. `ensure_supported` reads `statfs(state_dir).f_type` and requires `BTRFS_SUPER_MAGIC` (`0x9123683e`); anything else fails closed with `EngineError::BtrfsRequired { path, actual_fstype, mount_hint }` naming the mount command above (transcript in §6.12). It runs no scratch round-trip, so a Btrfs mount that merely lacks `user_subvol_rm_allowed` **boots** — that gap is not a boot gate, and `M3.12` asserts the negative directly (a driver whose create/snapshot/destroy return `EPERM` still passes `ensure_supported`).
2. **`slipfork doctor` — live scratch round-trip.** The thin `slipfork` CLI constructs `KernelBtrfsDriver` at its composition boundary and calls `slipfork_engine::btrfs::probe_with(&dyn SubvolumeDriver, state_dir)` inside `doctor::run`, holding no `WorldEngine` at all, so `doctor` still explains a refused boot (`M3.15` target state). `BtrfsProbeReport` (shape owned by `ADR-0010` §4) is the single report `doctor` renders: its `user_subvol_rm_allowed` evidence is `None` off Btrfs and, on Btrfs, `Ok(())` or the raw `errno` from a live create → RO snapshot → clear-RO → destroy round-trip in a scratch subvolume. Because the report is produced through `&dyn SubvolumeDriver`, the field mapping is unit-testable against a test driver.
3. **First teardown — `EPERM`.** If the option is absent and `doctor` was skipped, the first unprivileged destroy — transient base-seed cleanup, `destroy_world`, or Phase-2 GC — fails closed inside `SubvolumeDriver::destroy_subvolume` with `EngineError::SubvolumeDestroyDenied { path, errno, mount_hint }` — the raw `errno` (`EPERM`) plus the same mount hint. The variant mapping, and why this is **not** `EngineError::BtrfsRequired`, is catalogued once in §6.12. Nothing is leaked silently and nothing is retried in a degraded mode. `M3.12` proves the mapping with an injected `EPERM`; a real mount without the option is the only way to observe the kernel's own `EPERM`, so Phase 1 asserts the mapping, not a second mount.

Alongside the Btrfs report, the `M3.15` `doctor` surface renders only the host-probe group: `unshare_userns` and `netns` from `HostProbe` (`OsHostProbe`: fixed `PATH`, `LC_ALL=C`, AppArmor `userns` hint). `doctor` emits no `crun` field and performs no host `crun` probe: static `crun` is a world-rootfs asset, never a probe target and never a `crun` resolved from host `PATH` (`ADR-0010` §4). `crates/slipfork::runtime::runtime_assets` provisions the pinned binary at `<state_dir>/cache/runtime/crun-1.20-x86_64-ff8407185ec4.bin`, and `DockerfileStageBuilder` stages and SHA-256-verifies it at the guest path `WorldRootfs.crun_bin = /.slipfork/bin/crun` (`ADR-0009` §1).

There is **no** probe-and-degrade path, no alternate filesystem, and no fallback engine.

### 2.2 Layout: One Subvolume Holds Rootfs **and** State

```text
<state_dir>/                                  # Btrfs mount (§2.1)
├── cache/                                    # content-addressed plain directories (§7)
├── dumps/                                    # verified <sha256>.pgdump + decompressed <sha256>.pgcustom (§7)
├── worlds/<world_id>/                        # ONE writable Btrfs subvolume per live world
│   ├── rootfs/                               # $UID:$GID-owned L0 tree, read-only at `/` in the world
│   ├── state/pgdata/<id>/                    # bind source for /var/lib/postgresql/data in postgres:<v>
│   ├── state/redis/<id>/                     # bind source for /data in redis:<v>
│   ├── state/s3/                             # bind source for /data in the seaweedfs container
│   └── home/                                 # world $HOME (/.slipfork/home)
├── worlds/seed-<base_key_sha12>/             # TRANSIENT seed subvolume (sealed, then destroyed, §2.3)
├── worlds/.seed.lock                         # serializes base seeding
├── snapshots/<snapshot_id>/                  # sealed read-only subvolumes: <project>-base-<base_key_sha12>, fixtures, stages
├── artifacts/<digest>/                       # sealed read-only artifact subvolumes (Phase 2 M3.2)
├── ctl/seed-<base_key_sha12>/                # ephemeral seed control dir: created and removed by slipforkd, never by the engine
└── ctl/<world_id>/                           # ephemeral control dir (0700), OUTSIDE every subvolume, created by slipforkd and injected
    ├── netns.sock (0600)  world-init.json (0600)  init.ready
    ├── oci/<service_id>/config.json           # generated OCI runtime bundle per service
    ├── oci-run/                               # `crun --root` state (never snapshotted)
    ├── mountpoints/                           # RO OverlayFS lowerdir supplying guest mount targets + .old_root
    └── merged/                                # world `/` (read-only overlay) and pivot_root staging
```

Rules that follow from this layout:

1. **Everything inside `<subvol>` is a plain directory.** `BTRFS_IOC_SNAP_CREATE_V2` does not snapshot nested subvolumes recursively, so a nested subvolume inside a world would be silently omitted from its snapshot. Nothing inside a world subvolume may itself be a subvolume.
2. **State paths are fixed and world-local.** Inside a world's mount namespace — and therefore inside every `crun` container that receives the matching bind — backing-service state appears under `/var/lib/slipfork/state/{pgdata/<id>,redis/<id>,s3}`, plain directories in the same subvolume, so sealing the world captures a consistent rootfs + database + cache + bucket tree in one operation.
3. **Ephemeral state stays outside.** `netns.sock` (`0600`), `world-init.json` (`0600`), `init.ready`, OCI runtime state, the OverlayFS mountpoint tree, and the `pivot_root` staging directory (`.old_root`, pre-created at `mountpoints/.old_root` and visible as `/.old_root` after the pivot) live in the daemon-injected control directory — `WorldSpec.control_dir` for a world, `BaseSeedSpec.control_dir` for the seed world — which `slipforkd` creates and the engine only consumes (never re-derived and never created inside the engine), so `freeze_world` never captures sockets or runtime scratch. `slipfork-world-init` writes `init.ready` last — after `pivot_root` and before `netns.sock` accepts — and `slipforkd` bounds its wait on that file. The three control/socket paths are disjoint by construction (`ADR-0010` §2).

### 2.3 The `BTRFS_IOC_*` Lifecycle, Mapped Onto Methods

The ioctl constants, `struct btrfs_ioctl_vol_args_v2` layout, flag values, and ordering are normatively specified in `ADR-0010` §3. This table maps each world operation onto the single `WorldEngine` / `SubvolumeDriver` method that performs it.

| World operation | Operation performed | Detail |
|---|---|---|
| **Seed and seal a base** (`BtrfsEngine`, once per base key, never per world) | Create a subvolume under `<state_dir>/worlds`, fill it, then seal it read-only (`ADR-0010` §3) | The engine decides neither *which* base nor *what goes in it*: the daemon-assembled `BaseSeedSpec` argument names the base (`project` + `base_key_sha12`, §2.3.2) and carries every input, including the `app_rootfs` tree to copy. Under `flock` on `<state_dir>/worlds/.seed.lock`, `BtrfsEngine` creates `worlds/seed-<base_key_sha12>` from `seed.base_key_sha12`, copies `seed.app_rootfs` (the `cache/rootfs/<l0_merkle_sha256>/app` tree, checked against `seed.rootfs`) into `<subvol>/rootfs` (`$UID:$GID`-owned plain tree), creates `seed.state_subdirs` and `home/`, then starts the seed world's backing services from `seed.service_specs` and, for each `BaseSeedSource::Dump`, runs the one restore sequence of §3.2 in that database's container through `exec_in_service` on the read-only `/dumps/<sha256>.pgcustom` mount (`ADR-0008` §2; non-zero exit or `ServiceNotRunning` fails closed). It then writes `seed.complete` as the commit marker (after the restore, before the seal; the cache's own `manifest.json` stays in `cache/rootfs/<digest>/`, §7), calls `freeze_world` on the seed world — the same path that stops the restore-fed services and seals read-only — to produce `snapshots/<project>-base-<base_key_sha12>` (formatted from `seed.project` + `seed.base_key_sha12`), and destroys the seed subvolume. Guest mount *targets* are never seeded here: they come only from `<control_dir>/mountpoints` (§4.1). A seed subvolume without `seed.complete` is destroyed and retried (fault-injected through the test `SubvolumeDriver`), so a partial seed is never observable. That sealed base — not the seed — is the clone source |
| **Read-only snapshot** (`create_snapshot`, `freeze_world`) | Seal a world subvolume read-only into `<state_dir>/snapshots/<snapshot_id>` (`ADR-0010` §3) | Source `<state_dir>/worlds/<world_id>`, destination parent `<state_dir>/snapshots`. `freeze_world` stops `WorldProcess` services, then `OciContainer` services (`SIGTERM` → bounded grace), then seals. `CHECKPOINT;` belongs to the seed/restore path only (§3.2) |
| **Writable world clone** (`clone_world`) | Clone a sealed subvolume into `<state_dir>/worlds/<world_id>` writable (`ADR-0010` §3) | Source is a sealed read-only subvolume — `snapshots/<project>-base-<base_key_sha12>` for a fresh project world, or a `[fixtures.*]`/stage snapshot. `clone_world` never copies or re-materializes `rootfs/`: the `L0` tree and state directories arrive by CoW. It creates `<subvol>/state/...` from `WorldSpec.state_subdirs` (§2.3.2) before any service starts, then gets a fresh PID 1 and its own `WorldSpec.control_dir` |
| **Teardown** (`destroy_world`; unreferenced snapshots via Phase-2 `M3.2.13`) | Clear read-only, then destroy the subvolume (`ADR-0010` §3) | `destroy_world` reaps the world's PID 1 namespace and removes `<control_dir>`, then calls `SubvolumeDriver::destroy_subvolume`, which reads the subvolume flags, clears read-only when set, and destroys. Read-only subvolumes **cannot** be destroyed until the flag is cleared; unprivileged deletion additionally requires the mount option of §2.1 (`EPERM` otherwise, §6.12). No CLI verb ever holds the driver: the GC entry point is the engine's, over the injected `Arc<dyn SubvolumeDriver>` (§7.2 #4) |

Read-only snapshot and writable clone are **distinct method signatures** (`SubvolumeDriver::snapshot_ro` / `snapshot_writable`); there is no `set_readonly(bool)` mode switch and no second filesystem-kind query — clearing read-only belongs inside `destroy_subvolume` (`ADR-0010` §3). Because destroy is one driver call, the clear-RO-then-destroy ordering is asserted through the injected `SubvolumeDriver`, not through filesystem state.

#### 2.3.1 Base Key: Rootfs Digest × Service Images × Seed Dumps

A sealed base is only reusable when *everything* restored into it is unchanged, so the base key covers all three inputs and the subvolume name is derived from that key — never from the rootfs digest alone:

```text
base_key_sha256 = SHA256(
    "slipfork-base:v2\0"
 || project.as_str()                                   || "\0"
 || l0_merkle_sha256                                   || "\0"   # WorldRootfs.digest (ADR-0009 §3)
 || u32le(service_images.len())              || "\0"
 || for (key, oci) in service_images.sorted(): key || "\0" || oci.digest || "\0"
 || u32le(databases.len())                   || "\0"
 || for db in databases.sorted(): db.id || "\0" || db.source.tag() || "\0"   # BaseSeedSource::tag()
)
```

`service_images` is keyed by `BackingServiceKey` and each `OciRootfs` carries the resolved `digest`, so a tag re-pin or a SeaweedFS pin bump changes the key even when the tag string does not. `BaseSeedSource::tag()` is the one accessor the key reads off the carrier (§2.3.2): `Clean => "clean"`, `Dump { sha256, .. } => sha256` (the verified compressed digest). Length prefixes make the encoding injective: no two distinct inputs produce the same byte string.

- **Who computes it:** a pure `BaseKey::derive` in `crates/slipfork-engine` (golden-vector unit tests: baseline, `pg_clusters[].version` bump, dump change, and rootfs-digest change yield four distinct keys). `slipforkd` (Composition Root) calls it — it holds `WorldRootfs` and the resolved `ResourcePlan` — and passes the result onward twice: `base_key_sha12` (+ `project`) inside the `BaseSeedSpec` that seeds and seals the base (§2.3.2), and the resulting sealed-base name as `WorldSpec.snapshot` (`SnapshotId`) for the `clone_world` that follows (§2.3 clone row). `BtrfsEngine` owns materialization only: seed, restore, freeze (§2.3). An engine test asserts the sealed subvolume's name is exactly `<project>-base-<base_key_sha12>`, observable through `list_children`.
- **Why all three inputs:** a `Forkfile` `pg_clusters[].version` bump, a new digest-pinned SeaweedFS image, or a `--refresh-dump` stream changes the base's *contents* without changing `l0_merkle_sha256`. Because the key covers all three, each such change yields a different `<project>-base-<base_key_sha12>`, so a seal never collides with, or silently reuses, an existing base.
- **Clean versus dump-backed bases are distinct:** a `--no-dump` base records `clean` per database (`docs/reference/cli.md` §"Seed dumps and sealed bases"), so the next default `up` seals its own dump-backed base instead of reusing a clean one.

#### 2.3.2 Seed Inputs Are Explicit (`BaseSeedSpec`)

`BtrfsEngine` reads no `Forkfile`, no `<state_dir>/dumps` listing, and no `ResourcePlan`: the daemon already assembles exactly these inputs for the shipped seed path — the rootfs tree to copy (`app_rootfs`), the base identity it derived (`project`, `base_key_sha12`), the seed control dir, and the state/service/database payload (`state_subdirs`, `service_specs`, `databases`) — so it passes them as one carrier **argument** to the base-preparation call:

```rust
// crates/slipfork-engine::btrfs_engine (M3.13) — every field is an explicit input, never derived
pub struct BaseSeedSpec {
    pub control_dir: PathBuf,             // <state_dir>/ctl/seed-<base_key_sha12>, created by slipforkd (ADR-0010 §2)
    pub project: ProjectSlug,             // names the sealed destination: snapshots/<project>-base-<base_key_sha12>
    pub base_key_sha12: String,           // first 12 hex of BaseKey::derive (§2.3.1); names worlds/seed-<base_key_sha12>
    pub rootfs: RootfsDigest,             // WorldRootfs.digest the copied tree must match (ADR-0009 §3)
    pub app_rootfs: PathBuf,              // <state_dir>/cache/rootfs/<l0_merkle_sha256>/app — the plain tree to copy
    pub state_subdirs: Vec<PathBuf>,      // subvolume-relative: state/pgdata/<id>, state/redis/<id>, state/s3
    pub service_specs: Vec<ServiceSpec>,  // the seed world's backing-service specs (argv, readiness, RO dump mounts)
    pub databases: Vec<BaseDatabaseSeed>, // one per Forkfile pg_databases[] entry
}

pub struct BaseDatabaseSeed {
    pub database: String,                 // Forkfile pg_databases[].id
    pub service: String,                  // ServiceSpec.id of its pg_clusters container
    pub source: BaseSeedSource,
}

pub enum BaseSeedSource {
    Clean,                                // initdb only
    Dump { path: PathBuf, sha256: String }, // verified <sha256>.pgcustom, mounted RO into that container
}

impl BaseSeedSource {
    /// The base-key component (§2.3.1): `"clean"` or the verified compressed dump digest.
    pub fn tag(&self) -> &str {
        match self {
            Self::Clean => "clean",
            Self::Dump { sha256, .. } => sha256,
        }
    }
}
```

The engine forms its two names by **formatting injected values**, never by parsing a path: `worlds/seed-{seed.base_key_sha12}` and `snapshots/{seed.project}-base-{seed.base_key_sha12}` (§2.2, §2.3.1). It also fails closed if `seed.app_rootfs` is not the `cache/rootfs/<seed.rootfs.hex()>/app` tree, so a mismatched digest/tree pair can never be sealed under the wrong base name.

`BaseSeedSpec` is the only carrier of seed inputs and it never rides on `WorldSpec`: `service_specs` and `databases` are consumed by base preparation alone. `WorldSpec` keeps only `state_subdirs` — the subvolume-relative state directories `clone_world` creates before any service starts — so a world that clones an already-sealed base carries no seed payload. `slipforkd` is the single author of both `state_subdirs` values (one per carrier), so base preparation reads `seed.state_subdirs` and `clone_world` reads `WorldSpec.state_subdirs` from one computed list. Adding `WorldSpec.state_subdirs` and the `BaseSeedSpec` carrier shape to `ARCHITECTURE.md` §3.3 is a `lead` followup in `plan/nodes/M3/M3.8/RESULT.md`, since `ARCHITECTURE.md` is outside this node's paths.

The seed world is booted by the same machinery as every world, so it has a real `WorldHandle` for `spawn_service` / `exec_in_service` / `freeze_world`:

1. **Entry point.** `BtrfsEngine::ensure_base(&self, seed: &BaseSeedSpec) -> Result<(), EngineError>` (`M3.13`) — a public **inherent** method on the concrete engine, never a `trait WorldEngine` method: base preparation is a storage-adapter concern whose inputs only the composition root assembles, and the domain port stays world-lifecycle only. `slipforkd` calls it immediately before `clone_world`; it is idempotent, so no caller has to know whether the base is already sealed.
2. **Already sealed?** Under `flock`, `ensure_base` lists `<state_dir>/snapshots`; when `<project>-base-<base_key_sha12>` is present it returns `Ok(())` without seeding, and an `EEXIST` from the seal is treated as success (a restarted daemon never re-seeds).
3. **Boot.** It creates `worlds/seed-<base_key_sha12>`, spawns `slipfork-world-init` under the `ADR-0010` §6 namespace set inside the daemon-created `seed.control_dir`, and gets a `WorldHandle` exactly as `clone_world` does.
4. **Three mount hops for every dump.** Host `<state_dir>/dumps/<sha256>.pgcustom` → the seed world's mount table at `/dumps/<sha256>.pgcustom` → the container's `readonly_mounts` entry whose source is that **in-world** path. Containers run inside the world's mount namespace, where the host path no longer resolves after `pivot_root`; the same rule makes `/var/lib/slipfork/state/...` the container bind source for state, with `<subvol>/state/...` only its host-side origin.
5. **Teardown.** After `freeze_world`, `ensure_base` destroys the seed subvolume. The seed control directory is `slipforkd`'s: it created `<state_dir>/ctl/seed-<base_key_sha12>/` and removes it once the seed world is gone.

### 2.4 Cost Model: One ioctl, O(1) Metadata, No File Walk

Because a world's rootfs tree and all three state directories share one subvolume, sealing or forking a world is a metadata operation: Slipfork copies no file content, walks no extent, and traverses no directory tree. The four figures that justify this cost model — read-only snapshot, writable clone, time to a warm-SQL world, and per-world RSS — are measured, defined, and owned in exactly one place: `docs/research/btrfs-crun-benchmarks.md` §2, including what each figure does and does not cover. This document cites that record and copies no value from it; `M3.19` re-measures all four on the Btrfs VM and reports deviations against it.

### 2.5 Composition Root & DI Tree

`slipforkd` boot has exactly one `BtrfsEngine` construction site, and it uses the one constructor: `BtrfsEngine::with_driver_and_clock(state_dir, host_uid, host_gid, world_init_bin, driver: Arc<dyn SubvolumeDriver>, clock: Arc<dyn MonotonicClock>)`. The composition root supplies `Arc::new(KernelBtrfsDriver::new())` plus the one clock it also injects into the rootfs builder and OCI store, so a missed wire is a compile error rather than a second time source (`ADR-0010` §5). Test harnesses pass the same constructor a `TestTreeDriver` and a deterministic clock. There is exactly one other sanctioned construction of the storage seam — the thin CLI's `main.rs` building `KernelBtrfsDriver` for `slipfork doctor` — and it builds **no** `WorldEngine` (`ADR-0010` §4):

```rust
// crates/slipfork (slipforkd) — the SOLE BtrfsEngine construction site, in wiring order
let base_images: Arc<dyn BaseImageBuilder> = Arc::new(DockerfileStageBuilder { // ADR-0009 §1; no driver, plain dirs only
    exporter, oci_store, abi_probe, hasher,
});
let engine = Arc::new(BtrfsEngine::with_driver_and_clock(                    // M3.13; one constructor: driver + the one clock
    state_dir.clone(), host_uid, host_gid, world_init_bin,
    Arc::new(KernelBtrfsDriver::new()), Arc::clone(&clock),                 // fails closed BtrfsRequired
)?);
let world_engine: Arc<dyn WorldEngine> = engine.clone();                    // world-lifecycle port, injected downstream

let rootfs: WorldRootfs = base_images.materialize(&rootfs_request, &mut on_event)?; // plain cache/rootfs + cache/oci dirs
let base_seed = BaseSeedSpec {                                              // §2.3.2 — assembled here, never derived later
    control_dir: state_dir.join("ctl").join(format!("seed-{base_key_sha12}")),
    project: project.clone(),
    base_key_sha12: base_key_sha12.clone(),
    rootfs: rootfs.digest.clone(),
    app_rootfs: rootfs.app_rootfs.clone(),                                  // <state_dir>/cache/rootfs/<hex>/app
    state_subdirs: state_subdirs.clone(),
    service_specs: service_specs.clone(),
    databases: databases.clone(),
};
engine.ensure_base(&base_seed)?;                                            // idempotent; a sealed base is a no-op
let handle = world_engine.clone_world(&spec)?;                              // spec.snapshot = the base just sealed (§2.3)
```

- **Zero ambient reads inside the engine.** `crates/slipfork-engine::rootfs` and `crates/slipfork-engine::btrfs_engine/` never call `std::env::var`, never read the wall clock, never inspect `current_dir()`, and never guess a path: `state_dir`, the control directory (`WorldSpec.control_dir` / `BaseSeedSpec.control_dir`), `host_uid`/`host_gid`, `world_init_bin`, `WorldSpec.mounts`, `WorldSpec.state_subdirs`, `BaseSeedSpec`, `WorldRootfs`, and the `InheritedEnv` allowlist (`PATH`/`TERM`/`LANG`, resolved once by the composition root) all arrive as constructor or call arguments (monotonic `Instant` only, for bounded grace and readiness timeouts). `unsafe` lives only in `src/btrfs.rs` (all `statfs` and `BTRFS_IOC_*` calls) and `src/linux_cow.rs`, each with a per-call `// SAFETY:` proof; every other module is `#![forbid(unsafe_code)]`.
- **One probe owner.** `slipforkd` boot and `slipfork doctor` both go through `crates/slipfork-engine::btrfs` (`ensure_supported` / `probe_with`), so the magic check, mount hint, and scratch-subvolume round-trip exist once.
- **One CoW seam, shared upward.** `Arc<dyn SubvolumeDriver>` is the whole seam: `probe`, `ensure_supported`, `create_subvolume`, `snapshot_ro`, `snapshot_writable`, `destroy_subvolume`, `list_children` (`ARCHITECTURE.md` §3.3). The same handle is injected into Phase 2 `M3.2`'s `BtrfsArtifactStore` (`<state_dir>/artifacts/<digest>`) and into the Phase-2 GC entry point (§7.2 #4), so artifacts, worlds, and collection all use one trait with no second cloning or deletion port.
- **The builder stops at plain directories.** `DockerfileStageBuilder` holds no `SubvolumeDriver` and produces only `<state_dir>/cache/rootfs/<l0_merkle_sha256>/app` and `<state_dir>/cache/oci/<sha256>/rootfs` (`ADR-0010` §2). It never creates, seeds, seals, or destroys a subvolume: seeding, the dump restore, and sealing are `BtrfsEngine`'s alone (§2.3). There is no second base pipeline.
- **One enumeration-filter owner.** `SubvolumeDriver::list_children(parent)` returns every immediate child name and filters nothing; `BtrfsEngine` owns the only exclusion policy (`worlds/seed-*`, `worlds/.seed.lock`, `ADR-0010` §3) and hands a world set — never a raw directory listing — to its Phase-2 GC planner. Control directories are never candidates: they live under `<state_dir>/ctl/<id>`, not under `worlds/` or `snapshots/`.
- **One owner per manifest mapping.** `slipforkd`'s service-spec builder — not the engine — translates `Forkfile` `resources:` into `ServiceSpec` (`BackingServiceKey`, `argv`, `readiness`, `StateSubdirMount`); the engine resolves the key in `WorldRootfs::service_images` and synthesizes that container's runtime bundle and mounts from the spec.
- **Test double by injection.** `BtrfsEngine::with_driver_and_clock(..., Arc<dyn SubvolumeDriver>, clock)` receives a `TestTreeDriver` and a deterministic clock through `ARCHITECTURE.md` §1's Cargo Feature & Test-Scaffolding Rule (`#[cfg(test)]`, `tests/support/`, or an unpublished `publish = false` testkit crate — never a Cargo feature on a production crate); real kernel behaviour is asserted by `critical_btrfs_live` on the Btrfs VM (`M3.14`, `M3.19`), and `M3.22` deletes the workspace's last test-scaffolding Cargo feature.

There is no optional capability to branch on: a host without Btrfs is not a degraded Slipfork — it is not a supported Slipfork host, and the daemon says so before creating anything.

---

## 3. Three-Layer Composition & Single Ownership Per Tooling Class

Slipfork composes every world from three strictly orthogonal layers, with **exactly one mechanism per tooling category**:

| Category | Canonical Mechanism | Never Done |
|---|---|---|
| **App runtime & OS libraries** (`python`, `node`, `php`, `.venv`, C `.so` deps) | **Layer 1:** Project's own `Dockerfile` stage (`world.image` + `world.stage`) | Never shared from host `/usr` or `/lib`; never synthesized from a language menu |
| **Compiled-language build/test toolchains** (`cargo`/`rustc`, `go`, `javac`) | **Layer 1:** Select the `Dockerfile`'s named build stage via `world.stage = "builder"` | Never installed ad hoc by Slipfork into a stripped runtime stage |
| **Backing services & DB/cache CLI tools** (`postgres`, `psql`, `pg_restore`, `initdb`, `redis-server`, `redis-cli`, `seaweedfs`) | **Layer 2:** Daemonless **`crun`** OCI containers in the world's shared `127.0.0.1` `netns` and private `ipcns`, mapped from `Forkfile` `resources:`, state in the world subvolume | Never `apt`/`apk`-installed into the app rootfs; never run via `dockerd`-in-Docker |
| **Headless browsers** (`Playwright` Chromium / Firefox / WebKit) | **Host browser pool** opening isolated `BrowserContext`s against `http://test-<slot>.<project>.localhost[:<port>]` (`ADR-0006` §4.2) | Never installed inside world rootfs; never cold-launched per world |
| **Host package/toolchain caches** (`~/.cargo`, `~/.rustup`, `uv`, `pnpm` store) | **Layer 3:** Per-invocation `--host-caches` read-only mounts gated by a fail-closed ELF/libc ABI probe | Never configured in `Forkfile`; never mounted RW; never mounted across mismatched libc ABIs |

### 3.1 Layer 1 — Fidelity Layer (`slipfork-world` Stage Synthesis)

Slipfork never edits the user's `Dockerfile` in the worktree. It writes a temporary build file under `<state_dir>/cache/tmp/Dockerfile.<uuid>` containing the user's `Dockerfile` bytes verbatim followed by one machine-generated stage:

```dockerfile
# --- appended by Slipfork (never written to the repository) ---
FROM <target_stage> AS slipfork-world
COPY --from=slipfork-runtime /crun /.slipfork/bin/crun
COPY --from=slipfork-runtime /slipfork-world-init /.slipfork/bin/slipfork-world-init
```

`slipfork-runtime` is a **named build context**, supplied by the builder as a directory holding exactly the pinned static `crun` and `slipfork-world-init` payloads:

```bash
docker buildx build \
  --file <scratch_dockerfile> --target slipfork-world \
  --build-context slipfork-runtime=<state_dir>/cache/tmp/runtime.<uuid> \
  --output type=local,dest=<staging_dir> <context>
```

The export extracts a plain directory tree owned by the invoking **`$UID:$GID`** — the sole evidence in this document is the BuildKit probe recorded in §8.1, whose ownership line records `remy:remy` for the extracted tree — and the builder re-verifies the exported `/.slipfork/bin/crun` against its compile-time SHA-256. Zero `apt` or `apk` commands run on `<target_stage>`.

The builder's work ends here: it caches the extracted tree immutably at `<state_dir>/cache/rootfs/<l0_merkle_sha256>/app` (§7) and never touches a subvolume. `BtrfsEngine` then seeds that plain directory once into the transient seed subvolume and seals it (§2.3), so the sealed base — not the cache directory and not a builder-owned snapshot — is what every world clones. Inside every world the `L0` tree is mounted read-only at `/`, so forks and `TestSlot`s share one rootfs without copying a single file.

### 3.2 Layer 2 — Daemonless `crun` Backing-Service Containers (`Forkfile` `resources:`)

`Forkfile` `resources:` map deterministically to official OCI images with **zero new syntax**:

| `Forkfile` Declaration | Resolved OCI Image | In-World Loopback Bind | State Subdir in World Subvolume (RW) | Container Path | `OciRootfs.readiness` (`M3.13`/`M3.15` target state) |
|---|---|---|---|---|---|
| `resources.pg_clusters: [{ id, version: "<v>" }]` | `docker.io/library/postgres:<v>` (e.g. `postgres:18`) | `127.0.0.1:5432` (one cluster per world) | `<subvol>/state/pgdata/<id>` → `/var/lib/slipfork/state/pgdata/<id>` | `/var/lib/postgresql/data` | `Exec { port: 5432, argv: ["psql", "-U", "postgres", "-Atc", "SELECT 1"], timeout }` |
| `resources.redis_clusters: [{ id, version: "<v>" }]` | `docker.io/library/redis:<v>` (e.g. `redis:8`) | `127.0.0.1:6379` (one cluster per world) | `<subvol>/state/redis/<id>` → `/var/lib/slipfork/state/redis/<id>` | `/data` | `Exec { port: 6379, argv: ["redis-cli", "PING"], timeout }` |
| `resources.s3_buckets: [...]` (when non-empty & local S3 active) | `docker.io/chrislusf/seaweedfs:<PINNED_VER>@sha256:<PINNED_HEX>` | `127.0.0.1:9000` | `<subvol>/state/s3` → `/var/lib/slipfork/state/s3` | `/data` | `TcpPort { port: 9000, timeout }` |

The shipped SeaweedFS argv never resolves DNS: `weed server -dir=/data -ip=127.0.0.1 -ip.bind=127.0.0.1 -s3 -s3.port=9000 -s3.ip.bind=127.0.0.1`.

One cluster per world is the supported shape in this design: the shipped service specs give every `pg_clusters` / `redis_clusters` entry the same in-world port (`5432` / `6379`) inside the one shared `netns`, so a second cluster needs its own loopback port, which no node currently assigns.

Each `OciRootfs` materialized into `WorldRootfs::service_images` carries `{ digest, rootfs, config, readiness }`. `readiness` has exactly one author — `BaseImageBuilder`, at materialization — and `slipforkd`'s service-spec builder passes `oci_rootfs.readiness` through to `ServiceSpec.readiness` unchanged, with **zero `match BackingServiceKey` ladder** (`ADR-0002` §2, `ADR-0009` §2). That builder owns the manifest mapping above: the `ServiceSpec.id` (`pg_clusters.<cluster>`, `redis_clusters.<id>`, `s3_buckets` — the shipped ids; one PostgreSQL/Redis service per declared cluster and one SeaweedFS service when any bucket is declared), per-service `argv` such as the `weed server` line below, and `StateSubdirMount`; `BackingServiceKey` (`postgres:<v>`, `redis:<v>`, `seaweedfs`) stays the image identity used to look the container rootfs up in `WorldRootfs::service_images`. `BtrfsEngine` synthesizes the container's runtime bundle from the spec (§4.1 #5).

`postgres:<v>` starts through its own `docker-entrypoint.sh postgres`, whose superuser comes from `POSTGRES_USER` (default `postgres`), so the readiness probe's `-U postgres` resolves whether `$UID` maps to `remy` or `runner`; the cluster never depends on the OS user name that owns the state directory. Each container's OCI runtime bundle is synthesized per service by `BtrfsEngine::spawn_service` (`M3.13` target state) at `<control_dir>/oci/<service_id>/config.json` (namespaces, identity maps, `readonly: true` rootfs, state and readiness mounts), and `crun --root <control_dir>/oci-run` keeps all runtime state inside the world's control directory.

**Zero OS coupling:** each backing service runs inside its own read-only OCI rootfs via static `/.slipfork/bin/crun --cgroup-manager=disabled` while omitting `"network"` from `linux.namespaces`, so an `alpine:3.21` (`musl`) application stage talks to `postgres:18` (`Debian Trixie glibc`) and `redis:8` over `127.0.0.1:5432` and `:6379` inside the world's `netns` (exercised end to end by the transcript in §8.3). Each container declares `namespaces: [user, pid, mount, ipc, uts]` with identity `uidMappings`/`gidMappings` (`$UID -> $UID`); `ipc` is mandatory, not optional — the System V shared-memory collision it prevents is recorded once in §8.2 trap 3 (`ADR-0010` §6; proven live by `critical_btrfs_live`, §8.4 #4).

`WorldEngine::freeze_world` stops services in `ADR-0010` §3's order and seals the whole world subvolume read-only (§2.3) — one snapshot, rootfs and all backing-service state included. The `CHECKPOINT;` that flushes WAL before a seal belongs to the seed/restore path (§3.2), not to `freeze_world`.

Client operations execute inside the backing service's own rootfs (`WorldEngine::exec_in_service` / `service_entry_command`), so the app image never needs database client packages and never hits `pg_restore` version skew. **The seed restore sequence — stated once, here, and cross-referenced everywhere else (§2.3, §7.2):** run in the cluster's container, `psql -U postgres -h 127.0.0.1 -d postgres -tAc "SELECT 1 FROM pg_database WHERE datname = '<db>'"` and, when absent, `createdb -U postgres -h 127.0.0.1 <db>`; then `pg_restore --no-owner --no-privileges --clean --if-exists -U postgres -h 127.0.0.1 -d <db> /dumps/<sha256>.pgcustom`; then `psql -U postgres -h 127.0.0.1 -d <db> -v ON_ERROR_STOP=1 -c "CHECKPOINT;"`. Any non-zero exit or `EngineError::ServiceNotRunning` fails the seed. `slipfork db` → `psql` and `slipfork redis` → `redis-cli` use the same seam, and every call fails closed with `EngineError::ServiceNotRunning` when the target container is not running.

### 3.3 Layer 3 — Opt-In Host-Cache Passthrough (`--host-caches`) & Real ABI Probe

Passing `--host-caches` to `slipfork up`, `slipfork fork`, `slipfork test`, or `slipforkd` attaches existing host cache directories **read-only** (`MountMode::ReadOnly`) inside the world:
- `~/.cargo/registry`, `~/.cargo/git`, `~/.cargo/bin`, and `~/.rustup` (`CARGO_HOME` pointed at writable world `$HOME/.cargo` symlinking RO `registry`/`git`/`bin`; `RUSTUP_HOME=/.slipfork/host-cache/rustup`)
- `~/.cache/uv` and `~/.local/share/uv` (`UV_CACHE_DIR`, `UV_PYTHON_INSTALL_DIR`)
- `~/.local/share/pnpm/store` (pnpm `--store-dir` / `npm_config_store_dir`)

**Fail-Closed ELF & Libc ABI Probe (`AbiProbe`):** Before mounting `--host-caches`, Slipfork parses the ELF header (`e_machine` + `PT_INTERP`) and C library version symbol table (`GLIBC_2.*` max version in `libc.so.6` or `musl` version string in `ld-musl-*.so.1`) of both the host (`/bin/sh`) and `<app_rootfs>/bin/sh` in `#![forbid(unsafe_code)]` Rust:
1. `host.arch == world.arch` (`x86_64` vs `aarch64`), **and**
2. `host.libc_family == world.libc_family` (`glibc` vs `musl`), **and**
3. For `glibc`: `host.glibc_version <= world.glibc_version` (a host binary linked against `glibc 2.41` fails at runtime inside a `bookworm` `glibc 2.36` rootfs with `GLIBC_2.38 not found`); for `musl`: `host.musl_major_minor == world.musl_major_minor`.

Any mismatch **fails closed** with `BaseImageError::HostCacheAbiMismatch` (§6.11) — never silently skipping or mounting incompatible binaries.

---

## 4. Current-User (`$UID:$GID`) Invariant & Writable Path Contract

**Normative rule:** A world **never** runs as `root` (`0:0`) and **never** runs as the `Dockerfile`'s `USER` directive (such as `USER appuser` / `1001` or `USER www-data` / `33`). Every hook, component service, `slipfork exec`, `slipfork shell`, and `crun` backing-service process executes as the invoking developer's exact **`$UID:$GID`** — which is also why unprivileged, `user_subvol_rm_allowed` subvolume management works without any privileged helper.

### 4.1 What `$UID:$GID` Execution Implies

1. **Read-Only `/` (`EROFS`) Shared Across All Forks:** `slipfork-world-init` first makes the mount tree private (`mount --make-rprivate /`), then mounts a **read-only two-lowerdir OverlayFS** at `<control_dir>/merged`: `mount -t overlay overlay -o userxattr,lowerdir=<control_dir>/mountpoints:<subvol>/rootfs <control_dir>/merged` — zero `upperdir`, zero `workdir`, zero per-file copy, and therefore no writable `/` inode. It then stacks the ordered mount set (§4.1 #5), `chdir`s into `<control_dir>/merged`, `pivot_root(".", ".old_root")`, and detaches the previous root with `umount -l /.old_root` so no earlier root — including the writable subvolume — stays reachable. `.old_root` is the put-old directory supplied by `mountpoints/`, so it exists before the pivot. Any unexpected write to `/usr`, `/etc`, `/var/log`, or `/opt` fails immediately with `EROFS` (`Read-only file system`) instead of polluting the shared `L0` tree or silently diverging forks. Every guest mount *target* (`/proc`, `/dev`, `/sys`, `/.slipfork/{home,oci,host-cache}`, tmpfs paths, `spec.mounts`, `rootfs.host_caches`, and one mountpoint per backing-service state directory) is created as an empty directory under `<control_dir>/mountpoints` before the overlay is mounted — state *content* stays in `<subvol>/state/...`, state *mountpoints* live in `mountpoints/` — so `slipfork-world-init` never mounts onto a non-existent path and never `mkdir`s on a read-only `/`.
2. **Synthesized Read-Only `/etc/passwd` & `/etc/group` NSS Overlays:** Because `$UID` (e.g. `1000` on laptops or `1001` in CI) is frequently absent from minimal container `/etc/passwd` files — causing `postgres` `initdb`, `psql`, `git`, `ssh`, Python `getpass.getuser()`, and Node `os.userInfo()` to fail (`could not look up effective user ID`) — Slipfork writes `<control_dir>/mountpoints/etc/{passwd,group}` preserving image system entries while appending `<host_username>:x:$UID:$GID:slipfork world user:/.slipfork/home:/bin/sh`. The overlay supplies that view at `/etc/passwd` and `/etc/group` inside the world; each `crun` container bind-mounts the same two files read-only.
3. **`$UID:$GID` Ownership Comes From the Export — Never a Permission Rewrite:** BuildKit's `--output type=local` exporter writes the extracted tree `$UID:$GID`-owned (§8.1 transcript), so Slipfork performs **no** recursive `chmod`, no read/traverse normalization, and no pre-boot path scan over the extracted rootfs (`ADR-0009` §3, `M3.25`). If a path the runtime needs is nevertheless unreadable to `$UID` — the reachable case is a mode that denies the owner too, e.g. a `chmod 000` directory the component reads — it surfaces **at boot** as the operating system's own `EROFS` or `EACCES` naming that path — a runtime file-access failure, never a `BaseImageError` and never a pre-boot contract violation. `M3.14`'s boot conformance suite owns that fixture.
4. **Complete Path Writability Table Inside a World:**

| Path Inside World | Mode | Backed By | Purpose |
|---|---|---|---|
| `/` (`/usr`, `/lib`, `/etc`, `/opt`, image `/app`) | **Read-Only (`EROFS`)** | Read-only overlay of `mountpoints/` + `<subvol>/rootfs` | Immutable runtime binaries, `.venv`, system libs |
| `<worktree_path>` (repo or `.worktrees/<fork>`) | **Read-Write (`$UID:$GID`)** | Host VCS checkout, bind-mounted RW | Live source code, migrations, generated files (zero `root:root` files) |
| `/.slipfork/home` (`$HOME`) | **Read-Write (`$UID:$GID`)** | `<subvol>/home` (inside the world subvolume) | Tool state, `.psql_history`, runtime dotfile writes |
| `/tmp`, `/var/tmp`, `/run`, `/dev/shm` | **Read-Write (`1777`/`0755`)** | Per-world `tmpfs` (never captured by a snapshot) | Sockets, PID files, Gunicorn/PHP-FPM/temp uploads |
| `SLIPFORK_OUTPUT_DIR` & slot coverage dir (`M3.2`) | **Read-Write (`$UID:$GID`)** | `.slipfork/runs/<run_id>/<slot>/{output,coverage}` | Surviving Playwright traces, reports, coverage shards |
| `/var/lib/slipfork/state/{pgdata/<id>,redis/<id>,s3}` (bind source for the containers) | **Read-Write (`$UID:$GID`)** | `<subvol>/state/...` — `postgres:<v>` sees `/var/lib/postgresql/data`, `redis:<v>` and `seaweedfs` see `/data` | PostgreSQL / Redis / S3 state that the world snapshot captures atomically |

5. **One Ordered Mount Manifest — the Sole Owner of Post-Overlay Mount Order:** `slipfork-world-init` has a fixed, parameter-free prologue (`mount --make-rprivate /`, the read-only overlay, `chdir` into it, `pivot_root`, `umount -l /.old_root`). Everything mounted after that prologue — `/proc`, `--rbind /dev`, `--rbind /sys`, tmpfs (`/tmp`, `/var/tmp`, `/run`, `/dev/shm`), `/.slipfork/home`, `/.slipfork/oci/<service_id>`, `rootfs.host_caches`, one mountpoint per backing-service state directory, `spec.mounts`, and the overlay-supplied `/etc/{passwd,group}` view — is serialized **once**, in order, into `<control_dir>/world-init.json` (mode `0600`) by the engine, which also creates the `<control_dir>/mountpoints` tree (directory target for a directory source, file placeholder for a file source). `slipfork-world-init` consumes that manifest in the order it records and hardcodes no second table and no second ordering (`ADR-0010` §2).

---

## 5. World Image Contract & Supported Distro List

### 5.1 Supported App Stage Linux Distributions (Detected from `/etc/os-release`, Never Assumed)

`BaseImageBuilder` parses `<app_rootfs>/etc/os-release` (`ID` + `VERSION_CODENAME` / `VERSION_ID`) and verifies `<app_rootfs>/bin/sh` is an executable ELF binary (or symlink resolving inside `<app_rootfs>` to one). The supported `L0` app stage distribution set is:

| Distribution (`ID`) | Supported Releases (`VERSION_CODENAME` / `VERSION_ID`) | C Library Family | Typical Base Tags |
|---|---|---|---|
| `debian` | `bookworm` (`12`), `trixie` (`13`) | `glibc` | `debian:trixie-slim`, `python:3.14-slim-trixie`, `rust:1.85-slim-bookworm`, `node:22-bookworm-slim`, `php:8.4-cli` |
| `ubuntu` | `jammy` (`22.04`), `noble` (`24.04`) | `glibc` | `ubuntu:24.04`, `ubuntu:noble` |
| `alpine` | `3.20`, `3.21` | `musl` | `alpine:3.21`, `node:22-alpine3.21`, `php:8.4-fpm-alpine`, `golang:1.24-alpine3.21` |

`FROM scratch`, `gcr.io/distroless/*`, EOL releases (`bullseye`, `focal`, `alpine:3.18`), and non-Debian/Ubuntu/Alpine stages fail closed naming the supported table above and suggesting `world.stage = "builder"` when a multi-stage `Dockerfile` ends in a `distroless`/`scratch` packaging stage.

### 5.2 The 6-Rule World Image Contract

| # | Contract Rule | Static / Boot Verification | Failure Variant |
|---|---|---|---|
| **1** | **Named target `Dockerfile` stage** | Selected `Dockerfile` stage must carry `FROM <base> AS <name>` (inferred from final stage when named, or selected via `world.stage = "<name>"`). | `UnnamedTargetStage`, `UnknownTargetStage` |
| **2** | **Supported Linux distro & executable `/bin/sh`** | `<app_rootfs>/etc/os-release` must match `debian` (`bookworm`\|`trixie`), `ubuntu` (`jammy`\|`noble`), or `alpine` (`3.20`\|`3.21`), and `/bin/sh` must exist and be executable. Both causes carry the same variant with a different `detail`. | `UnsupportedDistro { detail }` |
| **3** | **Unprivileged `$UID:$GID` & read-only `/` runtime** | **Static, pre-boot (the only `BaseImageError` check):** image `ENTRYPOINT` / `CMD` (when used because `run_command` is empty) must not invoke root-only privilege-drop wrappers (`gosu`, `su-exec`, `sudo`, `chpst`). **Boot, no `BaseImageError`:** `/` is `EROFS` and POSIX mode bits are the runtime file-access enforcement, so an unreadable path surfaces as the operating system's own `EROFS`/`EACCES` naming that path (`ADR-0009` §3, `M3.25`; §4.1 #3). | `RootAtRuntimeEntrypoint` (static only) |
| **4** | **Zero baked host paths or single-origin URLs (`ADR-0005`)** | Image `ENV` and `WORKDIR` must not bake `BASE_URL`, `*.localhost` URLs, `DATABASE_URL`, `REDIS_URL`, `AWS_S3_ENDPOINT_URL`, or host user paths (`/Users/*`, `/home/*`). | `BakedOriginOrHostPath` |
| **5** | **App deps in `Dockerfile`; backing services in `Forkfile`** | App stage must not install or launch `postgres`/`postgresql`, `redis-server`, `weed` (SeaweedFS), or `dockerd`; backing services belong in `Forkfile` `resources:`. | `EmbeddedBackingService` |
| **6** | **`EXPOSE` / `HEALTHCHECK` agrees with `Forkfile`** | Every TCP port in image `EXPOSE` / `HEALTHCHECK` must appear in `Forkfile` `components[].internal_ports` or `ingresses[].target.port` for components using that image. | `ExposeIngressMismatch` |

---

## 6. Failure-Mode Catalogue & Exact User-Facing Error Strings

Every contract violation halts **before** creating partial world directories or subvolumes and prints an actionable diagnostic naming the exact 1-line fix. The CLI prints `slipfork: ` followed by the error's `Display` text; each block below shows the `Display` text alone. `BaseImageError` diagnostics begin with their own `error:`. Daemon-side `WorldConfigError` world-selection variants (`NoBuildableImage` on `up`, `AmbiguousWorldImage`, `UnknownWorldImage`) carry no prefix and **no path**. Parse-time `WorldConfigError` variants are not rendered by the daemon at all: they surface through `crates/slipfork/src/project.rs`'s `load_slipfork_config`, which wraps the parser message as `<path>: {err}` (and `cannot read <path>: {err}` when the file is unreadable), so a parse-time diagnostic carries the `Forkfile` path while a world-selection diagnostic never does. The published rendering in `docs/reference/manifest.md` shows the `slipfork: ` wrapper. Diagnostics that belong to `M3.12`–`M3.15` target state (`EngineError::BtrfsRequired`, `EngineError::SubvolumeDestroyDenied`, the `doctor` renderings) carry that label inline.

### 6.1 No Buildable Image in `Forkfile` (`WorldConfigError::NoBuildableImage` on `up`; `BaseImageError::NoBuildableImage` in the builder)
Both variants carry this body; the builder's copy begins with its own `error: ` prefix, and `up`'s carries none.
```text
Forkfile declares no buildable Dockerfile under 'images:'; a project that cannot be built into an image is not a project Slipfork can fork
  fix: add a named Dockerfile stage and declare it in Forkfile:
    images:
      - id: app
        context: .
        dockerfile: Dockerfile
```

### 6.2 Multiple `BuildImage` Entries Without `world.image` (`WorldConfigError::AmbiguousWorldImage`)
```text
Forkfile declares 2 buildable images ('api', 'worker') and Forkfile does not select one
  fix: add `world.image = "api"` under `[world]` in Forkfile
```

### 6.3 Unknown `world.image` Selector (`WorldConfigError::UnknownWorldImage`)
```text
Forkfile sets `world.image = "backend"`, which matches no buildable image in Forkfile (available: 'api', 'worker')
  fix: set `world.image = "api"` in Forkfile
```

### 6.4 Rule 1 — Unnamed Final `Dockerfile` Stage (`BaseImageError::UnnamedTargetStage`)
```text
error: World Image Contract #1 violated in Dockerfile:24: final stage `FROM python:3.14-slim-trixie` has no stage name
  fix: change Dockerfile:24 to `FROM python:3.14-slim-trixie AS app` (or set `world.stage = "<stage>"` in Forkfile)
```

### 6.5 Rule 1 — Unknown `world.stage` (`BaseImageError::UnknownTargetStage`)
```text
error: World Image Contract #1 violated: `world.stage = "dev"` not found in Dockerfile (declared stages: 'builder', 'runtime')
  fix: set `world.stage = "builder"` in Forkfile
```

### 6.6 Rule 2 — Unsupported Base Distro or `distroless`/`scratch` (`BaseImageError::UnsupportedDistro`)
Both causes share the variant and the fix line; the `detail` differs:

```text
error: World Image Contract #2 violated in stage 'runtime' (Dockerfile:18): detected distro 'fedora:41'; supported distros are debian (bookworm, trixie), ubuntu (jammy, noble), and alpine (3.20, 3.21)
  fix: set `world.stage = "builder"` in Forkfile or switch `FROM` to a supported Debian, Ubuntu, or Alpine base
```

```text
error: World Image Contract #2 violated in stage 'runtime' (Dockerfile:18): missing /etc/os-release + /bin/sh; supported distros are debian (bookworm, trixie), ubuntu (jammy, noble), and alpine (3.20, 3.21)
  fix: set `world.stage = "builder"` in Forkfile or switch `FROM` to a supported Debian, Ubuntu, or Alpine base
```

### 6.7 Rule 3 — Root-at-Runtime Entrypoint (`BaseImageError::RootAtRuntimeEntrypoint`)
```text
error: World Image Contract #3 violated in stage 'api': ENTRYPOINT ["docker-entrypoint.sh", "gosu", "appuser"] requires root to switch UIDs, but Slipfork always executes as host $UID:$GID (1000:1000) with a read-only '/' filesystem
  fix: set an unprivileged command in Forkfile `modes.local.components.api.run_command` (or remove `gosu`/`su-exec`/`chown` from ENTRYPOINT)
```

### 6.8 Rule 4 — Baked Host Path or Origin/Service URL (`BaseImageError::BakedOriginOrHostPath`)
The `what` slot names the offending entry; two representative transcripts:

```text
error: World Image Contract #4 violated in stage 'api': baked image ENV `BASE_URL=http://localhost:8000` conflicts with Slipfork single-origin and ResourceCoordinates injection
  fix: delete ENV `BASE_URL=http://localhost:8000` from Dockerfile and reference `${{ resources::api.url }}` under `modes.all.envs` in Forkfile
```

```text
error: World Image Contract #4 violated in stage 'api': baked image host path `/home/alice/app` conflicts with Slipfork single-origin and ResourceCoordinates injection
  fix: delete host path `/home/alice/app` from Dockerfile and reference `${{ resources::api.url }}` under `modes.all.envs` in Forkfile
```

### 6.9 Rule 5 — Backing Service Installed Inside App `Dockerfile` (`BaseImageError::EmbeddedBackingService`)
```text
error: World Image Contract #5 violated in stage 'api': detected `/usr/lib/postgresql/18/bin/postgres` (backing service daemon) installed inside the app image; Slipfork runs postgres, redis, and seaweedfs as isolated crun containers from Forkfile
  fix: remove `postgresql` server installation from Dockerfile and declare `resources.pg_clusters` in Forkfile
```

### 6.10 Rule 6 — `EXPOSE` / `HEALTHCHECK` Disagrees with `Forkfile` (`BaseImageError::ExposeIngressMismatch`)
```text
error: World Image Contract #6 violated for image 'api': Dockerfile EXPOSE declares port 3000, but Forkfile component 'api' declares internal_ports [8000] (ingresses target 8000)
  fix: change `EXPOSE 3000` to `EXPOSE 8000` in Dockerfile (or update `components.api.internal_ports` / `ingresses` in Forkfile to 3000)
```

### 6.11 Layer 3 — `--host-caches` ELF/Libc ABI Mismatch (`BaseImageError::HostCacheAbiMismatch`)
```text
error: `--host-caches` ABI check failed: host is x86_64 glibc 2.41, but world stage 'api' is x86_64 musl 1.2 (alpine:3.21); mounting host toolchains would fail to execute
  fix: re-run without `--host-caches` (or use a matching x86_64 glibc >= 2.41 base such as `debian:trixie-slim`)
```

### 6.12 Host Contract — State Directory Not on Btrfs (`EngineError::BtrfsRequired`, `M3.12` target state)
```text
Slipfork requires the state directory to live on Btrfs: statfs('/home/alice/.cache/slipfork/acme') reports f_type 0x58465342 (xfs), not BTRFS_SUPER_MAGIC 0x9123683e
  fix: mount a Btrfs filesystem for Slipfork state and re-run: sudo mount -o user_subvol_rm_allowed,noatime /dev/disk/by-label/slipfork /var/lib/slipfork
```

The canonical `mkfs.btrfs` + `mount` snippet is §2.1; this hint names only the mount option the kernel enforces (`EngineError::BtrfsRequired.mount_hint`).

A missing `user_subvol_rm_allowed` on an otherwise-Btrfs mount is **not** this error — it is the other two stages of §2.1: `slipfork doctor`'s scratch round-trip reports `BtrfsProbeReport::user_subvol_rm_allowed` as `Some(Err(errno))` (rendered `subvolume_ioctl: failed (errno 1)`), and if `doctor` was skipped, `SubvolumeDriver::destroy_subvolume` fails closed on the first unprivileged destroy with `EngineError::SubvolumeDestroyDenied { path, errno, mount_hint }` — the raw `errno` (`EPERM`) plus the same mount hint, never `BtrfsRequired`, which reports a filesystem type this host does not have (`ADR-0010` §4–§5). There is no skip-and-leak fallback: the world's directory and control directory stay until the mount is fixed.

---

## 7. Content-Addressed Caching & Invalidation Model (Non-`tmpfs`)

All `L0` rootfs trees and backing-service OCI bundles live under `<state_dir>/cache/` **on the same Btrfs filesystem as `<state_dir>/worlds` and `<state_dir>/snapshots`** — the magic check at daemon boot covers the mount containing `state_dir`, and a nested mount elsewhere that is not Btrfs fails closed at its own first use. The word "cache" refers to content-addressed reuse, not to memory: this tree is plain directories on disk, never `tmpfs`, and never `/tmp`.

```text
<state_dir>/cache/
├── runtime/
│   └── crun-1.20-x86_64-ff8407185ec4.bin      # compile-time SHA-256 verified static crun (0755)
├── oci/
│   ├── tags.json                               # {"docker.io/library/postgres:18": "sha256:e3d24f..."}
│   └── <image_sha256>/
│       ├── rootfs/                             # $UID:$GID-owned extracted OCI rootfs (RO-mounted into crun)
│       └── config.json                         # upstream image ENV/PATH metadata
└── rootfs/
    └── <l0_merkle_sha256>/
        ├── app/                                # $UID:$GID-owned extracted slipfork-world rootfs (BtrfsEngine seeds it into <subvol>/rootfs)
        └── manifest.json                       # ImageContractMetadata (committed atomically via rename)
```

`<state_dir>/dumps/` (§2.2) is a sibling plain directory of `cache/`: verified `<sha256>.pgdump` archives and their decompressed `<sha256>.pgcustom` files, never a subvolume and never a snapshot source on their own.

### 7.1 `RootfsDigest` (`l0_merkle_sha256`)

`RootfsDigest` is owned by `crates/slipfork-engine::rootfs` (`RootfsDigest::compute`, `ADR-0009` §3): a SHA-256 over the domain `"slipfork-l0-rootfs:v1\0"` and the `Dockerfile`/context/stage/env/`crun`/`world-init`/arch inputs, every component `NUL`-terminated. Its bare hex (`.hex()`, not the `sha256:` `Display` form) names `cache/rootfs/<l0_merkle_sha256>/`. This document depends on one property only: a change to any hashed input yields a different digest, and a source-only edit outside them yields the same one (§7.2).

### 7.2 Invalidation Rules & `ADR-0003` / `ADR-0008` Composition

1. **Source-only edit outside `Dockerfile` `COPY` inputs (or ignored by `.dockerignore`):** `l0_merkle_sha256` is **unchanged**, so no rebuild happens at all; the live worktree bind-mount reflects the edit immediately inside running worlds.
2. **`Dockerfile`, lockfile (`uv.lock`, `Cargo.lock`, `pnpm-lock.yaml`), or `world.stage` edit:** `l0_merkle_sha256` changes → `DockerfileStageBuilder` runs `docker buildx build` (reusing BuildKit layer cache) into `.staging-<uuid>`, validates the 6-rule contract, and atomically renames to `cache/rootfs/<new_sha256>`. The builder's work ends there. Because the new digest changes the base key (§2.3.1), `BtrfsEngine` seeds and seals a **new** `snapshots/<project>-base-<base_key_sha12>` (§2.3 — never copy-over, so the old base is untouched). That invalidates `DagPlanner`'s `base` fixture root and downstream `[fixtures.*]` while leaving `[artifacts.front]` untouched (`ADR-0003`). Worlds already cloned keep their existing CoW lineage and rootfs until destroyed; the superseded base is collected by Phase-2 GC once no subvolume clones from it (§7.2 #4).
3. **`Forkfile` `pg_clusters[].version` change (e.g. `"17"` → `"18"`):** Resolves/extracts `postgres:18` into `cache/oci/<sha256>/rootfs` once and changes the base key's service-image identity (§2.3.1), so a fresh base is seeded (`initdb`, or the §3.2 restore sequence followed by `freeze_world`) without rebuilding `app_rootfs`.
4. **Offline, Fork & Staged GC Guarantee:** Once `tags.json` + `cache/oci/<sha256>/rootfs` + `cache/rootfs/<l0_merkle_sha256>/app` + `<state_dir>/dumps/<sha256>.pgdump` exist on disk, `slipfork up`, `slipfork fork`, and `slipfork test` resolve the cached digest and clone subvolumes; `DockerfileStageBuilder` is the only component that ever calls Docker, and it runs only on a cache miss (its cache-hit contract test counts exporter calls and asserts zero on a hit). Collection is **staged**, and `slipfork prune` is never a second holder of the storage seam:
   - **Phase 1 (`M3.13`–`M3.15`, `M3.19`):** `DaemonControl::prune` reaps detached world records only, each through the daemon's own `WorldEngine::destroy_world`, and `PruneReport.snapshots` stays empty. Sealed bases under `snapshots/`, fixture/stage snapshots, artifacts, and `cache/` are untouched, so `M3.19` asserts only that the scenario's own world subvolumes are gone (`btrfs subvolume list <state_dir> | grep -v '/snapshots/'` empty); the digest-keyed sealed base legitimately survives.
   - **Phase 2 (`M3.2.13`):** `ADR-0010` §3 is normative for the delegation. `M3.2`'s `DaemonService::prune` is the orchestrator: it computes the liveness sets and drives `WorldEngine::prune_snapshots` (subvolumes over `SubvolumeDriver`), `ArtifactStore::prune_artifacts` (`artifacts/*`), and `BaseImageBuilder`'s cache pruning (`cache/{rootfs,oci}`). `M3.2.13` owns `crates/slipfork-engine/src/prune.rs` — the typed planner (`PrunePlan`) behind `prune_snapshots`, plus the daemon wiring and `--dry-run`/idempotency. Plain directories are never deleted through the subvolume driver, and `BaseImageBuilder` is never injected into `BtrfsEngine`. Liveness includes each live world's `WorldSpec.snapshot`, so a base any live world cloned from is retained; an unknown on-disk entry is reported, not deleted.
   - **Re-seed after collection:** a sealed base is derived state, never the only copy. `slipforkd` re-materializes a collected `.pgcustom` through `SeedDumpStore::materialize` — the port method `DiskSeedDumpStore` implements with its bounded `decompress_bounded`, returning `<sha256>.pgcustom` (`crates/slipfork-ospp/src/dump.rs`, `disk_dump.rs`) — before seeding, then calls `BtrfsEngine::ensure_base`, which re-runs the §3.2 restore sequence and seals via `freeze_world` (`ADR-0008` §2). The next `up` therefore works with or without the dumps directory's `.pgcustom` files.

---

## 8. Measured Host Feasibility Probes (Zero Assumed Values)

Two probe sets back this design, and they are kept distinct:

1. **Runtime probes** — static `crun`, BuildKit ownership, namespace and `/etc/passwd` traps — run on this repository's Linux container host on `2026-11-15` (`kernel 6.12.117+deb13-amd64`, `x86_64`, unprivileged LXC container, `/` on XFS, `uid=1000` `remy:remy`, `docker 29.9.3`, `buildx v0.39.0`) inside `.slipfork/probe-crun-e2e`; the tree was removed after measurement, so absolute paths below are as captured. These probe results are recorded in `plan/nodes/M2/M2.24/RESULT.md` Summary item 3, `plan/nodes/M2/M2.27/RESULT.md`, and `docs/knowledge/LESSONS.md`.
2. **Storage measurements** — read-only snapshot, writable clone, warm-SQL world, and per-world RSS on a Btrfs host — recorded once, with units, provenance, and open questions, in `docs/research/btrfs-crun-benchmarks.md` §2. That report owns the four figures; §2.4 cites it and copies no value, and `M3.19` re-measures them on the Btrfs VM against it.

### 8.1 Static `crun` + `buildx --output type=local` Ownership Verification

```console
$ curl -fsSL https://github.com/containers/crun/releases/download/1.20/crun-1.20-linux-amd64-disable-systemd -o "$SCRATCH/crun" && chmod +x "$SCRATCH/crun"
$ file "$SCRATCH/crun" && sha256sum "$SCRATCH/crun"
crun: ELF 64-bit LSB executable, x86-64, version 1 (SYSV), statically linked, for GNU/Linux 3.10.0, stripped
ff8407185ec474d7c60f697032b7b1fc1fb028fbd0bf13c7d5da9a7d9a479e38  crun
$ docker buildx build --quiet -f Dockerfile.pg --output "type=local,dest=$SCRATCH/pg-rootfs" "$SCRATCH/build"
# Extracted postgres@sha256:e3d24f6906d0760bf4da0dae872415391ededefad23fd471c47dc53b0ddf3c42,
# redis@sha256:1f88291b4c57bdf249f02eb085a8669889b2d2954e91d4de5118a5da6b78dd53, and alpine:3.21 — all files owned remy:remy (1000:1000).
```

### 8.2 Kernel/Runtime Traps (Transcripts for Traps 1–2; Observed Kernel Behaviour for Trap 3)

Traps 1–2 below carry the console output captured on the 2026-11-15 host. Trap 3 is not a transcript: it is the observed kernel behaviour recorded in `docs/research/btrfs-crun-benchmarks.md` §5 #1, and its live proof is `critical_btrfs_live` (§8.4 #4).

1. **Trap 1 — Omitting `user` + `pid` namespaces in `crun` `config.json` when `geteuid() == 1000`:**
   - Observed failure with `namespaces: [mount, ipc, uts]` only (`crun --debug --cgroup-manager=disabled`):
     ```text
     non root user need to have an 'user' namespace
     newgidmap: write to gid_map failed: Operation not permitted
     newuidmap: write to uid_map failed: Operation not permitted
     setresuid to `0`: Invalid argument
     ```
   - Adding `{"type": "user"}` with `uidMappings`/`gidMappings` `[{"containerID": 1000, "hostID": 1000, "size": 1}]` without `{"type": "pid"}` advanced past UID setup and failed at `mount 'proc' to 'proc': Operation not permitted` (`EPERM`).
   - **Verified fix:** Declare `namespaces: [user, pid, mount, ipc, uts]` (omitting `network` so every container shares the world's `127.0.0.1`) + identity `uidMappings`/`gidMappings` (`1000 -> 1000`) + `--cgroup-manager=disabled`.
2. **Trap 2 — Missing `$UID` entry in `postgres:18` `/etc/passwd`:**
   - Observed failure on `initdb` without `/etc/passwd` overlay:
     `initdb: could not look up effective user ID 1000: user does not exist` (`rc=1`).
   - **Verified fix:** Bind-mount a read-only synthesized `/etc/passwd` appending `remy:x:1000:1000:slipfork world user:/tmp:/bin/sh` (this probe form used `HOME=/tmp`; the shipped overlay uses the field layout in §4.1 #2) while keeping `root.readonly = true`.
3. **Trap 3 — `ipc` is not optional for parallel worlds (observed kernel behaviour, no transcript here):** two `postgres` containers binding their own `127.0.0.1:5432` in separate network namespaces still contend for System V shared memory (`shmget` key `90000` / port-derived keys) unless each world also gets a private IPC namespace. Hence `--ipc` (`CLONE_NEWIPC`) in the world `unshare` set and `{"type": "ipc"}` in every `crun` container (`ADR-0010` §6; recorded in `docs/research/btrfs-crun-benchmarks.md` §5 #1).

### 8.3 End-to-End Unprivileged World Probe (`postgres:18` + `redis:8` + Cross-Distro `alpine:3.21`)

```console
$ unshare --map-user=1000 --map-group=1000 --keep-caps --mount --net --ipc --pid --fork --mount-proc bash -euo pipefail -c '
    ip link set lo up
    "$CRUN" --cgroup-manager=disabled --root "$CROOT" run -b "$SCRATCH/bundles/initdb" pg-init >/dev/null
    "$CRUN" --cgroup-manager=disabled --root "$CROOT" run -b "$SCRATCH/bundles/pg" pg-18 >"$SCRATCH/pg.log" 2>&1 &
    "$CRUN" --cgroup-manager=disabled --root "$CROOT" run -b "$SCRATCH/bundles/redis" redis-8 >"$SCRATCH/redis.log" 2>&1 &
    # ... wait for 127.0.0.1:5432 and 127.0.0.1:6379 ...
    psql -h 127.0.0.1 -p 5432 -U postgres -d postgres -Atc "CREATE TABLE t(v text); INSERT INTO t VALUES (\$\$from-crun-pg18\$\$); CHECKPOINT; SELECT split_part(version(), \$\$ \$\$, 1) || \$\$ \$\$ || split_part(version(), \$\$ \$\$, 2) || \$\$ | row=\$\$ || v FROM t;"
    redis-cli -h 127.0.0.1 -p 6379 SET k "from-crun-redis8" >/dev/null
    echo "redis_get=$(redis-cli -h 127.0.0.1 -p 6379 GET k)"
    "$CRUN" --cgroup-manager=disabled --root "$CROOT" run -b "$SCRATCH/bundles/app-probe" app-alpine
  '
CREATE TABLE
INSERT 0 1
CHECKPOINT
PostgreSQL 18.6 | row=from-crun-pg18
redis_get=from-crun-redis8
PRETTY_NAME="Alpine Linux v3.21"
127.0.0.1 (127.0.0.1:5432) open
+PONG

$ stat -c '%U:%G %n' "$SCRATCH/state/pgdata/PG_VERSION" "$SCRATCH/state/redis/appendonlydir"
remy:remy /home/remy/offworld-nexus/slipfork/.slipfork/probe-crun-e2e/state/pgdata/PG_VERSION
remy:remy /home/remy/offworld-nexus/slipfork/.slipfork/probe-crun-e2e/state/redis/appendonlydir
```

A `musl` app stage, a `glibc` PostgreSQL 18, and Redis 8 speak over one loopback with no shared `/usr`, no package manager, and no privilege escalation — the composition this document specifies. The host `psql`/`redis-cli` calls are deliberate: this probe checks the world `netns` loopback from outside every container, whereas the shipped path runs those clients inside the backing service's own rootfs (§3.2).

### 8.4 Storage & Namespace Acceptance Criteria Owned by `critical_btrfs_live` (`M3.14`, `M3.19`)

The storage and IPC claims below are acceptance criteria for `critical_btrfs_live`, which does not exist yet (`M3.14`); a non-Btrfs host cannot host them, and no fast suite pretends otherwise:

1. `statfs(<state_dir>).f_type == 0x9123683e`, and the probe report's `btrfs_super_magic` field is `true` (the human rendering of that field is `M3.15`'s).
2. `BTRFS_IOC_SNAP_CREATE_V2` + `BTRFS_SUBVOL_RDONLY` yields a snapshot whose `/rootfs` and `/state` reject writes (`EROFS` on the sealed copy), while a write to `/usr` inside a *running* world also fails `EROFS`.
3. Two sibling writable clones of one sealed snapshot hold independent `postgres:18` data — `INSERT` in one is invisible in the other.
4. Both clones bind `:5432` in their own `netns` and reach readiness concurrently, and `readlink /proc/<init_pid>/ns/ipc` differs between the two worlds (`--ipc`, `CLONE_NEWIPC`). The assertion is namespace identity plus the user-visible property (two `postgres:18` containers ready at once); no test reaches into SysV shared-memory state itself.
5. Teardown clears read-only via `BTRFS_IOC_SUBVOL_SETFLAGS` and destroys via `BTRFS_IOC_SNAP_DESTROY_V2`. The scenario destroys the RO snapshot it created in item 2 (so the clear-RO path is exercised), then destroys its worlds, then runs `slipfork prune --yes`; `btrfs subvolume list <state_dir>` must exit `0` and `btrfs subvolume list <state_dir> | grep -v '/snapshots/'` must print nothing (§7.2 #4 owns exactly what Phase-1 `prune` may and may not collect).
6. A cloned world contains no nested subvolume: `btrfs subvolume list <state_dir>/worlds/<world_id>` reports no child of that world (the one-subvolume rule of §2.2 #1).
7. A sealed snapshot contains no ephemeral state: `snapshots/<snapshot_id>/` has no `ctl`, no `oci-run`, no `netns.sock`, and no `init.ready` (§2.2 #3).

Missing tooling on the VM (`docker`, `btrfs-progs`, and the SHA-256-pinned `crun` asset under `<state_dir>/cache/runtime/`, staged into the world rootfs — never a host `crun` on `PATH`) is a hard failure of the suite, never a skip. Steps 2–3 exercise the operations whose cost the research record measures (§2.4); `M3.19` re-runs the benchmark and reports any material deviation from `docs/research/btrfs-crun-benchmarks.md`.

---

## 9. Implementation Sequencing (`M3.7`–`M3.25`)

The `L0` / backing-service surfaces described here shipped in `0.1.0` by `M2.26`–`M2.29` (`WorldImageConfig`, `WorldRootfs` + `DockerfileStageBuilder`, `ServiceRuntime::OciContainer`, daemon wiring); `OciRootfs.readiness`, the unified subvolume, and the Btrfs lifecycle are `M3.13`/`M3.15` target state. Every row below owns a disjoint path set recorded in `plan/nodes/M3/<id>/NODE.md` — that file, not this table, is the authority on paths.

| Node | `depends_on` | Scope |
|---|---|---|
| **`M3.7`** *(done)* | `[]` | Design of record: `BtrfsEngine` as sole `WorldEngine`, unified per-world subvolume layout, `BTRFS_IOC_*` lifecycle, fail-closed host contract, `SubvolumeDriver` seam, `--ipc`, AppArmor `userns`. |
| **`M3.8`** *(this node)* | `[M3.7, M3.25]` | This specification: layout, base key, cache paths, mount table, probe transcripts, node sequencing. |
| **`M3.9`** *(done)* | `[M3.7, M3.25]` | Target DX: `slipfork doctor` Btrfs fields, `nsenter` world entry, `crun exec` service entry. |
| **`M3.10`** *(done)* | `[M3.7, M3.25]` | Btrfs volume + `crun` + AppArmor `userns` + persistent `--identity` harness design. |
| **`M3.11`** *(done)* | `[M3.7]` | Records the snapshot / clone / warm-SQL / RSS measurements in `docs/research/btrfs-crun-benchmarks.md`, which this document cites. |
| **`M3.20`** | `[M3.1, M3.7]` | Aligns `trait WorldEngine` (distinct RO-snapshot vs writable-clone signatures, no snapshot-mode flags). |
| **`M3.23`** | `[M3.11, M3.18, M3.19, M3.21, M3.22]` | Docs hygiene gate: purge alternative-architecture framing and stale post-pivot engine residue from `ARCHITECTURE.md`, `README.md`, `docs/`, `crates/`, `tools/`. |
| **`M3.24`** | `[M3.1]` | Bounded-wait consolidation, orphaned `world-init` reaping, and `rs-test-list` comment accuracy. |
| **`M3.25`** | `[M3.7]` | Design-of-record corrections: `doctor` host-probe DI seam, `ensure_supported` vs `probe` split, Rule 3 static-only check, staged `prune` GC, world-identity spelling. |
| **`M3.12`** | `[M3.8, M3.9, M3.20]` | `SubvolumeDriver` + `KernelBtrfsDriver` (`probe`, `ensure_supported`, `statfs` magic, `BTRFS_IOC_*`), `probe_with`/`BtrfsProbeReport`, and the test-only tree driver. |
| **`M3.13`** | `[M3.12]` | `BtrfsEngine`: base seeding, dump restore, and sealing, unified subvolume per world, RO snapshots, writable clones, state mounts, OCI bundle synthesis, `--ipc`. The `slipfork-world-init` mount namespace, NSS overlay, tmpfs, ingress bridge, and `dial_world_tcp` machinery port onto the Btrfs root rather than being re-invented. Two concerns only: base materialization is a cached constructor path (one base key → seed → restore → seal, with retry) and bundle synthesis lands in `container.rs` next to the existing `ServiceRuntime` match, leaving `btrfs_engine/` as lifecycle + namespace assembly. |
| **`M3.14`** | `[M3.13]` | Driver-injected fast conformance + real-kernel `critical_btrfs_live` suite (§8.4). |
| **`M3.15`** | `[M3.14]` | `slipforkd` wiring, `slipfork doctor` Btrfs probe, `daemon unit --only apparmor`. |
| **`M3.16`** | `[M3.10, M3.15]` | Btrfs + `crun` DO VM harness with persistent SSH identity. |
| **`M3.21`** | `[M3.14, M3.15]` | Deletes every non-`BtrfsEngine` adapter and per-file extent-clone helper after `M3.12`–`M3.15` land; the workspace then holds one engine and one CoW seam. |
| **`M3.22`** | `[M3.1, M3.15]` | Deletes the last test-scaffolding Cargo feature, leaving test doubles in `#[cfg(test)]` / `tests/support/` / an unpublished testkit crate only. |
| **`M3.17`** | `[M3.21]` | Benchmark + support registry on the Btrfs engine, failing closed off-Btrfs. |
| **`M3.18`** | `[M3.17]` | Published docs describe the shipped Btrfs + `crun` surfaces. |
| **`M3.19`** | `[M3.11, M3.16, M3.18]` | Live Btrfs VM proof of §8.4 and re-measurement against the research record. |

`slipfork test` slots ride the same substrate: Phase 2 `M3.2`'s language-blind orchestrator (`crates/slipfork-test`) freezes persistent fixture stages into read-only subvolume snapshots and clones `test-<slot>` worlds through the shared `Arc<dyn SubvolumeDriver>`; the protocol between Slipfork and frameworks is `ADR-0006` §3's `STP v1`.
