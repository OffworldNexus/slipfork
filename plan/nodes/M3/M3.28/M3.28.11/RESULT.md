# RESULT M3.28.11 — `dag` deleted from `0.1.0` (CLI + docs sweep)

Attempt 2 landed the settled scope: the `dag` subcommand and the machinery that only it reached are
gone, every published mention is gone in the same change, and the retained `0.2.0` planner/support
machinery is untouched. Quiet world entry and attributed `logs` moved to `M3.28.22`/`M3.28.23` and
are not part of this diff.

## Deleted

| What | Where |
|---|---|
| `Commands::Dag` + `DagAction` (no rename, no alias, no `#[deprecated]`) | `crates/slipfork/src/cli.rs` |
| `run` arm, `dag_plan` renderer, `resolve_dag_paths`, now-unused `parent_dir`, and the `DagAction` / `load_slipfork_config` / `DagPlanner` / `FsDigestCache` / `FsMerkleHasher` / `BTreeMap` imports | `crates/slipfork/src/main.rs` |
| `features = ["fs"]` on the `slipfork-dag` dependency (`project.rs` and `daemon.rs` compile without it; nothing outside `slipfork-dag` names `FsDigestCache`/`FsMerkleHasher`) | `crates/slipfork/Cargo.toml` |
| the five `dag plan` blocks: worktree-default plan, explicit `--file`/outside-repo refusal, standalone empty-input plan + cache-poisoning assertions, `invalid_project_label` dag assertions, and `dag_plan_repo_flag_is_gone_and_rejected_by_the_parser` | `crates/slipfork/tests/critical_slug.rs` (now 6 tests, was 7) |
| the two `dag plan` rows + the `doctor`/`dag plan` half-sentence | `docs/reference/cli.md` |
| the whole page | `docs/testing-dag.md` |
| its nav entry `"Orthogonal DAG planner"` | `zensical.toml` |
| the `dag plan` sentence and table row, and the `DAG planner` link | `docs/index.md` |
| the `dag plan` link | `docs/getting-started.md` |
| the `dag plan` sentence, the `doctor and dag plan` clause, the "snow.yml resolver and DAG planner" bullet, and the guide link | `README.md` |
| the false "`0.1.0` ships … the `dag plan` planner" claim | `AGENTS.md` line 3 |
| a stale `slipfork dag plan` CLI example in the retained registry's doc comment | `crates/slipfork-dag/src/support.rs` |

## Kept (not unreachable — `0.2.0` `slipfork test` engine, pinned by protected suites)

`DagPlanner`, `DagPlan`, `InputHasher`/`DigestCache`, `FsDigestCache`, `FsMerkleHasher` (and the
`fs` feature inside `slipfork-dag`), `SupportRegistry`, `Evidence`/`EvidenceField`,
`IntegrationCategory`, `SlipforkConfig`, `TestSpec`/`CoverageConfig`/`CoverageThreshold`,
`ResourcePlan`/`ResourceStrategy`/`RemoteResourceOffer`. `slipfork support` stays wired to
`SupportRegistry`/`category_label`/`evidence_json`; `critical_dag` and `critical_support` ran
unchanged.

## Acceptance (all commands run)

| Check | Result |
|---|---|
| `cargo run -q -p slipfork -- dag plan; echo exit=$?` | `error: unrecognized subcommand 'dag'` … `exit=2` |
| `cargo run -q -p slipfork -- --help \| grep -c dag` | `0` |
| `cargo run -q -p slipfork -- --help \| grep -c -- '--verbose'` | `1` (flag intact) |
| `uv run --no-project tools/check_docs_honesty.py` | `check_docs_honesty: OK (10 doc(s) match real CLI subcommands …)` |
| `test ! -e docs/testing-dag.md && ! grep -q 'Orthogonal DAG planner' zensical.toml` | `page-gone` |
| `! grep -rn 'DagPlanner\|FsDigestCache\|FsMerkleHasher' crates/slipfork/src` | `cli-planner-free` |
| `cargo test -p slipfork-dag --test critical_dag` | `11 passed; 0 failed` |
| `cargo test -p slipfork-dag --test critical_support` | `4 passed; 0 failed` |
| `cargo test -p slipfork --test critical_cli -- --test-threads=1` | `28 passed; 0 failed` (150.76 s) |
| `cargo test -p slipfork --test critical_slug -- --test-threads=1` | `6 passed; 0 failed` (8.64 s) |
| `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` / `RUSTDOCFLAGS=-D warnings cargo doc` | clean / clean / clean |
| `uv run --no-project tools/check_support.py --check` | `OK (10 integration(s); all evidence pointers verified …)` |
| `uv run --no-project tools/guard.py` | `guard: OK (603 file(s) checked)` |
| `uv run --no-project tools/plan.py validate` | `plan OK` |
| `make docs-check` | `No issues found` + docs honesty OK |
| `make check` | **exit 0** in an isolated worktree of `97efc4a` + this exact patch only (see below) |

