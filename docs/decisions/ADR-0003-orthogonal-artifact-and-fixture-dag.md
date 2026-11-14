# ADR-0003: Orthogonal Build Artifact Volumes + Stateful Fixture Snapshot DAG + Runner Zygote

- **Status:** Proposed
- **Date:** 2026-11-14

## Context

Two naive topologies fail at scale for full-stack integration/browser test suites:
- **Naive Topology A (One VM per component per test):** A 5-component `snow.yml` running across 16 parallel test workers spawns **80 VMs** concurrently, adds virtual-network complexity, and loses atomic snapshot consistency across API + worker + DB + S3.
- **Naive Topology B (Single linear snapshot chain mixing frontend builds and DB seeds):** Baking `pnpm build` inside the same snapshot lineage as `manage.py migrate -> seed` means changing 1 line of CSS invalidates every seeded PostgreSQL snapshot (and adding 1 DB migration triggers a frontend rebuild).

## Decision

1. **Split Stateless Build Artifacts from Stateful Fixture Snapshots:**
   - **`ArtifactStore` (`[artifacts.<id>]`):** Caches stateless build outputs (e.g., `front/build`, `staticfiles`) by SHA-256 Merkle digest of `(build_cmd, build_envs, input_files)`.
   - **`FixtureDag` (`[fixtures.<id>]`):** Forms a tree of stateful CoW snapshots (`base -> migrate -> seed_*`), hashed independently by `(parent_digest, run_cmd, runtime_envs, input_files)`.
2. **One Unified Pod World per Cloned Test / Worktree:**
   - Each test world clones **one** CoW instance from its target `FixtureNode` snapshot and bind-mounts all required `ArtifactVolume`s read-only alongside the shared-UID repo mount.
3. **Warm Runner Zygote (`fork()` RAM CoW):**
   - Language test adapters (`PytestAdapter`, `PlaywrightVitestAdapter`, `PhpUnitAdapter`) support a pre-imported runner zygote that forks per test via kernel `fork()` (`< 1 ms` page-table RAM CoW), eliminating repeated framework import overhead even when disk-only CoW snapshots are used.

## Consequences

- CSS/TS edits rebuild only `artifact:front` (`0 ms` DB re-seed); migration/seed edits rebuild only downstream `FixtureNode`s (`0 ms` frontend rebuild); `N` parallel workers launch `N` lightweight CoW worlds on identical canonical ports with zero teardown cost.
