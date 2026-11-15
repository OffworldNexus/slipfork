---
id: M2.25
title: "Whole-repo consistency sweep after L0 rootfs + OCI-container pivot (single writer: ARCHITECTURE.md + ADRs)"
kind: task
status: active
owner: designer
priority: 1
depends_on: [M2.24]
paths:
  - ARCHITECTURE.md
  - "docs/decisions/**"
  - "docs/design/**"
  - "docs/reference/**"
  - docs/index.md
  - "docs/getting-started.md"
  - "docs/snow-modes.md"
  - "docs/testing-dag.md"
  - "docs/agents-and-opencode.md"
  - docs/knowledge/LESSONS.md
  - README.md
  - plan/ROADMAP.md
  - support/integrations.toml
attempts: 2
max_attempts: 3
escalation: none
created: "2026-11-15T20:17"
updated: "2026-11-15T20:45"
---

# Goal

Whole-repo consistency sweep after the two `L0` rulings in `M2.24` (owner: *"review the whole documentation/ADR/whatever because I'm sure it's wildly inconsistent now"*). The design of record (ADR-0002/0003/0005/0006/0008, `ARCHITECTURE.md`, `target-dx.md`) and all 22 `M2.*` node specs were written **before** the pivot to (a) a Dockerfile-derived `L0` rootfs and (b) backing services running as `crun` OCI containers. Sweep every stale claim.

**Source of truth for the sweep: `docs/decisions/ADR-0009-world-base-image-and-image-contract.md`** (must be `Accepted`/final before this node starts).

## Known contradictions to fix (verify each; add any others you find)

1. **`postgres`/`redis` are no longer part of the world rootfs.** Any text implying `exec_in_world` can run `psql`, `pg_restore`, `pg_ctl`, `redis-cli` from the app rootfs is now wrong:
   - `ADR-0008` dump restore step (`pg_restore --no-owner --no-privileges --clean --if-exists` + `CHECKPOINT;`) must state that these execute **inside the `postgres:<version>` container** (e.g. `crun exec` into the running service container), not in the world rootfs — and what happens if the container is not up yet (fail closed).
   - `ADR-0002` "postgres runs natively as `$UID` on private port 5432" → now "postgres runs as a pinned OCI container inside the world's netns, state bind-mounted from the world CoW state, uid mapped to the host user".
   - `docs/knowledge/LESSONS.md` / `docs/design/m1-core-and-poc.md`: label the M1 benchmark as the **host-shared-`/usr` topology**, historically accurate, not the current design.
2. **CLI services must target the container, not the rootfs:**
   - `slipfork db` → `psql` **inside the postgres service container**; `slipfork redis` → `redis-cli` inside the redis container. Update `target-dx.md`, `ADR-0008` §rescue UX, and the `M2.17` node spec.
   - `slipfork forward` is unaffected (host listener → `dial_world_tcp` → in-world port). Confirm no wording contradicts that.
3. **`spawn_service` / `stop_service` vs containers (`M2.7`, already committed).** Decide and state one answer: does the engine supervise containers via the same `spawn_service` surface (argv replaced by an OCI bundle spec) or does `crun` get its own adapter behind the same port? No parallel supervision paths; update `ARCHITECTURE.md` §3.2/§3.3 + the `M2.7` RESULT/FOLLOWUPS wording.
4. **`IncusEngine::base_instance` provenance** must be described as produced by the same `WorldRootfs` path (no hand-built instance, no second mechanism).
5. **`freeze_world` ordering** must say: stop/suspend **container** services first (crun has no stateful RAM semantics here), then reflink state dirs. Confirm `ADR-0003`'s snapshot/invalidations still hold with container state dirs as the mutable surface.
6. **`ResourceStrategy` (`ADR-0008`)** — `SeedDumpStore` restore path now needs the container; also confirm shared-vs-hermetic S3 still makes sense when S3 is a container (bind-mounted state dir). Fix any contradiction.
7. **`ROADMAP.md` M2 row + `M2/NODE.md`** must name the `L0` rootfs + OCI container runtime as exit criteria; `docs/VISION.md` may need one line.
8. **Purge dead alternatives**: anywhere `CuratedBaseProvider`, `slipfork_base`, prebuilt-image mode, apt-installed backing services, or host-`/usr` sharing is still described as available (a rejected-alternatives paragraph in `ADR-0009` is the only allowed mention).
9. **`support/integrations.toml`** — no entry may claim a tier that the new topology contradicts.
10. **Published docs** (`docs/index.md`, `getting-started.md`, `snow-modes.md`, `testing-dag.md`, `agents-and-opencode.md`, `reference/*`, `README.md`) must stay **truthful to `0.1.0`** (nothing published about `world.image`, containers, `crun` until the code ships) — `check_docs_honesty.py` + `zensical --strict` must stay green.

## Rules

- **Single writer**: this node owns `ARCHITECTURE.md` and `docs/decisions/**` for its duration (`lead` must not run another node touching them concurrently).
- Every fix must be a *deletion or correction of a false statement*, not an added hedge ("in some cases", "may also"). No duplicated normative text: `ADR-0009` is the single home for the rootfs/container contract; other docs reference it by section.
- Do not touch `crates/**` in this node; implementation deltas are proposed as node-spec edits (`plan.py edit`) or filed as follow-ups.

# Acceptance

- [ ] `rg -ni "apt(-get)? install .*(postgres|redis|seaweedfs)|curated base|slipfork_base|CuratedBaseProvider|prebuilt image|shares? the host .?(/usr|rootfs)" ARCHITECTURE.md docs README.md plan/ROADMAP.md` exits 1 (0 hits outside `ADR-0009`'s rejected-alternatives line).
- [ ] `rg -n "pg_restore|pg_ctl|psql" docs/decisions/ADR-0008*.md` shows every occurrence qualified by the postgres container (manual review note in `RESULT.md` quoting the 3 lines).
- [ ] `rg -n "world\\.image|world\\.stage|crun" docs/index.md docs/reference README.md` exits 1 (published docs stay vaporware-free).
- [ ] `validate_accepted_adrs.py`-style check is not required, but every ADR touched keeps a truthful `Status:` line (`Accepted` only if the decision is final).
- [ ] `make check` exits 0 (`plan.py validate`, `guard.py`, `ruff`, `pytest`, `bun`, `cargo fmt/clippy/doc/test`, `zensical --strict`, `check_docs_honesty.py`).
