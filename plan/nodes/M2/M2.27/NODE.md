---
id: M2.27
title: "slipfork-engine::rootfs: WorldRootfs + BaseImageBuilder + DockerfileStageBuilder (6-rule contract)"
kind: task
status: done
owner: implementer
priority: 1
depends_on: [M2.26]
paths: [Cargo.toml, Cargo.lock, "crates/slipfork-engine/**"]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-15T20:40"
updated: "2026-11-15T22:04"
---

# Goal

Implement `ADR-0009` §`Composition Root & DI Tree` + §`Decision` rules 1-3: the engine-agnostic `WorldRootfs` value and the single production `BaseImageBuilder` adapter.

- `WorldRootfs { digest: RootfsDigest, app_rootfs: PathBuf, crun_bin: PathBuf, service_images: BTreeMap<BackingServiceKey, OciRootfs>, metadata: ImageContractMetadata, host_caches: Vec<WorldMount> }` exactly as specified.
- `trait BaseImageBuilder { fn materialize(&self, req: &WorldRootfsRequest, on_event: &mut dyn FnMut(RootfsBuildEvent)) -> Result<WorldRootfs, BaseImageError>; }` with injected collaborators `BuildxExporter`, `OciImageStore`, `AbiProbe`, `InputHasher` (zero `std::process`/`std::env` inside the builder itself — adapters injected).
- `DockerfileStageBuilder`: append `FROM <stage> AS slipfork-world` to the project's `snow.yml` `images:` Dockerfile (never rewriting existing lines), build only that target with `buildx --target slipfork-world --output type=local,dest=<cache>/rootfs/<digest>/app`, cache key = SHA-256 Merkle over (Dockerfile bytes + context hash + capability spec + distro), store under `<state_root>/cache/rootfs/<hex>` (never `/tmp`).
- `FakeBaseImageBuilder` for conformance/unit tests; shared conformance suite asserts both produce a `WorldRootfs` satisfying the same invariants.
- 6-rule World Image Contract diagnostics with the **exact** error strings from `ADR-0009` (unnamed stage, unsupported distro, root-at-runtime requirement, baked host path/hostname/port, EXPOSE/ingress mismatch, no buildable image). Distro detection from the extracted rootfs `/etc/os-release` only.

# Context

- `docker buildx` and a static `crun` binary are available on this host (probe in `docs/design/world-base-image.md`): do not invent a fallback when either is missing — fail closed naming the missing tool.
- This node is the **sole owner** of root `Cargo.toml`/`Cargo.lock` in this chain.

# Acceptance

- [ ] `cargo test -p slipfork-engine --test critical_rootfs_contract` exits 0: one test per contract rule asserting the typed error + the exact user-facing message; `FakeBaseImageBuilder` passes the shared conformance suite.
- [ ] `cargo test -p slipfork-engine --test critical_rootfs_live -- --ignored` (real `buildx` build of a 3-line fixture Dockerfile with a named stage) exits 0 on this host; the test SKIPS with an explicit "docker/buildx absent" message only when the binary is missing, never passing silently.
- [ ] `rg -n "slipfork_base|CuratedBase|prebuilt|base_instance" crates/slipfork-engine` exits 1.
- [ ] `make check` exits 0.
