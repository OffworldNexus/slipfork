# Result M3.28.16 — Guard rail: composition roots may not wire stub adapters

**STATUS: blocked** — `BLOCK_KIND: missing-input` (dependency `M3.28.15` has not executed).

The guard is implemented, tested against doctored fixtures, wired into `make check`, and it *correctly*
fails the current tree because the vaporware this node exists to prevent is still present. Acceptance
item 6(e) ("the real repository exits `0`") and the `# Acceptance` line "`make check` must be green on
the current tree after `M3.28.15`" cannot pass until `M3.28.15` deletes that wiring; making them pass
today would require weakening the guard (out of rules) or editing `crates/**` (outside this node's
`paths`). No fallback, allow-list, or `xfail` was added.

## Blocker evidence (unmet dependency `M3.28.15`)

- `plan/nodes/M3/M3.28/M3.28.15/NODE.md` -> `status: ready` (never started; its `LOG.md` has planner/lead
  edits only). `M3.28.16` declares `depends_on: [M3.28.15, M3.29.3]`.
- The tree at `4a1428f` still wires the deleted-surface adapter in a production composition root:

```console
$ uv run --no-project python tools/check_stub_adapters.py --check all; echo "exit=$?"
/home/remy/offworld-nexus/slipfork/crates/slipfork/src/daemon.rs:778: FAIL composition root wires unavailable adapter `UnavailableTunnelRegistrar` — a fail-closed adapter makes the command vaporware
/home/remy/offworld-nexus/slipfork/crates/slipfork/src/daemon.rs:34: FAIL composition root imports stub adapter `UnavailableTunnelRegistrar` — wire the real adapter or remove the import
check_stub_adapters: FAIL (2 fatal, 0 warning(s)) across 92 production file(s), 6 composition root(s)
exit=1
```

Both findings are the exact pattern this node was specified to reject (`crates/slipfork/src/daemon.rs:778`
= `Arc::new(UnavailableTunnelRegistrar::new())` passed to `WorldRegistry::new`; `:34` = the multi-line
`use slipfork_edge::{...}`). Both disappear when `M3.28.15` lands; nothing in this node needs to change.
They are not in a `#[cfg(test)]` item (the test-module `Arc::new(FakeClock { .. })` at `:4513` and
`Arc::new(StubRootfs)` at `:4987` are correctly ignored).

## What was implemented

### `tools/check_stub_adapters.py` (new, 391 lines, stdlib only, PEP-723 header)

Mirrors `tools/guard.py` (frozen `Finding` dataclass, `CheckError`, `os`-free single `main() -> int`,
`--root` via `git rev-parse --show-toplevel` when omitted, `path:line: message` rendering, one summary line).

- **Production tree:** `crates/*/src/**/*.rs`; every path with a `tests` component is skipped.
- **Composition roots:** `crates/*/src/main.rs`, `crates/*/src/bin/*.rs`, `crates/*/src/daemon.rs`
  (`ARCHITECTURE.md` §1 wiring points). 6 roots on the real tree.
- **Test-code exclusion:** `mask_rust()` blanks line/block comments, `"…"`, `r#"…"#`, `b"…"`, and char
  literals at equal length; `test_item_spans()` then brace-matches every `#[cfg(test)]`-attributed item
  (`mod`, `use`, or any other item) and blanks it, so test doubles never trip a production rule.
- **FAIL:** `todo!()`/`unimplemented!()` in any production source; stub instantiation in a composition
  root (`Type::new`, `Arc|Box|Rc::new(Type`, `let … = Type {`) and `use` imports whose identifier starts
  with `Unavailable|Unimplemented|Todo|Stub|Fake`. Findings are deduplicated per `path:line`.
- **WARN:** `TODO`/`FIXME` in a composition root (never changes the exit code). Severity is printed
  verbatim: `…:12: FAIL …` / `…:7: WARN TODO …`.
- **Exit:** `0` clean, `1` fatal findings or an uninspectable root. `--check all` is the default and the
  only check set.

### `tools/tests/test_check_stub_adapters.py` (new, 200 lines, pytest)

`subprocess`-driven, every fixture under `tmp_path`:

1. clean mini-tree -> `0`, exact `OK (3 production file(s), 2 composition root(s))`;
2. `Arc::new(UnavailableTunnelRegistrar::new())` in `crates/x/src/daemon.rs` -> `1`, exact
   `<root>/crates/x/src/daemon.rs:4: FAIL composition root wires unavailable adapter …`;
3. `let probe = FakeHostProbe { … }` in `daemon.rs` -> `1` on the literal's line;
4. multi-line `use slipfork_edge::{EdgeError, UnavailableTunnelRegistrar, …}` in `main.rs` -> `1` on the
   identifier's line;
5. `todo!()` in `lib.rs` -> `1`; the same inside `#[cfg(test)] mod tests` **and** inside `crates/x/tests/`
   -> `0`;
6. error variants (`EdgeError::TunnelUnavailable { .. }`, `DagError::RemoteResourceUnavailable { id }`,
   `TpmdUnavailable`, `RegistryUnavailable`) in a composition root -> `0`, no `FAIL` line (pinned false-positive test);
