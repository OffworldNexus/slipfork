# RESULT — `M2.25` Whole-repo consistency sweep after the `L0` rootfs + `crun` OCI pivot

- **Owner:** designer (single writer of `ARCHITECTURE.md` + `docs/decisions/**` for this node).
- **Source of truth:** `docs/decisions/ADR-0009-world-base-image-and-image-contract.md` (Accepted, `M2.24`) + `docs/design/world-base-image.md`. Every fix below is a **deletion or correction of a false statement**; `ADR-0009` remains the single home for the rootfs/container contract and other files reference it by section. Citation shorthand used throughout: `ADR-0009` §1/§2/§3 = its `Decision` rules 1/2/3.
- **Verification:** `make check` -> **`0`**; `uv run --no-project tools/guard.py` -> `guard: OK`; `uv run --no-project tools/plan.py validate` -> `plan OK`.

## 1. Disposition of the 10 known contradictions

| # | Contradiction | Disposition |
|---|---|---|
| 1 | `postgres`/`redis` no longer in the world rootfs | `ADR-0008` §2 (`dump.begin..dump.end`) now mounts the dump **and** restores via `WorldEngine::exec_in_service` **inside** the running `postgres:<v>` container, with `CHECKPOINT;` in that same container and `EngineError::ServiceNotRunning` fail-closed when it is not `Running` — never retried in the world rootfs. `ADR-0002` §3's "`postgres` runs natively as `$UID` on private port `5432`" is deleted and replaced by `crun` containers on `:5432`/`:6379`/`:9000` over `<world_dir>/state/{pgdata,redis,s3}`. M1 benchmark labelled **host-shared-`/usr` topology** in `docs/design/m1-core-and-poc.md` (Status + `R11` + `R14`) and in one new `docs/knowledge/LESSONS.md` line. `ADR-0006` Context #3 carries the same label. |
| 2 | CLI services must target the container | `docs/design/target-dx.md` §1.4 example comments + a new §1.4 bullet + §1.6 `db`/`redis` rows now say `crun exec` into the service container (`psql`/`redis-cli` from the container rootfs, `ServiceNotRunning` fail-closed). `ADR-0008` §4 (interactive TTY entry) names `service_entry_command`. `forward` confirmed unaffected (host `127.0.0.1:0` -> `dial_world_tcp`); both files now state that explicitly. |
| 3 | `spawn_service` vs a `crun` adapter | **Decided: same `spawn_service` surface.** `ServiceSpec` gains `runtime: ServiceRuntime { WorldProcess, OciContainer }`, and `crates/slipfork-engine` owns the single dispatch; there is **no** `ContainerRuntime` trait and no second supervisor (`ARCHITECTURE.md` §3.3 new bullet; `ADR-0009` §3 bullet kept as the single normative answer). `M2.7` node body hand-off written to the splice folder. |
| 4 | `IncusEngine::base_instance` provenance | `ADR-0002` §2, `ARCHITECTURE.md` §3.3 (new bullet) + §4 `H-004`, and `docs/decisions/ADR-0009` all say one thing: `IncusEngine` imports `WorldSpec.rootfs.app_rootfs` once per digest as `slipfork-l0-<digest12>` and clones it; `base_instance: String` is deleted, no hand-built instance exists. |
| 5 | `freeze_world` ordering | `ARCHITECTURE.md` §3.3 trait doc + PID-1 bullet, `ADR-0002`, `ADR-0006` §4 Condition 5, and `ADR-0003` Status now state: stop `WorldProcess` services, then `OciContainer` services (`SIGTERM` -> bounded grace), then reflink/Incus-snapshot `state_dir`; container state contributes no RAM-resident state, so `SnapshotMode::StatefulRam` never applies to a backing service. `ADR-0003`'s snapshot/invalidation rules still hold with the container-written state set as the sealed surface. |
| 6 | `ResourceStrategy` / `SeedDumpStore` / S3 as a container | `ARCHITECTURE.md` §3.2 `ResourceStrategy::Local` names the `seaweedfs` `crun` container and `ResourcePlan::resolve` documents dump-as-`readonly_mounts`; `ADR-0008` §3 names the container; shared-vs-hermetic still resolves cleanly (`Local` = container over CoW state, `PreferRemote`/`Remote` = OSPP coordinates), so no fallback was added. |
| 7 | `ROADMAP.md` M2 row + `M2/NODE.md` exit criteria | `plan/ROADMAP.md` M2 row rewritten: `ADR-0009` added to the architecture item and a **new explicit exit gate** (`L0` rootfs + `crun` OCI runtime, read-only `$UID:$GID` root, container-scoped `initdb`/`pg_restore`/`CHECKPOINT`/`psql`/`redis-cli`, Incus provenance). `plan/nodes/M2/NODE.md` Goal items 1/3 + `# Acceptance` gained the same gate. Frontmatter untouched, so no `plan.py edit` was needed (`plan.py validate` = `OK`). |
| 8 | Purge dead alternatives | Acceptance grep now exits `1` (zero hits). Two wording-only fixes were needed: `docs/reference/manifest.md` "prebuilt image" -> "registry image" (matches the shipped `Image::DockerHub` variant, so the page stays truthful to `0.1.0`), and the same phrase in the historical critic record `docs/design/m2-manifest-docs.review-dx.md`. `CuratedBaseProvider`/`slipfork_base`/apt-installed services appear only in `ADR-0009`'s Rejected Alternatives and in plan history logs. |
| 9 | `support/integrations.toml` tiers | **Verified — no edit required.** The three `declared` entries (`reflink-netns-world-engine`, `file-vault-secrets`, `dag-config-cli`) point at shipped `0.1.0` primitives, recipe pages that remain truthful to `0.1.0`, and existing `critical_*` symbols; none claims container/rootfs behaviour the topology contradicts. (`tools/check_support.py` ships in `M2.15`; today the file is validated by `crates/slipfork-dag/tests/critical_support.rs` inside `make check`, which passed.) `M2.19` must re-point `reflink-netns-world-engine`'s `recipe_page` once the image-backed world is published (recorded in that splice body). |
| 10 | Published docs truthful to `0.1.0` | `rg -n "world\.image|world\.stage|crun" docs/index.md docs/reference README.md` exits `1`; `make docs-check` (`zensical --strict`) and `check_docs_honesty.py` (8 published docs) exit `0`. The only published change is the `manifest.md` wording fix in #8. |

