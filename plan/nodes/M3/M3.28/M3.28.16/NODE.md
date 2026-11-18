---
id: M3.28.16
title: "Guard rail: production composition roots may not wire stub adapters (tools/check_stub_adapters.py)"
kind: task
status: done
owner: implementer
priority: 2
depends_on: [M3.28.15, M3.29.3]
paths:
  - tools/check_stub_adapters.py
  - tools/tests/test_check_stub_adapters.py
  - Makefile
  - docs/knowledge/LESSONS.md
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-17T17:41"
updated: "2026-11-18T03:48"
---

# Goal

Close the bug class that shipped both `slipfork share` (wired to `UnavailableTunnelRegistrar`, which always fails) and `slipfork dag plan` (a command with nothing useful to say): a guard rail that fails the build when a **production composition root wires a stub, unavailable, or `todo!()` adapter**. A command whose only production adapter is fail-closed is vaporware; `make check` must refuse to ship it.

# Context

- `slipfork share` reached `UnavailableTunnelRegistrar::register()`, which unconditionally returned `Err(TunnelError::DaemonUnavailable)` — deleted by `M3.28.15`.
- `dag plan` shipped with no `0.1.0` value behind it — deleted by `M3.28.11`.
- Both were review-catchable only by a human reading the wiring; `tools/guard.py` (placeholders/hygiene), `tools/critical_guard.py` (protected tests) and `tools/check_docs_honesty.py` (docs vs real subcommands) are the existing gate style — mirror them.

# Design / Invariants

1. **`tools/check_stub_adapters.py`** — stdlib-only, PEP-723 header, `--check all` (default), exit `0`/`1`, output `path:line: message`, mirroring `tools/guard.py`'s structure and message style.
2. **Composition roots** = `crates/*/src/main.rs`, `crates/*/src/bin/*.rs`, and `crates/*/src/daemon.rs` (the wiring points named in `ARCHITECTURE.md` §1). Files under any `tests/` tree or inside `#[cfg(test)] mod` are excluded everywhere.
3. **Fatal findings:**
   - a bare `unimplemented!()` or `todo!()` anywhere in a production source file (`crates/*/src/**/*.rs`) outside `#[cfg(test)]`;
   - a stub instantiation in a composition root: `Type::new(...)`, `Arc::new(Type …)`, `Box::new(Type …)`, `Rc::new(Type …)` or `let … = Type {` where `Type` matches `^(Unavailable|Unimplemented|Todo|Stub|Fake)`;
   - a `use …::{… Unavailable* | Unimplemented* | Todo* | Stub* | Fake* …}` import in a composition root.
4. **Warning (reported, never fatal under `--check all`):** `TODO`/`FIXME` comments inside a composition root. Keep the two severities visible in the output (`WARN` vs `FAIL`) so the file is still usable as a review aid, but only fatal findings change the exit code.
5. **No false positives on error variants:** identifiers like `EdgeError::TunnelUnavailable { .. }`, `RemoteResourceUnavailable`, `TpmdUnavailable` are **error variants**, not adapters — only *constructed or imported types* whose own identifier starts with those prefixes are flagged. Tests pin this.
6. **`tools/tests/test_check_stub_adapters.py`** — pytest fixtures in `tmp_path` proving: (a) a clean mini-tree exits `0`; (b) `Arc::new(UnavailableTunnelRegistrar::new())` in `crates/x/src/daemon.rs` fails and prints exactly `crates/x/src/daemon.rs:<line>: …`; (c) `todo!()` in `crates/x/src/lib.rs` fails, while the same inside `#[cfg(test)] mod tests` and inside `tests/` passes; (d) an error-variant mention (`Err(X::TunnelUnavailable { .. })`) passes; (e) the "clean tree" case is the **real repository** run (`--check all` exits `0` after `M3.28.15`).
7. **`Makefile`:** the plain `make check` gate runs it next to `guard.py`/`check_docs_honesty.py`; no new `make` target that `check` does not already invoke.
8. **`docs/knowledge/LESSONS.md`:** one bullet — *a CLI command whose only production adapter is fail-closed is vaporware; the composition root must never wire `Unavailable*`/`todo!()` — guard rail `tools/check_stub_adapters.py` enforces it.*
9. Stdlib only, no new dependency, no network, no writes outside `tmp_path` in tests.

# Acceptance

```console
$ uv run --no-project python tools/check_stub_adapters.py --check all; echo "exit=$?"
check_stub_adapters: OK (N production file(s), M composition root(s))
exit=0
$ uv run --no-project --with pytest pytest -q tools/tests/test_check_stub_adapters.py
... passed
$ uv run --no-project python tools/check_stub_adapters.py --check all --root /tmp/opencode/stub-fixture; echo "exit=$?"
/tmp/opencode/stub-fixture/crates/x/src/daemon.rs:12: FAIL composition root wires unavailable adapter `UnavailableTunnelRegistrar`
exit=1
$ grep -q 'check_stub_adapters' Makefile && echo wired
wired
$ grep -q 'check_stub_adapters.py' docs/knowledge/LESSONS.md && echo lesson-recorded
lesson-recorded
$ make check
... exit 0
```

- `--root` is the escape hatch the tests and the manual fixture use; without it the tool checks the repository root exactly like `guard.py`.
- `make check` must be green **on the current tree after `M3.28.15`** — the guard exists to keep it green, not to fail today.
- `RESULT.md` records the tool LOC, the test LOC, and the two doctored-fixture transcripts with their `file:line`.
