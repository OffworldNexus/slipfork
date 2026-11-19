# ADR-0003: Orthogonal Build Artifact Volumes + Stateful Fixture Snapshot DAG

- **Status:** Accepted — amended for M2/M3 (`per_world` schema & planner shipped in `0.1.0`, `"per-world"` vs `"shared"` hash discriminator, `DigestCache` read/write non-sharing rules, `ADR-0008` `base` seed-dump boundary, `ADR-0009` `Dockerfile` `L0` rootfs + `crun` service containers, and `ADR-0010` §2 unified per-world Btrfs subvolumes)
- **Date:** 2026-11-15

## Context

Two naive topologies fail at scale for full-stack integration/browser test suites:
1. **One environment per component per test:** A 5-component `Forkfile` running across 16 parallel test workers spawns **80 environments** concurrently, adds virtual-network complexity, and loses atomic snapshot consistency across API + worker + DB + S3.
2. **Single linear snapshot chain mixing frontend builds and DB seeds:** Baking `pnpm build` inside the same snapshot lineage as `manage.py migrate -> seed` means changing 1 line of CSS invalidates every seeded PostgreSQL snapshot (and adding 1 DB migration triggers a frontend rebuild).

## Decision

1. **Split Stateless Build Artifacts from Origin-Classified Fixture Lineages:**
   - **`ArtifactStore` (`[artifacts.<id>]`):** Caches stateless build outputs (e.g., `front/build`, `staticfiles`) by SHA-256 Merkle digest of `(build_cmd, build_envs, input_files)`.
   - **`FixtureDag` (`[fixtures.<id>]`):** Forms a tree of stateful steps (`base -> migrate -> seed_*` — where in `local` dev `base` may materialize a cached `OSPP v1` PostgreSQL seed dump before `pre-deploy` migrations while `slipfork test` defaults hermetically to clean `initdb`, `ADR-0008` §2–§3) in which each node is explicitly either:
     - **Origin-independent (`per_world = false`, default):** Reads only origin-free service coordinates (`DATABASE_URL`, `REDIS_URL`, `AWS_S3_ENDPOINT_URL`), hashes by `("slipfork-fixture:v1", id, parent_digest, run_cmd, "shared", origin_free_runtime_envs, input_files)`, and seals into a **reusable CoW snapshot** shared across forks and test slots. Origin-bearing values (`resources::app.*`, `domains::*`, `BASE_URL`) are withheld during reusable snapshot execution (`ADR-0005` §1).
     - **Per-world (`per_world = true`, schema, `"per-world"` hash discriminator & planner shipped in `0.1.0`; per-world runtime execution in M2/M3):** Writes absolute origin URLs into persistent state (e.g., `django.contrib.sites` or Wagtail `Site.root_url` rows). A `per_world = true` node hashes by `("slipfork-fixture:v1", id, parent_digest, run_cmd, "per-world", runtime_envs, input_files)` (so flipping `per_world` always changes the digest), is **never queried from or written into the world-independent `DigestCache`** (`DagPlanner::plan` always emits `CacheAction::Rebuild` without consulting `DigestCache::has_fixture`, and M2/M3 snapshot materializers and `crates/slipfork-test` MUST NEVER insert a `per_world = true` digest into `DigestCache`), cannot be the `parent` of an origin-independent node (`DagError::OriginIndependentChildOfPerWorld`), and executes inside each materialized world **after** its single `WorldOrigin` is assigned (`ADR-0005` §1).
2. **One Unified Pod World per Cloned Test / Worktree:**
   - Each world clones **one** writable Btrfs subvolume from its deepest origin-independent `FixtureNode` read-only snapshot (`ADR-0010`), assigns its single `WorldOrigin`, runs any downstream `per_world = true` fixture steps, and bind-mounts all required `ArtifactVolume`s read-only alongside the shared-UID repo mount.
3. **Persistent Setup Snapshots (`STP v1`, `ADR-0006` §3):**
   - Over `STP v1` (`SLIPFORK_PROTOCOL_FD`), testing frameworks expose persistent setup/fixture stage nodes that freeze into read-only Btrfs subvolume snapshots (`ADR-0010` §3), eliminating repeated setup overhead.

## Consequences

- CSS/TS edits rebuild only `artifact:front` (`0 ms` DB re-seed); migration/seed edits rebuild only downstream `FixtureNode`s (`0 ms` frontend rebuild); origin-independent snapshots share safely across worlds while `per_world = true` fixtures prevent stale baked-in URLs.