## 2. New contradictions found (beyond the 10)

11. **`docs/design/m1-core-and-poc.md:43`** quoted `3.12x` / `123 ms`/test; the measured report (`docs/research/m1-poc-benchmark.md:32`, `README.md:21`, `docs/index.md:36`) says `3.21x` / `118 ms`. Corrected, plus the topology label.
12. **`docs/decisions/ADR-0006-...md:12`** claimed `429 ms` cold vs `118 ms` warm; the measured report's cold mode is `352 ms` (`1.89 s`/`16` warm vs `5.63 s`/`16` cold). Corrected to `352 ms`; the M1 topology is named in the same line.
13. **`ARCHITECTURE.md:3`** cited `ADR-0001`..`ADR-0008`; stale after `ADR-0009` was accepted. Corrected (also `docs/design/m1-core-and-poc.md` ADR list).
14. **`ADR-0009` + `ADR-0008` could not compose: the seed dump had no path into the service container.** `ServiceRuntime::OciContainer` carried only `state_mounts` (RW state under `<world_dir>/state`), `exec_in_service` carries no stdin, and `pg_restore` must read the verified `<sha256>.pgdump` from inside the `postgres:<v>` rootfs. Fixed by adding `readonly_mounts: Vec<WorldMount>` to `ServiceRuntime::OciContainer` in `ADR-0009` (single home) and mirroring the field in `ARCHITECTURE.md` §3.3; `M2.28`'s node body splice carries the field + an overlap fail-closed assertion. **Owner may want to confirm this additive field** — it does not add a fallback, a second mechanism, or new config surface.
15. **`docs/VISION.md`** (outside this node's `paths`) still describes M2 without the `L0` rootfs / `crun` exit criteria (its M2 row lists single-origin edge, OSPP, VCS, CLI, support hygiene only). Not editable here — **lead action:** add one clause to `docs/VISION.md` §"Milestone map" M2 row: "`Dockerfile`-derived `L0` rootfs + daemonless `crun` backing-service OCI containers (`ADR-0009`)".
16. **`ExecTargetRequest` has no service selector, and no node owns the file that needs it.** `slipfork db`/`redis` must ask the daemon for a *service* entry, i.e. `ExecTargetRequest { cwd, world, service: Option<String>, argv }` in `crates/slipfork-edge/src/daemon.rs`. `M2.17`'s paths are `crates/slipfork/**` and `M2.29`'s are also `crates/slipfork/**` — neither may edit `crates/slipfork-edge/**`. **Lead action (one line):**
    ```bash
    uv run --no-project tools/plan.py edit M2.29 --paths '["crates/slipfork/**", "crates/slipfork-edge/src/daemon.rs"]'
    ```
    (plus `--depends` unchanged). Recorded in the `M2.17` splice body as a hard prerequisite.
17. **`M2.28`'s node body cited "`ADR-0009` §`Decision` rules 4-7"** while `ADR-0009`'s `Decision` has exactly 3 rules. The splice body now cites rules 2–3.

## 3. `# Acceptance` evidence

1. Dead-alternatives grep -> **exit `1`, zero hits**:
   ```bash
   rg -ni "apt(-get)? install .*(postgres|redis|seaweedfs)|curated base|slipfork_base|CuratedBaseProvider|prebuilt image|shares? the host .?(/usr|rootfs)" ARCHITECTURE.md docs README.md plan/ROADMAP.md
   ```
2. `pg_restore|pg_ctl|psql` in `ADR-0008` -> 3 occurrences on 2 lines, **all container-qualified**:
   - line 3 (Status): "…amended by `ADR-0009` — seed restore and `psql`/`redis-cli` execute inside the `crun` service container, never the `L0` app rootfs".
   - line 39: "Verified `<sha256>.pgdump` files mount `MountMode::ReadOnly` into the `base` world via `WorldSpec.mounts` **and** into the running `postgres:<v>` container via that service's `ServiceRuntime::OciContainer` `readonly_mounts` (`ADR-0009` §2), and restore via `WorldEngine::exec_in_service` **inside that container** (`pg_restore --no-owner --no-privileges --clean --if-exists` …)".
3. Published-docs vaporware grep -> **exit `1`, zero hits**:
   ```bash
   rg -n "world\.image|world\.stage|crun" docs/index.md docs/reference README.md
   ```
4. Every touched ADR keeps a truthful `Status:` line — `ADR-0002`, `ADR-0003`, `ADR-0006`, `ADR-0008` stay `Accepted` with an explicit `amended by ADR-0009` clause; `ADR-0009` untouched in status (owner-ratified).
5. `make check` -> **`0`** (plan validate, `guard.py`, `ruff`, `pytest`, `bun`, `cargo fmt/clippy/doc/test`, `zensical --strict`, `check_docs_honesty.py`).

## 4. Splice-ready node-spec deltas (`plan/nodes/M2/M2.25/NODE-BODIES-FIX/`)

Each file is the **body only** (everything after a node's frontmatter), with a leading `<!-- splice: … -->` note stating what to replace and any frontmatter command.

| File | Node | Delta |
|---|---|---|
| `M2.7.md` | `M2.7` | Supervision wording: current `argv`/`cwd` payload, `M2.28` reshapes it to `ServiceRuntime` behind the same three methods; no second supervisor; `freeze_world` ordering hand-off; `dial_world_tcp` serves container ports too. `# Acceptance` byte-identical (verified by `diff`). |
| `M2.16.md` | `M2.16` | Composition root starts only `ServiceRuntime::WorldProcess`; `BaseImageBuilder` injection + `--host-caches` + container `db`/`redis` belong to `M2.29` — no buildx/`crun` calls here. `# Acceptance` byte-identical (verified by `diff`). |
| `M2.17.md` | `M2.17` | `db`/`redis` enter the running service container via `service_entry_command` (`crun exec`), TTY + `ServiceNotRunning` fail-closed; `exec` sends `service: None`; `forward` unchanged; states the `ExecTargetRequest.service` prerequisite (see #16). |
| `M2.19.md` | `M2.19` | Promote `[world]`, `--host-caches`, 6-rule contract **by reference**, container-scoped `db`/`redis`; support-matrix promotion constrained by the new topology; frontmatter `--depends M2.15,M2.17,M2.18,M2.29`. |
| `M2.21.md` | `M2.21` | Live Incus conformance over the real `WorldRootfs` (`slipfork-l0-<digest12>`), no `base_instance`, `WorldProcess`-then-`OciContainer` freeze ordering; frontmatter `--depends M2.20,M2.28`; stays terminal. |
| `M2.28.md` | `M2.28` (extra) | Adds `readonly_mounts` to `ServiceRuntime::OciContainer` + overlap fail-closed assertion; corrects the `Decision` rule citation (#17). |

**Lead actions:** apply #15 (one `docs/VISION.md` clause) and #16 (one `plan.py edit` for `M2.29` paths), then hand the splice files to `planner`/`lead` for application (frontmatter commands are stated in each file's header comment).
