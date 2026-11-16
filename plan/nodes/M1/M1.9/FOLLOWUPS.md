# Follow-ups M1.9 (Gate Freeze: shipped with 0 BLOCKERs)

Merged from two Quad Review Gate rounds (`reviewer`, `review-legality`, `review-ioc`, `review-cruft`).
Round 1 was NO PASS (one false-evidence BLOCKER); round 2 passed all four lenses with zero
`BLOCKER`, so the items below are recorded instead of blocking `0.1.0`.

## BLOCKER (resolved before commit)

- **False evidence attestation** (`support/integrations.toml`, round 1): `incus-engine.signoff_commit`
  pointed at `09e8315` (this node's base), which does not touch the cited `ADR-0002`. Fixed:
  `d0b11e0` — the commit that flipped `ADR-0002` to `Accepted` — and `rationale` names that decision.
  All other `signoff_doc`/`signoff_commit`/`recipe_page`/`critical_test` pointers were re-audited.
- **Empty `coverage_report` silently enabled coverage with zero formats** (round 1): now fails closed
  with `DagError::EmptyCoverageReport`, documented in `ADR-0006` §5 / `ARCHITECTURE.md` §3.5.
- **`per_world = true` fixtures could be `CacheAction::Cached`** (round 1): they are now always
  `Rebuild` and never consult the world-independent `DigestCache` (`ADR-0003` §1).

## MAJOR (owner: M2 — must land before the M2 support-registry / edge work renders tiers)

1. **`ADR-0007` tier vocabulary cannot classify a shipped, conformance-tested adapter with no user
   recipe** (`review-ioc` `SPEC_BLIND_SPOT`, `review-legality` MAJOR). `incus-engine` is typed `none`
   ("no meaningful test surface") while its adapter passes the shared `WorldEngine` conformance
   suite; its `rationale` discloses the gap, so the claim is under-stated rather than false, but the
   machine-readable tier is wrong. Before M2's `slipfork support [--json]`/docs matrix renders any
   tier, file a `direction-challenge`: either add a rung between `none` and `declared`
   (e.g. `harnessed` = adapter + shared conformance suite, no published recipe) or redefine `none`.
   Do **not** invent a tier locally.
2. **`ARCHITECTURE.md`:13/26 claim `crates/slipfork/src/main.rs` wires `SupportRegistry::embedded()`**,
   but `main.rs` imports only `DagPlanner`/`FsDigestCache`/`FsMerkleHasher`/`SlipforkConfig`. Retarget
   that text and the `CliCtl --> SR` edge to M2 (schema + embedded file ship in `0.1.0`; composition-root
   wiring and the `slipfork support` command land in M2).
3. **`ADR-0003` §1 (outside this node's paths):** still calls `per_world` an "M2/M3 schema extension"
   (the schema ships in `0.1.0`), and its normative hash tuple omits the `per_world` discriminator the
   planner hashes plus the write-side rule that per-world digests never enter the shared cache.
4. **`ADR-0007` status/§2 (outside this node's paths):** says the registry is "M2/M3 target — not in
   `0.1.0`" although `SupportRegistry`/`embedded()`/`support/integrations.toml` ship here. Reword to
   "schema + embedded file in `0.1.0`; CLI view, `make check` ceiling and `make support-sync` in M2/M3".

## MINOR

- `ARCHITECTURE.md`:263 still says "target schemas" for `[test]`/`[fixtures]` types that now ship; the
  §3.6 block also omits the shipped `IntegrationTier`/`EvidenceField` `Display` impls and
  `RegistryError::MissingEvidence`.
- `ARCHITECTURE.md`:453 claims "every path exists" while `critical_support.rs` only checks
  tier-required pointers (an extra optional pointer escapes the check); iterate all present
  `EvidenceField`s or qualify the sentence.
- `critical_dag.rs` duplicates the per-world TOML block; `critical_support.rs` repeats the `none`
  record four times inside a long test and re-implements the presence predicate (third copy).
- `support.rs` restates the nine evidence fields four times (struct, `EvidenceField::parse`, Display,
  test ladder) and `Display` duplicates the serde kebab-case strings; `CoverageReportFormat` keeps an
  unused `Serialize` derive.
- `support/integrations.toml` header over-attributes disk/git/CI checks to M2 (the cargo test already
  checks paths/symbols) and `integrations.toml` "amendment that established the decision" overstates
  what `d0b11e0` did (Status flip to `Accepted`).
- `reflink-netns-world-engine` cites a CLI-flow recipe page against a library-level critical test — pick
  a recipe page/symbol pair that describes the same surface.
- `crates/slipfork-dag/Cargo.toml` `publish = false` is asymmetric while sibling crates stay
  publishable with no crates.io flow; drop it or apply it workspace-wide.
- `README.md`:58, `docs/VISION.md`:48 and `docs/design/m1-core-and-poc.md`:13 still place
  `support/integrations.toml` in M2 only (true for the CLI view; the file/schema ship in `0.1.0`).
- M3 `TestCoordinator` must never write per-world digests into the shared cache — state that
  write-side rule where `DigestCache` is specified.
- `ADR-0007` §3: have the M2 `--json` view emit `required_evidence` so the `make check` ceiling can
  consume the one Rust ladder instead of re-deriving it in Python.
- `ADR-0007` §status milestone wording ("before `0.1.0` merge **or** in M2") is resolved by this node
  (file shipped pre-merge) and should say so.