`[resources]` behind `-v`/`--json` was verified as already satisfied, not redone:
`crates/slipfork/src/restart.rs:50-75` renders the rows only when `options.verbose`, and
`BootEvent::Resources` is already a no-op in `main.rs`.

### Why `make check` was verified in an isolated worktree

The shared working tree also carries **M3.28.22's uncommitted quiet-entry work**
(`crates/slipfork-engine/src/**` + `crates/slipfork-engine/tests/critical_engine.rs`, neither in
this node's paths, neither touched here). Its new `conformance.rs` assertion
(`entry.cwd == "/"`) currently fails against `slipfork-edge`'s stub engine
(`stub_world_engine_passes_the_shared_conformance_matrix`), so `make check` aborts there in the
shared tree — a sibling-in-flight failure, not this diff. To verify this slice honestly I created
`.worktrees/m3-28-11-verify` at base `97efc4a`, applied only the 15 files above, and ran the full
gate:

```console
$ make check
… critical_cli 28 passed … critical_slug 6 passed … critical_btrfs_live 1 passed …
uvx --offline zensical build --strict
No issues found
check_docs_honesty: OK (10 doc(s) …)
make-exit=0
$ uv run --no-project tools/critical_guard.py --base 97efc4a --node M3.28.11
critical-guard: 97efc4a..working tree, 15 changed file(s)
  M  crates/slipfork/tests/critical_slug.rs  [authorized by crates/slipfork/tests/critical_slug.rs]
critical-guard: OK: authorized by M3.28.11
```

The worktree was removed afterwards; nothing outside the listed files remains.

In the shared tree, `critical_guard --base 97efc4a --node M3.28.11` reports the sibling's
`M crates/slipfork-engine/tests/critical_engine.rs [VIOLATION: not covered by any node path]`
alongside the authorized `critical_slug.rs` change. This node did not edit that file.

## Out-of-path consequence that had to travel with the page deletion

`tools/check_support.py --check` (part of `make check`) verifies every `recipe_page` pointer is an
existing file, and `support/integrations.toml` pointed the `dag-config-cli` entry at
`docs/testing-dag.md`; deleting the page failed the gate (`'recipe_page' pointer
'docs/testing-dag.md' does not exist`). Two minimal truth edits repaired it:

- `support/integrations.toml`: `dag-config-cli.recipe_page` -> `docs/reference/manifest.md`, the
  page that actually documents `[artifacts]`, `[fixtures]`, and `[test]`.
- `docs/reference/manifest.md`: the sentence that said "`dag plan` reads it explicitly" now says
  `0.1.0` validates them at boot but ships no command that materializes a plan.

`docs/support-matrix.md` renders field names only (no paths), so it stayed byte-identical and
`check_support` reports "in sync". Both files need to be authorized when this change is committed.

## Residuals for the lead (not in this node's paths)

1. `ARCHITECTURE.md:92` still claims `M3.28.11` "deletes `src/fs.rs` `FsMerkleHasher`/`FsDigestCache`
   and the `fs` feature alongside `slipfork dag`". This node deliberately keeps them (invariant 2);
   the line should say the CLI's `features = ["fs"]` is dropped and the machinery stays unshipped
   for `0.2.0`.
2. `poc/run_benchmark.py` still measures `slipfork dag plan` (`:11,1796-1798,1961,2131,2136`);
   `make bench` is opt-in and outside `make check`, but that harness now drives a deleted verb and
   needs a sweep.
