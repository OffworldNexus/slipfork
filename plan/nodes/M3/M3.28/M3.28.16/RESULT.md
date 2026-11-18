# Result M3.28.16 — Guard rail: composition roots may not wire stub adapters

**STATUS: done.** The guard is implemented, green on the real tree, and enforced by plain `make check`.
`M3.28.15` (`a364b31`) deleted the `UnavailableTunnelRegistrar` surface that this node originally caught,
so the `check:` wiring is re-armed in the same change that turned it green.

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
9. the real repository -> `0` (the guard's own regression test).

### `Makefile`

New `stub-check` prerequisite of the existing `check` gate, directly after `guard` — no target that
`check` does not already run. The wiring was parked while the pre-`M3.28.15` tree still carried the
`UnavailableTunnelRegistrar` composition-root wiring (a red gate for every other node); it is re-armed
now that `a364b31` deleted that surface and the tool exits `0` on the real tree:

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
$ uv run --no-project python tools/check_stub_adapters.py --check all; echo "exit=$?"
check_stub_adapters: OK (91 production file(s), 6 composition root(s))
exit=0
```

Doctored fixture A (`/tmp/opencode/stub-fixture`, an `Arc::new(UnavailableTunnelRegistrar::new())` in a
`daemon.rs` root at line 12):

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
.........                                                                [100%]
9 passed in 1.38s
```

```console
$ grep -q 'check_stub_adapters' Makefile && echo wired
wired
$ grep -q 'check_stub_adapters.py' docs/knowledge/LESSONS.md && echo lesson-recorded
lesson-recorded
$ uv run --no-project tools/guard.py
guard: OK (597 file(s) checked)          # exit 0
$ uv run --no-project tools/plan.py validate
plan OK                                  # exit 0
```

`make check` (every prerequisite, end-to-end):

```console
$ make check
uv run --no-project tools/plan.py validate              -> plan OK
uv run --no-project tools/guard.py                      -> guard: OK (598 file(s) checked)
uv run --no-project tools/check_stub_adapters.py --check all
check_stub_adapters: OK (91 production file(s), 6 composition root(s))
uvx --offline ruff check tools poc                      -> All checks passed!
uvx --offline ruff format --check tools poc             -> 15 files already formatted
uv run --no-project --with pytest pytest -q tools/tests -> 150 passed in 15.89s
uv run --no-project tools/check_support.py --check      -> check_support: OK (10 integration(s); …)
bun test packages/opencode-slipfork/tests               -> 16 pass, 0 fail, 99 expect() calls
cargo fmt --all -- --check                              -> OK
cargo clippy --workspace --all-targets -- -D warnings   -> OK
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps -> OK
cargo test --workspace --lib --bins                     -> every suite ok
cargo test --workspace --doc                            -> ok
cargo test -p slipfork --test critical_l0_wiring -- --test-threads=1
error[E0308]: mismatched types
   --> crates/slipfork/src/main.rs:823:9
    |
823 |         fork,
    |         ^^^^ expected `String`, found `Option<String>`
make: *** [Makefile:109: rs-test] Error 101          # exit 2
```

Every prerequisite through `cargo test --workspace --doc` is green, including the new `stub-check`.
`rs-test` then aborts compiling `crates/slipfork` on **a concurrent node's uncommitted, half-applied
edit**: `git diff` at that moment shows `ForkOptions.fork: String` -> `Option<String>` in `main.rs` with
the matching `cli.rs` churn, applied after the passing `cargo test --workspace --lib --bins` and before
the `critical_l0_wiring` rebuild. This node touches no Rust file, so the failure is theirs and transient;
re-run `make check` once that edit settles.

## Files touched

- `tools/check_stub_adapters.py` (new, 391 lines)
- `tools/tests/test_check_stub_adapters.py` (new, 200 lines)
- `Makefile` (`stub-check` target + `check` prerequisite)
- `docs/knowledge/LESSONS.md` (one bullet)
- `plan/nodes/M3/M3.28/M3.28.16/RESULT.md`

Not committed.
