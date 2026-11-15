# M1.6 Follow-ups (post-Gate-7, committed under the Gate-8 freeze)

**Rule (conductor, binding):** Gate 8 is the final gate for `M1.6`. From Gate 8 onward only
`BLOCKER` findings (wrong behavior, secret leak, fail-open, broken acceptance, vaporware docs)
stop the commit. Everything below is a known, consciously deferred non-blocker.

**Why this is acceptable:** `0.1.0` is the design-of-record + real-CoW-PoC release. The workspace
is `< 1.0.0`, so Iron Law #3 gives us zero backward-compatibility burden and full freedom to
refactor helper internals later. Helper-level duplication is not the shipping bar for the design
of record; the ADRs and fail-closed contracts are.

## Code / tests

1. `crates/slipfork-world/src/lib.rs`: in-module `normalize_host_strips_one_numeric_port_and_trailing_dot`
   duplicates `tests/critical_world.rs` cases; keep only the overflow case (move `test-99999999999`
   into the route suite) and delete the duplicate.
2. `slipfork_world::is_reserved_test_slot_label` is `pub` only so a test can assert it; behaviour is
   already covered through `validate_fork_name("test-3")` → make it private.
3. `crates/slipfork/tests/critical_slug.rs`: the `create_dir_all`/`git init`/`snow.yml` scaffold is
   copy-pasted per test → extract one `git_repo_with_snow(scratch, name)` fixture.
4. `crates/slipfork/src/main.rs`: a failed `--git-dir` probe is swallowed by
   `unwrap_or_else(|_| canonical_start)` inside a fail-closed diagnostic → report the detection
   error instead of substituting a plausible-looking path.
5. `crates/slipfork/src/main.rs`: the `--file`-only arm does not require `parent(file) == vcs_root`,
   so `<repo>/sub/slipfork.toml` can pair a subdirectory config with root-wide input globs (the
   `--file` + `--repo` arm already rejects this divergence) → apply the same check.
6. `crates/slipfork/src/main.rs`: the `--file` + `--repo` arm probes `--repo` eagerly, so its
   diagnostic can name `--repo` when the real inconsistency is `--file`'s VCS root.
7. `crates/slipfork-snow/src/model.rs`: `RejectedKey` has two error shapes
   (`TopLevelEnvsUnsupported` early return vs a `Validation` string) → collapse to one
   forbidden-key variant.
8. `SnowFile::effective_run_command` is `pub` with no production caller until M2 supervision
   (docs already label per-mode `run_command` supervision M2).
9. `HOME` unset silently falls back to `.` for vault/state paths → fail closed with an actionable
   "set HOME" error.
10. Extract a `RepoRootResolver` port: ~300 lines of git/jj root topology are free functions in the
    binary's composition root; M2 should expose the adapter behind a trait wired at the CR.
11. `FsMerkleHasher`/`FsDigestCache` (`std::fs` adapters) live inside the `slipfork-dag` domain
    crate; relocate beside the other concrete adapters at the composition root (M2).
12. `ARCHITECTURE.md` `WorldSpec.target: WorldTarget` implies a `slipfork-engine -> slipfork-world`
    edge that no `Cargo.toml` declares yet (drawable, added in M2).

## Docs

13. `ADR-0005` §3 says every root refusal points at `project = "<slug>"`; carve out bare repos,
    which instead get reachable `git clone` guidance (no `slipfork.toml` exists there).
14. `docs/reference/manifest.md` `[test].after_suite` sample relies on `$SLIPFORK_RUN_ARTIFACTS_DIR`,
    defined only in ADR-0006 (excluded from the published site) → define both env vars in one
    published line.
15. `docs/reference/cli.md`: state that `snow resolve --file`'s directory must live inside a git/jj
    repo, and that an explicit `--file` + `--repo` pair must name the same canonical VCS root.
16. `docs/reference/manifest.md` "unknown fields" claim is top-level-only: nested component structs
    do not deny unknown fields (verified live) → reword, or add `deny_unknown_fields` to nested
    structs plus a fail-closed critical assertion.
17. `plan/HUMAN.md` H-005 answer text predates the `ProjectSlug` ruling. It is a historical owner
    answer and is never hand-edited; treat ADR-0005 §3 as the current specification.

## Surfaced at the final Gate 8 (ranked non-blocking by all four reviewers)

21. **Secret hygiene (highest priority of this list):** `crates/slipfork-snow/src/model.rs`
    `ResourceCoordinates`/`S3Coordinates` derive `Debug` with a `pub secret_key`, and `RejectedKey`
    derives `Debug` over a raw `serde_json::Value` (a top-level `envs:` carrying literal secrets
    would print on `{:?}`). No current call path prints them, but add redacting/opaque `Debug`
    implementations before any logging surface lands.
22. `crates/slipfork-world/src/conformance.rs` (in-memory `TunnelRegistrar` double) ships inside the
    production library as `pub mod conformance` → gate behind a `conformance` feature or a dev-only
    module so test doubles are not part of the shipped API surface.
23. `LocalHostRoute::parse` calls `Self::new` three times in the single-label branch → one
    resolve-then-construct flow.
24. `crates/slipfork/src/main.rs`: `serde_json::to_string_pretty(...).unwrap_or_default()` would
    print a blank line and exit 0 on a (currently impossible) serialization failure.
25. `crates/slipfork/src/main.rs`: the CLI fabricates `WorldHandle { .. }` and uses
    `state_dir.exists()` as a liveness probe; M2's `WorldRegistry` should issue/validate handles so
    callers cannot synthesize one.
26. `packages/opencode-slipfork`: `existsSync(snowPath)` inside the gateway hides fs state — pass an
    explicit optional `snowFile` through `SlipforkGateway.wrapShellCommand` so the coordinator can
    express and test it.
27. `crates/slipfork-world/src/conformance.rs` `is_lease_active` is `pub` only for its own test.

## Deferred by design (M2 / M3)

18. `WeavedRegistrar` wire adapter stays gated on `tunnel-weaver` publishing `weaved`'s real IPC
    specification (H-006); only pure traits + in-memory conformance ship now.
19. Daemon-owned tunnel lease lifecycle, `DaemonControl` IPC surface, and the `VcsWorkspace`
    conformance suite ship with M2 (`slipforkd`, `git-remote-slipfork`, `jj workspace`).
20. `[test].after_suite` execution (and `RunnerProtocol v1`) is M3; the field is parsed/validated
    now, not executed.
