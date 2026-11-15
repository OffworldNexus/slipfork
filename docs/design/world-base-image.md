# `L0` World Base Image, `crun` Backing-Service Containers & World Image Contract (`docs/design/world-base-image.md`)

- **Status:** Accepted Design Specification (`M2.24`, companion to `ADR-0009`)
- **Visibility:** Internal design spec (`exclude_docs` in `zensical.toml`). Per **Core Rule #4 (Zero Vaporware in Published Docs)**, nothing here appears in published `docs/` or `README.md` until `M2.26`–`M2.29` ship and pass the Quad Review Gate (`M2.19`).

---

## 1. Decision Summary & Zero-Menu User Experience

A Slipfork world **always** derives its `L0` application root filesystem from a `Dockerfile` declared in the project's own `snow.yml` `images:` list. There is **no** Slipfork-maintained language base image, **no** separate registry-image mode, and **no** host-`/usr` fallback (Iron Law #3).

### 1.1 Single-Image Project (Zero `[world]` Config Needed)

When `snow.yml` declares one `BuildImage` and its final `Dockerfile` stage is named (`FROM ... AS api`), **zero lines** are needed in `slipfork.toml`:

```yaml title="snow.yml"
images:
  - id: api
    context: .
    dockerfile: Dockerfile
resources:
  pg_clusters: [{ id: main, version: "18" }]
  pg_databases: [{ id: app, cluster: main }]
  redis_clusters: [{ id: cache, version: "8" }]
  s3_buckets: [{ id: media }]
```

```dockerfile title="Dockerfile"
FROM python:3.14-slim-trixie AS api
WORKDIR /workspace
COPY pyproject.toml uv.lock ./
RUN uv sync --frozen --no-dev
```

### 1.2 Multi-Image or Multi-Stage Compiled Project (`world.image` & `world.stage`)

`[world]` in `slipfork.toml` is a **selector, never a menu**, exposing **at most two optional keys** (`#[serde(deny_unknown_fields)]`):

| Key | Type | When Required | Meaning |
|---|---|---|---|
| `world.image` | `String` | Mandatory when `snow.yml` declares `>= 2` `BuildImage` entries; inferred when `== 1` | Selects which `snow.yml` `images[].id` (`context` + `dockerfile`) builds the `L0` world rootfs |
| `world.stage` | `String` | Required when targeting an earlier stage (e.g. `"builder"`) or when the final stage is not targetable; inferred when final `FROM` has `AS <name>` | Selects the named `FROM ... AS <stage>` that Slipfork extends into `slipfork-world` |

**Rust / Go multi-stage example (retaining `cargo` + `rustc` omitted from the slim runtime stage):**

```dockerfile title="Dockerfile"
FROM rust:1.85-slim-trixie AS builder
WORKDIR /workspace
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

FROM debian:trixie-slim AS runtime
COPY --from=builder /workspace/target/release/acme-api /usr/local/bin/acme-api
```

```toml title="slipfork.toml"
[world]
image = "api"
stage = "builder" # Targets the named stage that still carries cargo + rustc for hooks and slipfork test
```

---

## 2. Three-Layer Composition & Single Ownership Per Tooling Class

Slipfork composes every world from three strictly orthogonal layers, with **exactly one mechanism per tooling category**:

| Category | Canonical Mechanism | Never Done |
|---|---|---|
| **App runtime & OS libraries** (`python`, `node`, `php`, `.venv`, C `.so` deps) | **Layer 1:** Project's own `Dockerfile` stage (`world.image` + `world.stage`) | Never shared from host `/usr` or `/lib`; never synthesized from a language menu |
| **Compiled-language build/test toolchains** (`cargo`/`rustc`, `go`, `javac`) | **Layer 1:** Select the `Dockerfile`'s named build stage via `world.stage = "builder"` | Never installed ad hoc by Slipfork into a stripped runtime stage |
| **Backing services & DB/cache CLI tools** (`postgres`, `psql`, `pg_restore`, `initdb`, `redis-server`, `redis-cli`, `seaweedfs`) | **Layer 2:** Daemonless **`crun`** OCI containers in the world's shared `127.0.0.1` `netns`, mapped from `snow.yml` `resources:` | Never `apt`/`apk`-installed into the app rootfs; never run via `dockerd`-in-Docker |
| **Headless browsers** (`Playwright` Chromium / Firefox / WebKit) | **Host browser pool** opening isolated `< 5 ms` `BrowserContext`s against `http://test-<slot>.<project>.localhost[:<port>]` (`ADR-0006` §4.2) | Never installed inside world rootfs; never cold-launched per world |
| **Host package/toolchain caches** (`~/.cargo`, `~/.rustup`, `uv`, `pnpm` store) | **Layer 3:** Per-invocation `--host-caches` read-only mounts gated by a fail-closed ELF/libc ABI probe | Never configured in `slipfork.toml`; never mounted RW; never mounted across mismatched libc ABIs |