7. `// TODO: …` in `daemon.rs` -> `0` with a `WARN` line and no `FAIL`;
8. missing `crates/` root -> `1` `cannot inspect tree` (never a silent pass);
9. the real repository -> `0` (blocked today, see above).

### `Makefile`

New `stub-check` prerequisite of the existing `check` gate, directly after `guard` — no target that
`check` does not already run:

```make
check: plan-check guard stub-check py-lint py-test support-check ts-test rs-fmt rs-lint rs-test docs-check
stub-check: | $(TMPDIR) ## Refuse production composition roots that wire stub/unavailable adapters
	uv run --no-project tools/check_stub_adapters.py --check all
```

### `docs/knowledge/LESSONS.md`

One bullet: a CLI command whose only production adapter is fail-closed is vaporware; the composition
root must never wire `Unavailable*`/`Unimplemented*`/`Todo*`/`Stub*`/`Fake*` (nor `todo!()`), enforced
by `tools/check_stub_adapters.py` in `make check`.

## Verification (real commands & exit codes)

```console
$ uv run --no-project python tools/check_stub_adapters.py --check all --root /tmp/opencode/stub-fixture; echo "exit=$?"
/tmp/opencode/stub-fixture/crates/x/src/daemon.rs:12: FAIL composition root wires unavailable adapter `UnavailableTunnelRegistrar` — a fail-closed adapter makes the command vaporware
check_stub_adapters: FAIL (1 fatal, 0 warning(s)) across 3 production file(s), 2 composition root(s)
exit=1
```

Doctored fixture B (`/tmp/opencode/stub-fixture-todo`, a `todo!()` in `lib.rs:3` plus an identical one
inside `#[cfg(test)] mod tests` and adversarial error variants in `daemon.rs`):

```console
$ uv run --no-project python tools/check_stub_adapters.py --root /tmp/opencode/stub-fixture-todo; echo "exit=$?"
/tmp/opencode/stub-fixture-todo/crates/x/src/lib.rs:3: FAIL `todo!()` in production source — implement the adapter or delete the caller; a fail-closed placeholder must never ship
check_stub_adapters: FAIL (1 fatal, 0 warning(s)) across 2 production file(s), 1 composition root(s)
exit=1
$ # after removing only the production `todo!()` (the `#[cfg(test)]` one and the error variants stay)
check_stub_adapters: OK (2 production file(s), 1 composition root(s))
exit=0
```

```console
$ uv run --no-project --with pytest pytest -q tools/tests/test_check_stub_adapters.py
........F                                                                [100%]
FAILED tools/tests/test_check_stub_adapters.py::test_real_repository_is_clean
1 failed, 8 passed in 1.41s
```

`test_real_repository_is_clean` is the only failing test and its assertion text is the two
`daemon.rs` findings quoted above — the `M3.28.15` blocker, not a guard defect.

```console
$ grep -q 'check_stub_adapters' Makefile && echo wired
wired
$ grep -q 'check_stub_adapters.py' docs/knowledge/LESSONS.md && echo lesson-recorded
lesson-recorded
$ uv run --no-project tools/guard.py
guard: OK (593 file(s) checked)          # exit 0
$ uv run --no-project tools/plan.py validate
plan OK                                  # exit 0
$ make py-lint
All checks passed! / 15 files already formatted   # exit 0
$ make py-test
FAILED tools/tests/test_check_stub_adapters.py::test_real_repository_is_clean
1 failed, 149 passed in 18.70s           # exit 2 (pytest exit 1) — same single blocked assertion
$ make support-check
check_support: OK (10 integration(s); …)  # exit 0
$ make ts-test
16 pass, 0 fail, 99 expect() calls       # exit 0
$ make docs-check
No issues found + check_docs_honesty: OK (11 doc(s) …)  # exit 0
$ make check
… guard: OK … check_stub_adapters: FAIL (2 fatal, 0 warning(s)) across 92 production file(s), 6 composition root(s)
make: *** [Makefile:36: stub-check] Error 1   # exit 2 — stops on the blocked M3.28.15 wiring
```

## Sibling `M3.28.9` compile/format blockers (their scope, `crates/slipfork/src/**`)

Re-run per the brief; neither is caused by this node (no Rust file was touched):

```console
$ make rs-fmt
Diff in /home/remy/offworld-nexus/slipfork/crates/slipfork/src/{browser.rs,cli.rs,main.rs,open.rs,ps.rs}   # 10 hunks, all M3.28.9 in-flight files
$ make rs-lint
error: unresolved link to `tests::the_remote_recipe_host_header_round_trips`
  --> crates/slipfork/src/open.rs:15:43   # new M3.28.9 file, not committed yet
```

`cargo clippy --workspace --all-targets -- -D warnings` passed; only the rustdoc `-D warnings` half of
`rs-lint` failed, inside `open.rs`. `make rs-test` (the remaining `check` prerequisite after `rs-lint`)
was therefore not reached; `make docs-check` was run separately and is green.

## Files touched

- `tools/check_stub_adapters.py` (new, 391 lines)
- `tools/tests/test_check_stub_adapters.py` (new, 200 lines)
- `Makefile` (`stub-check` target + `check` prerequisite)
- `docs/knowledge/LESSONS.md` (one bullet)
- `plan/nodes/M3/M3.28/M3.28.16/RESULT.md`

Not committed.
