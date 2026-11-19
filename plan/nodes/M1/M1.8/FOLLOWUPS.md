# Follow-ups M1.8 (Gate Freeze: shipped with 0 BLOCKERs)

Merged from the two Quad Review Gate rounds (`reviewer`, `review-legality`, `review-ioc`, `review-cruft`).
`M1.9` (created) is the required pre-`0.1.0`-merge code sweep; everything below it is doc-strength work for the owning milestone.

## BLOCKER (resolved before commit)

- **Shared-world ingress had no owner** (`review-ioc` `SPEC_BLIND_SPOT`, round 1) — resolved by owner ruling `H-008` (Option A), now normative in `ADR-0005` §1 + `ARCHITECTURE.md` §3.3.
- **Rejected `[test]` post-suite field still parsed by shipping code** (`review-cruft`, round 1) — documents now state the dead field factually with no compatibility promise; deletion is `M1.9`.

## MAJOR

1. **`M1.9` (created):** delete the dead `TestSpec` string field in `crates/slipfork-dag/src/lib.rs` + its `critical_dag.rs` / `critical_slug.rs` assertions; land typed `TestSpec { coverage: Option<CoverageConfig> }` (`CoverageThreshold` `0..=100`), `FixtureSpec.per_world: bool`, and the initial `support/integrations.toml`. Required before `0.1.0` merges.
2. **`ADR-0007` evidence is partly self-attesting:** `exercised` accepts a `ci_job` key present in a workflow file (a job that never runs still earns the tier) and `field-tested`/`hardened` accept path existence of free-form markdown. Require typed in-record fields verified by `make check` (workflow-run id + commit + conclusion; product, suite size, date, contact) or relabel those tiers as attested + reviewed instead of "Machine-verified".
3. **`make support-sync` demotion is silent:** it lowers tiers, rewrites the table and exits 0. Print every demotion (`id`, `from -> to`, missing pointer) and exit non-zero whenever anything was demoted so a schedule cannot quietly shrink support claims.
4. **`ADR-0007` milestone is ambiguous:** "registry enters before `0.1.0` merge **or** in M2" contradicts `RESULT.md`/`M1.9` and `plan/ROADMAP.md`/`docs/VISION.md` (M2). Pin one milestone; note that `include_str!("../../../support/integrations.toml")` cannot compile until `support/` exists.
5. **`ADR-0001` §1 over-cites `H-001`:** the written `H-001` answer approves only optional `modes.local.components.<id>.run_command` overrides, while the ADR claims owner approval for component add/remove/disable. File the release-gate directive as a `HUMAN.md` `direction-challenge` entry and cite it, or narrow the ADR to the approved override.
6. **`docs/VISION.md`:25 claims mode-scoped component add/remove shipped "in `0.1.0`".** `0.1.0` only parses the `run_command` override; add/remove + supervision are M2.

## MINOR

- `ARCHITECTURE.md` `WorldSpec` carries no typed `origin`; "at most one origin" is only assertable in rendered env strings. Carry `origin: WorldOrigin` + one injected env assembler (`ResourceCoordinates` + optional `CoverageDriver::world_env`) shared by `up` and the pre-warm pool.
- `WorldOrigin::{Local,Shared}` expose public `url: String` fields, so the "sole constructor" rule is unenforceable. Make fields private: `WorldOrigin::local(&LocalHostRoute)` / `WorldOrigin::shared(&dyn TunnelLease)`.
- `share_world -> SharedWorldInfo` but `unshare_world -> WorldOrigin` forces the CLI to build an origin away from the lease owner. Make `share_world -> WorldOrigin` and derive lease metadata inside `WorldRegistry`.
- `WorldRegistry` owning `(WorldOrigin, Option<TunnelLease>)` permits the invalid `Shared` + no-lease state. Model as `enum WorldEntry { Local { .. }, Shared { origin, lease: Box<dyn TunnelLease> } }`.
- Clarify the "planned static port": same :80 Host-dispatch listener or a second listener, and its single config source at the Composition Root.
- Two "sole constructor" names (`LocalHostRoute::url()`, `ResourceCoordinates::local_for_world`): state one renderer + one caller.
- State that a `Local` world's `snow_domain` hostnames are trusted local ingresses (only `origin.url()` feeds CSRF/cookies), so "one URL per world" is not read as "one reachable hostname".
- Give `SupportRegistry`/`IntegrationEntry` an owning crate row; make `parse_toml` the sole parser for CLI, `make check`, and docs generation; name the compliance tool a sanctioned non-DI entrypoint with an injected repo probe.
- Specify `evidence` representation (flat optional-field struct vs per-tier sum type) and how `make check` validates `tier` against `evidence`.
- `plan/nodes/M1/M1.8/NODE.md`:76 (owner text) says "test-scoped hooks (`[test]`)" while `ADR-0006` states no hook surface exists; and the `[test]` table holds typed fields only.
- `docs/design/target-dx.md` CLI table: pin `slipfork support [--json]` to M2.
- `plan/ROADMAP.md` M1 row still cites `ADR-0001..0006` and omits `H-008` from the closed list (strategist re-ratifies at the gate).
- `docs/design/m1-core-and-poc.md`:45 keeps the retired "hook" idiom; say "no user-authored shell pipelines".
- `ARCHITECTURE.md` dead-field note omits the explicit "no compatibility promise" clause stated in `ADR-0006`.