### 2.1 Layer 1 — Fidelity Layer (`slipfork-world` Stage Synthesis)

Slipfork never edits the user's `Dockerfile` in the worktree. It writes a temporary build file under `<state_dir>/cache/tmp/Dockerfile.<uuid>` containing the user's `Dockerfile` bytes verbatim followed by one machine-generated stage:

```dockerfile
# --- appended by Slipfork (never written to the repository) ---
FROM __slipfork_runtime AS __slipfork_bins
FROM <target_stage> AS slipfork-world
COPY --from=__slipfork_bins /crun /.slipfork/bin/crun
COPY --from=__slipfork_bins /slipfork-world-init /.slipfork/bin/slipfork-world-init
```

`docker buildx build -f <scratch_dockerfile> --target slipfork-world --output type=local,dest=<staging_dir> <context>` extracts a plain directory tree owned by the invoking **`$UID:$GID`** (verified live: BuildKit's `local` exporter writes client-owned files without `root:root` ownership). Zero `apt` or `apk` commands run on `<target_stage>`.

### 2.2 Layer 2 — Daemonless `crun` Backing-Service Containers (`snow.yml` `resources:`)

`snow.yml` `resources:` map deterministically to official OCI images with **zero new syntax**:

| `snow.yml` Declaration | Resolved OCI Image | In-World Loopback Bind | CoW State Subdirectory (RW) | Client Tools Executed in This Rootfs |
|---|---|---|---|---|
| `resources.pg_clusters: [{ id, version: "<v>" }]` | `docker.io/library/postgres:<v>` (e.g. `postgres:18`) | `127.0.0.1:5432` (`5432 + idx` if multi-cluster) | `<state_dir>/pgdata/<id>` → `/var/lib/postgresql/data` | `initdb`, `pg_restore`, `psql` (`slipfork db`), `CHECKPOINT` |
| `resources.redis_clusters: [{ id, version: "<v>" }]` | `docker.io/library/redis:<v>` (e.g. `redis:8`) | `127.0.0.1:6379` (`6379 + idx` if multi-cluster) | `<state_dir>/redis/<id>` → `/data` (`--appendonly yes`) | `redis-cli` (`slipfork redis`) |
| `resources.s3_buckets: [...]` (when non-empty & local S3 active) | `docker.io/chrislusf/seaweedfs:<PINNED_VER>@sha256:<PINNED_HEX>` | `127.0.0.1:9000` (`s3 -ip.bind=127.0.0.1 -port=9000`) | `<state_dir>/s3` → `/data` | S3 HTTP API readiness probe |

**Why this eliminates the distro matrix:** Because each backing service runs inside its own read-only OCI rootfs via static `/.slipfork/bin/crun --cgroup-manager=disabled` while omitting `"network"` from `linux.namespaces`, an `alpine:3.21` (`musl`) application stage talks to `postgres:18` (`Debian Trixie glibc`) and `redis:8` over `127.0.0.1:5432` and `:6379` inside the world's `netns` with zero OS coupling. On `WorldEngine::freeze_world` (`ADR-0008` §2), `slipforkd` runs `CHECKPOINT;` inside the `postgres:<v>` container, stops all component services and `crun` containers cleanly (`SIGTERM` -> bounded grace), and seals `<state_dir>` via `ioctl(FICLONE)` (or Incus snapshot).

### 2.3 Layer 3 — Opt-In Host-Cache Passthrough (`--host-caches`) & Real ABI Probe

Passing `--host-caches` to `slipfork up`, `slipfork fork`, `slipfork test`, or `slipforkd` attaches existing host cache directories **read-only** (`MountMode::ReadOnly`) inside the world:
- `~/.cargo/registry`, `~/.cargo/git`, `~/.cargo/bin`, and `~/.rustup` (`CARGO_HOME` pointed at writable world `$HOME/.cargo` symlinking RO `registry`/`git`/`bin`; `RUSTUP_HOME=/.slipfork/host-cache/rustup`)
- `~/.cache/uv` and `~/.local/share/uv` (`UV_CACHE_DIR`, `UV_PYTHON_INSTALL_DIR`)
- `~/.local/share/pnpm/store` (`PNPM_STORE_DIR`)

**Fail-Closed ELF & Libc ABI Probe (`AbiProbe`):** Before mounting `--host-caches`, Slipfork parses the ELF header (`e_machine` + `PT_INTERP`) and C library version symbol table (`GLIBC_2.*` max version in `libc.so.6` or `musl` version string in `ld-musl-*.so.1`) of both the host (`/bin/sh`) and `<app_rootfs>/bin/sh` in `#![forbid(unsafe_code)]` Rust:
1. `host.arch == world.arch` (`x86_64` vs `aarch64`), **and**
2. `host.libc_family == world.libc_family` (`glibc` vs `musl`), **and**
3. For `glibc`: `host.glibc_version <= world.glibc_version` (a host binary linked against `glibc 2.41` fails at runtime inside a `bookworm` `glibc 2.36` rootfs with `GLIBC_2.38 not found`); for `musl`: `host.musl_major_minor == world.musl_major_minor`.

Any mismatch **fails closed** with `BaseImageError::HostCacheAbiMismatch` (§5.11) — never silently skipping or mounting incompatible binaries.

---

## 3. Current-User (`$UID:$GID`) Invariant & Writable Path Contract

**Normative rule:** A world **never** runs as `root` (`0:0`) and **never** runs as the `Dockerfile`'s `USER` directive (such as `USER appuser` / `1001` or `USER www-data` / `33`). Every hook, component service, `slipfork exec`, `slipfork shell`, and `crun` backing-service process executes as the invoking developer's exact **`$UID:$GID`**.

### 3.1 What `$UID:$GID` Execution Implies

1. **Read-Only `/` (`EROFS`) Shared Across All Forks:** On `ReflinkNetnsEngine`, `slipfork-world-init` mounts `<world_dir>/merged` via unprivileged read-only OverlayFS (`mount -t overlay overlay -o userxattr,lowerdir=<world_dir>/mountpoints:<app_rootfs>` — two lowerdirs, **zero `upperdir`**, **zero per-file copy**) and `pivot_root`s into it. Any unexpected write to `/usr`, `/etc`, `/var/log`, or `/opt` fails immediately with `EROFS` (`Read-only file system`) instead of polluting shared cache layers or silently diverging forks.
2. **Synthesized Read-Only `/etc/passwd` & `/etc/group` NSS Overlays:** Because `$UID` (e.g. `1000` on laptops or `1001` in CI) is frequently absent from minimal container `/etc/passwd` files — causing `postgres` `initdb`, `psql`, `git`, `ssh`, Python `getpass.getuser()`, and Node `os.userInfo()` to fail (`could not look up effective user ID`) — Slipfork writes `<world_dir>/etc/{passwd,group}` preserving image system entries while mapping `$UID:$GID` to `<host_username>:x:$UID:$GID::/.slipfork/home:/bin/sh`, and bind-mounts them read-only at `/etc/passwd` and `/etc/group` in both the app rootfs and every `crun` container.
3. **Read/Traverse Normalization on Extracted Layers:** During `DockerfileStageBuilder` extraction (`$UID:$GID`-owned via `buildx --output type=local`), owner read bits (`u+r`) and directory/executable traverse bits (`u+X`) are verified so restrictive `USER`-owned `0700` virtualenvs or build directories remain readable by `$UID` under read-only `/`.
4. **Complete Path Writability Table Inside a World:**

| Path Inside World | Mode | Backed By | Purpose |
|---|---|---|---|
| `/` (`/usr`, `/lib`, `/etc`, `/opt`, image `/app`) | **Read-Only (`EROFS`)** | Cached `L0` `<app_rootfs>` | Immutable runtime binaries, `.venv`, system libs |
| `<worktree_path>` (repo or `.worktrees/<fork>`) | **Read-Write (`$UID:$GID`)** | Host VCS checkout (`shift=true` on Incus) | Live source code, migrations, generated files (zero `root:root` files) |
| `/.slipfork/home` (`$HOME`) | **Read-Write (`$UID:$GID`)** | `<world_dir>/home` (CoW per world) | Tool state, `.psql_history`, runtime dotfile writes |
| `/tmp`, `/var/tmp`, `/run`, `/dev/shm` | **Read-Write (`1777`/`0755`)** | Per-world `tmpfs` | Sockets, PID files, Gunicorn/PHP-FPM/temp uploads |
| `SLIPFORK_OUTPUT_DIR` & slot coverage dir (`M3`) | **Read-Write (`$UID:$GID`)** | `.slipfork/runs/<run_id>/<slot>/{output,coverage}` | Surviving Playwright traces, Blastit reports, coverage shards |
| `/var/lib/postgresql/data`, `/data` (inside `crun`) | **Read-Write (`$UID:$GID`)** | `<state_dir>/{pgdata/<id>,redis/<id>,s3}` | `FICLONE`-clonable stateful backing-service storage |

---

## 4. Published World Image Contract & Supported Distro List

### 4.1 Supported App Stage Linux Distributions (Detected from `/etc/os-release`, Never Assumed)

`BaseImageBuilder` parses `<app_rootfs>/etc/os-release` (`ID` + `VERSION_CODENAME` / `VERSION_ID`) and verifies `<app_rootfs>/bin/sh` is an executable ELF binary (or symlink resolving inside `<app_rootfs>` to one). The supported `L0` app stage distribution set is:

| Distribution (`ID`) | Supported Releases (`VERSION_CODENAME` / `VERSION_ID`) | C Library Family | Typical Base Tags |
|---|---|---|---|
| `debian` | `bookworm` (`12`), `trixie` (`13`) | `glibc` | `debian:trixie-slim`, `python:3.14-slim-trixie`, `rust:1.85-slim-bookworm`, `node:22-bookworm-slim`, `php:8.4-cli` |
| `ubuntu` | `jammy` (`22.04`), `noble` (`24.04`) | `glibc` | `ubuntu:24.04`, `ubuntu:noble` |
| `alpine` | `3.20`, `3.21` | `musl` | `alpine:3.21`, `node:22-alpine3.21`, `php:8.4-fpm-alpine`, `golang:1.24-alpine3.21` |

`FROM scratch`, `gcr.io/distroless/*`, EOL releases (`bullseye`, `focal`, `alpine:3.18`), and non-Debian/Ubuntu/Alpine stages fail closed naming the supported table above and suggesting `world.stage = "builder"` when a multi-stage `Dockerfile` ends in a `distroless`/`scratch` packaging stage.

### 4.2 The 6-Rule World Image Contract

| # | Contract Rule | Static / Boot Verification | Failure Variant |
|---|---|---|---|
| **1** | **Named target `Dockerfile` stage** | Selected `Dockerfile` stage must carry `FROM <base> AS <name>` (inferred from final stage when named, or selected via `world.stage = "<name>"`). | `UnnamedTargetStage`, `UnknownTargetStage` |
| **2** | **Supported Linux distro & executable `/bin/sh`** | `<app_rootfs>/etc/os-release` must match `debian` (`bookworm`\|`trixie`), `ubuntu` (`jammy`\|`noble`), or `alpine` (`3.20`\|`3.21`), and `/bin/sh` must exist and be executable. | `UnsupportedDistro`, `MissingPosixShell` |
| **3** | **Unprivileged `$UID:$GID` & read-only `/` runtime** | Image `ENTRYPOINT` / `CMD` (when used because `run_command` is empty) must not invoke root-only privilege-drop wrappers (`gosu`, `su-exec`, `sudo`, `chpst`) and `/` mounts `EROFS` under `no_new_privileges`. | `RootAtRuntimeEntrypoint` |
| **4** | **Zero baked host paths or single-origin URLs (`ADR-0005`)** | Image `ENV` and `WORKDIR` must not bake `BASE_URL`, `*.localhost` URLs, `DATABASE_URL`, `REDIS_URL`, `AWS_S3_ENDPOINT_URL`, or host user paths (`/Users/*`, `/home/*`). | `BakedOriginOrHostPath` |
| **5** | **App deps in `Dockerfile`; backing services in `snow.yml`** | App stage must not install or launch `postgres`/`postgresql`, `redis-server`, `weed` (SeaweedFS), or `dockerd`; backing services belong in `snow.yml` `resources:`. | `EmbeddedBackingService` |
| **6** | **`EXPOSE` / `HEALTHCHECK` agrees with `snow.yml`** | Every TCP port in image `EXPOSE` / `HEALTHCHECK` must appear in `snow.yml` `components[].internal_ports` or `ingresses[].target.port` for components using that image. | `ExposeIngressMismatch` |

---

## 5. Failure-Mode Catalogue & Exact User-Facing Error Strings

Every contract violation halts **before** creating partial world directories and prints an actionable diagnostic naming the exact 1-line fix:

### 5.1 No Buildable Image in `snow.yml` (`BaseImageError::NoBuildableImage`)
```text
error: snow.yml declares no buildable Dockerfile under 'images:'; a project that cannot be built into an image is not a project Slipfork can fork
  fix: add a named Dockerfile stage and declare it in snow.yml:
    images:
      - id: app
        context: .
        dockerfile: Dockerfile
```

### 5.2 Multiple `BuildImage` Entries Without `world.image` (`BaseImageError::AmbiguousWorldImage`)
```text
error: snow.yml declares 2 buildable images ('api', 'worker') and slipfork.toml does not select one
  fix: add `world.image = "api"` under `[world]` in slipfork.toml
```

### 5.3 Unknown `world.image` Selector (`BaseImageError::UnknownWorldImage`)
```text
error: slipfork.toml sets `world.image = "backend"`, which matches no buildable image in snow.yml (available: 'api', 'worker')
  fix: set `world.image = "api"` in slipfork.toml
```

### 5.4 Rule 1 — Unnamed Final `Dockerfile` Stage (`BaseImageError::UnnamedTargetStage`)
```text
error: World Image Contract #1 violated in Dockerfile:24: final stage `FROM python:3.14-slim-trixie` has no stage name
  fix: change Dockerfile:24 to `FROM python:3.14-slim-trixie AS app` (or set `world.stage = "<stage>"` in slipfork.toml)
```

### 5.5 Rule 1 — Unknown `world.stage` (`BaseImageError::UnknownTargetStage`)
```text
error: World Image Contract #1 violated: `world.stage = "dev"` not found in Dockerfile (declared stages: 'builder', 'runtime')
  fix: set `world.stage = "builder"` in slipfork.toml
```

### 5.6 Rule 2 — Unsupported Base Distro or `distroless`/`scratch` (`BaseImageError::UnsupportedDistro`)
```text
error: World Image Contract #2 violated in stage 'runtime' (Dockerfile:18): detected distro 'fedora:41' (or missing /etc/os-release + /bin/sh); supported distros are debian (bookworm, trixie), ubuntu (jammy, noble), and alpine (3.20, 3.21)
  fix: set `world.stage = "builder"` in slipfork.toml or switch `FROM` to a supported Debian, Ubuntu, or Alpine base
```

### 5.7 Rule 3 — Root-at-Runtime Entrypoint (`BaseImageError::RootAtRuntimeEntrypoint`)
```text
error: World Image Contract #3 violated in stage 'api': ENTRYPOINT ["docker-entrypoint.sh", "gosu", "appuser"] requires root to switch UIDs, but Slipfork always executes as host $UID:$GID (1000:1000) with a read-only '/' filesystem
  fix: set an unprivileged command in snow.yml `modes.local.components.api.run_command` (or remove `gosu`/`su-exec`/`chown` from ENTRYPOINT)
```

### 5.8 Rule 4 — Baked Host Path or Origin/Service URL (`BaseImageError::BakedOriginOrHostPath`)
```text
error: World Image Contract #4 violated in stage 'api': baked image ENV `BASE_URL=http://localhost:8000` (or host path `/home/alice/app`) conflicts with Slipfork single-origin and ResourceCoordinates injection
  fix: delete `ENV BASE_URL=...` from Dockerfile and reference `${{ resources::app.url }}` under `modes.all.envs` in snow.yml
```

### 5.9 Rule 5 — Backing Service Installed Inside App `Dockerfile` (`BaseImageError::EmbeddedBackingService`)
```text
error: World Image Contract #5 violated in stage 'api': detected `/usr/lib/postgresql/18/bin/postgres` (backing service daemon) installed inside the app image; Slipfork runs postgres, redis, and seaweedfs as isolated crun containers from snow.yml
  fix: remove `postgresql` server installation from Dockerfile and declare `resources.pg_clusters` in snow.yml
```

### 5.10 Rule 6 — `EXPOSE` / `HEALTHCHECK` Disagrees with `snow.yml` (`BaseImageError::ExposeIngressMismatch`)
```text
error: World Image Contract #6 violated for image 'api': Dockerfile EXPOSE declares port 3000, but snow.yml component 'api' declares internal_ports [8000] (ingresses target 8000)
  fix: change `EXPOSE 3000` to `EXPOSE 8000` in Dockerfile (or update `components.api.internal_ports` / `ingresses` in snow.yml to 3000)
```

### 5.11 Layer 3 — `--host-caches` ELF/Libc ABI Mismatch (`BaseImageError::HostCacheAbiMismatch`)
```text
error: `--host-caches` ABI check failed: host is x86_64 glibc 2.41, but world stage 'api' is x86_64 musl 1.2 (alpine:3.21); mounting host toolchains would fail to execute
  fix: re-run without `--host-caches` (or use a matching x86_64 glibc >= 2.41 base such as `debian:trixie-slim`)
```

---

## 6. Content-Addressed Caching & Invalidation Model (Non-`tmpfs`)

All `L0` rootfs trees and backing-service OCI bundles live under `<state_dir>/cache/` (default `~/.cache/slipfork/<project>/cache/`, verified non-`tmpfs` and `FICLONE`-capable at `slipforkd` startup — **never** `/tmp`!):

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
        ├── app/                                # $UID:$GID-owned extracted slipfork-world rootfs (RO lowerdir)
        └── manifest.json                       # ImageContractMetadata (committed atomically via rename)
```

### 6.1 Exact `L0` Merkle Key Formula (`RootfsDigest`)

```text
l0_merkle_sha256 = SHA256(
    "slipfork-l0-rootfs:v1\0"
 || image_id              || "\0"
 || dockerfile_rel_path   || "\0" || SHA256(dockerfile_raw_bytes)
 || context_rel_path      || "\0" || context_merkle_sha256          # respects .dockerignore; excludes .git/ & .worktrees/
 || target_stage          || "\0"
 || sorted_map(image.envs, (k, SHA256(resolved_v)))                 # hashes secret build-arg values without storing plaintext
 || crun_binary_sha256    || "\0"
 || world_init_sha256     || "\0"
 || target_arch
)
```

### 6.2 Invalidation Rules & `ADR-0003` / `ADR-0008` Composition

1. **Source-only edit outside `Dockerfile` `COPY` inputs (or ignored by `.dockerignore`):** `l0_merkle_sha256` is **unchanged** (`0 ms` `L0` cache hit); live worktree bind-mount reflects the edit immediately inside running worlds.
2. **`Dockerfile`, lockfile (`uv.lock`, `Cargo.lock`, `pnpm-lock.yaml`), or `world.stage` edit:** `l0_merkle_sha256` changes -> `DockerfileStageBuilder` runs `docker buildx build` (reusing BuildKit layer cache) into `.staging-<uuid>`, validates the 6-rule contract, atomically renames to `rootfs/<new_sha256>`, and invalidates `DagPlanner`'s `base` snapshot (`root:base-v2:<l0_merkle_sha256>:<oci_service_digests>`) and downstream `[fixtures.*]` while leaving `[artifacts.front]` untouched (`ADR-0003`).
3. **`snow.yml` `pg_clusters[].version` change (e.g. `"17"` -> `"18"`):** Resolves/extracts `postgres:18` into `cache/oci/<sha256>/rootfs` once and invalidates `base` (`initdb` / `ADR-0008` `dump -> pg_restore -> CHECKPOINT -> freeze_world`) without rebuilding `app_rootfs`.
4. **Offline & Fork Guarantee:** Once `tags.json` + `cache/oci/<sha256>/rootfs` + `cache/rootfs/<l0_merkle_sha256>/app` exist on disk, `slipfork up`, `slipfork fork`, and `slipfork test` perform **zero network/Docker calls**; `slipfork prune` garbage-collects unreferenced `rootfs/*` and `oci/*` digests under `flock`.

---

## 7. Live Host Feasibility Probe (Measured on This Host, Zero Assumed Values)

Executed on `2026-11-15` on this repository's Linux host (`kernel 6.12.117+deb13-amd64`, `x86_64`, unprivileged LXC container, `/` on XFS `reflink=1`, `uid=1000` `remy:remy`, `docker 29.9.3`, `buildx v0.39.0`) inside `.slipfork/probe-crun-e2e` (cleaned up after measurement):

### 7.1 Static `crun` + `buildx --output type=local` Ownership Verification

```console
$ curl -fsSL https://github.com/containers/crun/releases/download/1.20/crun-1.20-linux-amd64-disable-systemd -o "$SCRATCH/crun" && chmod +x "$SCRATCH/crun"
$ file "$SCRATCH/crun" && sha256sum "$SCRATCH/crun"
crun: ELF 64-bit LSB executable, x86-64, version 1 (SYSV), statically linked, for GNU/Linux 3.10.0, stripped
ff8407185ec474d7c60f697032b7b1fc1fb028fbd0bf13c7d5da9a7d9a479e38  crun
$ docker buildx build --quiet -f Dockerfile.pg --output "type=local,dest=$SCRATCH/pg-rootfs" "$SCRATCH/build"
# Extracted postgres@sha256:e3d24f6906d0760bf4da0dae872415391ededefad23fd471c47dc53b0ddf3c42,
# redis@sha256:1f88291b4c57bdf249f02eb085a8669889b2d2954e91d4de5118a5da6b78dd53, and alpine:3.21 — all files owned remy:remy (1000:1000).
```

### 7.2 Two Kernel/Runtime Traps Discovered & Solved Live Before Signing Off

1. **Trap A — Omitting `user` + `pid` namespaces in `crun` `config.json` when `geteuid() == 1000`:**
   - Observed failure with `namespaces: [mount, ipc, uts]` only (`crun --debug --cgroup-manager=disabled`):
     ```text
     non root user need to have an 'user' namespace
     newgidmap: write to gid_map failed: Operation not permitted
     newuidmap: write to uid_map failed: Operation not permitted
     setresuid to `0`: Invalid argument
     ```
   - Adding `{"type": "user"}` with `uidMappings`/`gidMappings` `[{"containerID": 1000, "hostID": 1000, "size": 1}]` without `{"type": "pid"}` advanced past UID setup and failed at `mount 'proc' to 'proc': Operation not permitted` (`EPERM`).
   - **Verified fix:** Declare `namespaces: [user, pid, mount, ipc, uts]` (omitting `network` so every container shares the world's `127.0.0.1`) + identity `uidMappings`/`gidMappings` (`1000 -> 1000`) + `--cgroup-manager=disabled`.
2. **Trap B — Missing `$UID` entry in `postgres:18` `/etc/passwd`:**
   - Observed failure on `initdb` without `/etc/passwd` overlay:
     `initdb: could not look up effective user ID 1000: user does not exist` (`rc=1`).
   - **Verified fix:** Bind-mount a read-only synthesized `/etc/passwd` appending `remy:x:1000:1000:slipfork world user:/tmp:/bin/sh` while keeping `root.readonly = true`.

### 7.3 End-to-End Unprivileged World Probe (`postgres:18` + `redis:8` + Cross-Distro `alpine:3.21` + XFS `FICLONE` Clone)

```console
$ unshare --map-user=1000 --map-group=1000 --keep-caps --mount --net --pid --fork --mount-proc bash -euo pipefail -c '
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
$ cp --reflink=always -a "$SCRATCH/state" "$SCRATCH/state-clone"
# Booted World B in a fresh unshare netns over state-clone on 127.0.0.1:5432 & :6379:
clone_pg=from-crun-pg18
clone_redis=from-crun-redis8
```

Read-only `L0` app rootfs overlay + `pivot_root` was likewise verified live on this XFS host: `mount -t overlay overlay -o userxattr,lowerdir=<world_mp>:<app_rootfs> <merged>` refuses `/` writes with `EROFS`, preserves `remy:remy` RW writes on bind-mounted `<worktree>`, and `nsenter --target=<init_pid> --user --preserve-credentials --mount --net --pid` enters `Alpine Linux v3.21` directly after `cd <merged> && pivot_root . .old_root && umount -l /.old_root`.

---

## 8. Proposed Implementation Sequencing (`M2.*` / `M3.*` Nodes & Disjoint `paths`)

| Node | `depends_on` | Disjoint `paths` Globs | Scope (`<= 500` LOC per leaf) |
|---|---|---|---|
| **`M2.25`** *(created)* | `[M2.24]` | `["ARCHITECTURE.md", "docs/**", "README.md", "plan/ROADMAP.md", "support/integrations.toml"]` | Whole-repo doc/ADR/`ARCHITECTURE.md` consistency sweep against `ADR-0009` (`ServiceRuntime`, container-scoped `pg_restore`/`psql`/`redis-cli`, `IncusEngine` `WorldRootfs` provenance). |
| **`M2.26`** | `[M2.25]` | `["crates/slipfork-dag/src/**", "crates/slipfork-dag/tests/**"]` | Add `WorldImageConfig { image, stage }` (`[world]`, 2 optional keys, `deny_unknown_fields`) to `SlipforkConfig` and fold `l0_rootfs_digest` into `DagPlanner` `base` root input. |
| **`M2.27`** | `[M2.26]` | `["crates/slipfork-engine/src/rootfs/**", "crates/slipfork-engine/src/lib.rs", "crates/slipfork-engine/tests/critical_rootfs.rs", "tools/critical_guard.py"]` | Implement `#![forbid(unsafe_code)]` `WorldRootfs`, `trait BaseImageBuilder`, `DockerfileStageBuilder` (injected `BuildxExporter`, `OciImageStore`, `AbiProbe`), `FakeBaseImageBuilder`, pure `Dockerfile` stage parser, `/etc/os-release` + ELF ABI parser, 6-rule World Image Contract diagnostics (§5.1–§5.11), and shared conformance + adversarial critical tests. |
| **`M2.28`** | `[M2.27]` | `["crates/slipfork-engine/src/reflink_netns.rs", "crates/slipfork-engine/src/incus.rs", "crates/slipfork-engine/src/service.rs", "crates/slipfork-engine/src/world_init.rs", "crates/slipfork-engine/src/bin/slipfork-world-init.rs", "crates/slipfork-engine/src/conformance.rs", "crates/slipfork-engine/tests/critical_engine.rs", "poc/**"]` | Add `rootfs: WorldRootfs` to `WorldSpec` and unified `ServiceRuntime { WorldProcess, OciContainer }` + `exec_in_service` / `service_entry_command` (`EngineError::ServiceNotRunning`); wire `userxattr` RO overlay + `/etc/passwd` overlay + `pivot_root` + static `crun` supervision in `slipfork-world-init` / `ReflinkNetnsEngine`; replace `IncusEngine::base_instance: String` with `WorldRootfs` import (`slipfork-l0-<digest12>`); prove live cross-distro `postgres:18` + `redis:8` + `alpine:3.21` + `FICLONE` clone in `critical_engine.rs`. |
| **`M2.29`** *(or fold into `M2.16`/`M2.17`)* | `[M2.16, M2.17, M2.28]` | `["crates/slipfork/**"]` | Wire `DockerfileStageBuilder` + `--host-caches` in `slipforkd`, route `initdb`, `ADR-0008` `pg_restore`/`CHECKPOINT`, `slipfork db` (`psql`), and `slipfork redis` (`redis-cli`) via `exec_in_service` / `service_entry_command` into the running `crun` container, and cover contract failures + `prune` cache GC in `critical_cli.rs`. |
| **Deltas to existing nodes** | — | Existing `M2.19`, `M2.21`, `M3.*` | **`M2.19`** (`depends_on += M2.29`): promote `[world]` (`world.image`, `world.stage`), `--host-caches`, and the 6-rule World Image Contract into published `docs/reference/manifest.md`. **`M2.21`**: live `IncusEngine` conformance consumes `WorldRootfs` automatically. **`M3.1`** (`crates/slipfork-test/**`): `TestCoordinator` `TestSlot` pool reuses the daemon-materialized `WorldRootfs` (`0 ms` rootfs copy per slot) and honours `--host-caches` on `slipfork test`. |
