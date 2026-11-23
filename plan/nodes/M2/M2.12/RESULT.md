# RESULT M2.12 — `JjWorkspaceAdapter` + `git-remote-slipfork` protocol handler

**Status:** complete (both adapters pass the identical, now *protected* conformance matrix; the Jj
family was verified against a **real** `jj 0.47.1`, and its absence path is verified with `jj` off
`PATH`).

## What landed (`crates/slipfork-vcs/**` only)

| File | Change |
|---|---|
| `src/jj.rs` | New `JjWorkspaceAdapter`: `detect()`, `create_workspace()`, `JjWorkspace` (`refs`/`resolve`/`changes`/`forget`), rollback, teardown |
| `src/remote_helper.rs` | New pure `git-remote-slipfork` line protocol: `serve()`, `RemoteHelperError`, `MAX_FRAME_BYTES` |
| `src/ports.rs` | `VcsError::ToolMissing` carries an actionable `hint`; new `VcsWorkspace::resolve()`; canonical `WORKTREES_DIRECTORY`; trait-default `refs()` |
| `src/tool_command.rs` (new) | Replaces `src/git_command.rs`: one private `git`/`jj` runner (`Tool`), explicit argv, `LC_ALL=C`, scrubbed `GIT_*`, `jj` `current_dir` + missing-cwd guard |
| `src/git_store.rs` (new) | Ref-store primitives shared by both adapters: `ref_exists`, `delete_ref`, `resolve_commit`, `path_exists`, `remove_if_exists` |
| `src/git.rs`, `src/repo_root.rs` | Use the shared runner/store; `GitWorkspace::resolve`; duplicated consts and `refs()` impls deleted |
| `tests/critical_conformance_matrix.rs` (new, **protected**) | The whole `assert_vcs_workspace_conformance` body |
| `tests/support/mod.rs` | Fixtures/builders only: `Fixture`, `ConformanceRepos`, `GitRepos`, `JjRepos`, `RefWorkspace`, `jj_in`/`jj_ok_in`, `mkfifo`, `tool_missing_skip_line` |
| `tests/critical_vcs.rs` | `#[path = "critical_conformance_matrix.rs"] mod matrix;` + Jj conformance, jj-absent child probe, 9 `remote_helper_*` tests (incl. a real `git fetch slipfork` round-trip) |
| `Cargo.toml` | `autotests = false` + explicit `[[test]] critical_vcs` so the protected matrix file is only compiled as part of that suite |

### Test integrity (reviewer follow-up)

The matrix assertions no longer live in an unprotected helper. `crates/slipfork-vcs/tests/critical_conformance_matrix.rs`
matches `tools/critical_guard.py`'s protected `tests/critical_*.rs` pattern, and `critical_vcs.rs`
includes it via `#[path]`. No assertion remains only in `tests/support/mod.rs`; `support` now holds
fixtures and builders only. `autotests = false` + the explicit `[[test]]` keeps that file from also
compiling as a standalone target (which would have made it a second, weaker suite).

### Dedupes applied (cruft lens)

- One `ports::WORKTREES_DIRECTORY` (`pub const`, re-exported) used by both adapters instead of two
  copies of `.worktrees`.
- `VcsWorkspace::refs()` is now a **trait default** derived from `fork_name()` through
  `fork_branch_ref`/`fork_snapshot_ref` — the two byte-identical impls are gone, so the canonical
  D/F-safe layout is defined exactly once.
- The `MAX_WORKSPACE_CHANGES` parser dedupe was skipped: both adapters parse different record
  formats (`git status -z` NUL records vs `jj diff` template lines), so sharing it would have grown
  the diff and hidden two genuinely different parsers behind an abstraction.

## Jj mapping (`ADR-0005` §3)

- `create_workspace`: `jj workspace add <vcs_root>/.worktrees/<fork> --name <fork> --revision <requesting @>`
  — the new workspace's working copy is a fresh commit whose parent is the requesting checkout's
  `@`, so the fork inherits the developer's uncommitted state.
- Ref projection is taken from the **requesting** checkout before anything is created (every `jj`
  command snapshots the working copy it runs in): `@-` → `refs/heads/<fork>`,
  `@` → `refs/slipfork/<fork>/wip`, collapsing to `@-` when
  `jj diff --from @- --to @ --summary` is empty (a clean Jujutsu working copy is still its own
  commit, so tree comparison — never commit-id comparison — decides emptiness).
- Both refs are written into the colocated Git store with `git update-ref`, so they are
  Git-fetchable and never create `refs/heads/<fork>/wip` (D/F lock). A repository without a
  colocated `.git` fails closed with `VcsError::UnsupportedTopology` + the `jj git init --colocate`
  hint.
- `changes()` = `jj diff --from <refs/heads/<fork>> --to @ -T 'status_char ++ " " ++ path ++ "\n"'`;
  Jujutsu snapshots every file, so `A` (absent from the fork branch) is reported as
  `WorkspaceChanges::untracked` and `M`/`D`/`R`/`C` as `tracked` — documented in the module docs.
- `forget()`: `jj workspace forget <fork>` when registered, then the checkout (Git worktree when
  `.worktrees/<fork>/.git` exists, else `remove_dir_all`), then both refs — idempotent.
- Missing tool: `VcsError::ToolMissing { tool: "jj", hint }` whose hint names
  `project = "<slug>"` in `slipfork.toml`; **no Git fallback anywhere**.

## Remote helper protocol

`capabilities` → `fetch`/`push`/`option`; `list`/`list for-push` → one `<sha> <refname>` line per
fork ref, blank-terminated; `fetch <sha> <ref>` batches are verified against
`VcsWorkspace::resolve` and answered with the batch-terminating blank line (the fork's commits live
in the caller's own object database, so there is nothing to transfer); `push` refuses every ref with
`error <dst> <why>`; `option` answers `ok`/`unsupported`/`error`; unknown verbs answer `unsupported`
and keep the session alive; frames are capped at `MAX_FRAME_BYTES` and non-UTF-8 input fails closed.

### Spec erratum (verified against Git 2.47.3)

`NODE.md` #4 says `<sha>\t<refname>`. Git's `gitremote-helpers(7)` grammar is
`"<value> <name> [<attr> ...]"` — **single space**. A tab-separated list makes every real fetch die
with `fatal: malformed response in ref list: <sha>\t<ref>` (reproduced live before implementing).
The handler therefore emits the documented space-separated form; the byte-exact test pins it.

## Shared conformance matrix

Both adapters run the *same* body; only repository preparation is injected (`ConformanceRepos`):
`GitRepos` = plain Git fixture, `JjRepos` = same fixture with `.worktrees/`/`.slipfork/` ignored
(committed) and then `jj git init --colocate`.

Three matrix adjustments were required to keep it honestly adapter-neutral (all empirically driven):

1. Removed the raw `git status --porcelain` block (duplicated by the port-level `changes()` view;
   meaningless inside a Jujutsu workspace).
2. Moved the "live `.git/index` inode/mtime untouched" assertion out of the shared body — Jujutsu's
   colocated mode legitimately maintains `.git/index` (measured: `jj workspace add` rewrites it).
   The invariant is still asserted for the Git adapter by the two Git-specific critical tests
   (`dirty_fork_snapshot_reproduces_modified_deleted_untracked_view`,
   `index_untouched_by_dirty_fork_snapshot`).
3. Added `resolve()` assertions for both refs (dirty: two distinct commits, `wip^ == base`; clean:
   `wip == base`).

## Jj verification method

The host has **no** system `jj` (`command -v jj` → none). A real upstream binary was used for
verification only, never added to the repository:
`jj 0.47.1-d16ee1fc88a72c54142241b079a34bbf3b033bc0` (official `x86_64-unknown-linux-musl` release)
at `~/.cache/slipfork-tools/jj/jj`. No mocking of `jj` was used anywhere.

## Exact command outputs

```console
$ cargo test -p slipfork-vcs --all-targets            # host PATH, jj not installed
running 7 tests
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
running 23 tests
test result: ok. 23 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.30s

$ PATH="$HOME/.cache/slipfork-tools/jj:$PATH" cargo test -p slipfork-vcs --all-targets
running 7 tests
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
running 23 tests
test result: ok. 23 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.26s

$ cargo test -p slipfork-vcs --test critical_vcs -- --nocapture conformance_matrix   # jj absent
running 2 tests
skipped: ToolMissing(jj)
test jj_conformance_matrix_satisfies_shared_suite ... ok
test git_conformance_matrix_satisfies_shared_suite ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 21 filtered out

$ PATH=…/jj:$PATH cargo test -p slipfork-vcs --test critical_vcs -- --nocapture jj_conformance
running 1 test
test jj_conformance_matrix_satisfies_shared_suite ... ok          # full matrix against real jj
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 22 filtered out

$ cargo test -p slipfork-vcs --test critical_vcs -- --nocapture jj_absent
running 2 tests
test jj_absent_child_probe_reports_tool_missing ... ok
test jj_absent_fails_closed_with_the_project_override_hint ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 21 filtered out

$ cargo test -p slipfork-vcs --test critical_vcs -- --nocapture remote_helper
running 9 tests
test remote_helper_capabilities_are_byte_exact ... ok
test remote_helper_fetch_batch_verifies_the_advertised_ids ... ok
test remote_helper_git_fetch_round_trip_from_a_scratch_repository ... ok
test remote_helper_list_advertises_base_and_wip_refs ... ok
test remote_helper_list_for_push_advertises_the_same_refs ... ok
test remote_helper_option_responses_follow_the_documented_vocabulary ... ok
test remote_helper_push_is_refused_for_every_ref ... ok
test remote_helper_rejects_oversized_frames ... ok
test remote_helper_unknown_verb_answers_unsupported_and_keeps_serving ... ok
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 14 filtered out

$ ! test -d crates/slipfork-vcs/src/bin          # exit 0 (no directory)
$ ! rg -q "AdapterKind" crates/slipfork-vcs/src/ports.rs crates/slipfork-vcs/src/remote_helper.rs
                                                 # exit 0 (no match; zero matches crate-wide)
$ cargo clippy -p slipfork-vcs --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.31s
$ cargo fmt --all -- --check                     # exit 0
$ RUSTDOCFLAGS="-D warnings" cargo doc -p slipfork-vcs --no-deps
    Finished `dev` profile …   Generated …/target/doc/slipfork_vcs/index.html
$ uv run --no-project tools/guard.py
guard: OK (274 file(s) checked)
$ uv run --no-project tools/critical_guard.py --base 84fb017 --node M2.12
critical-guard: 84fb017..working tree, 83 changed file(s)
  M  crates/slipfork-engine/tests/critical_engine.rs  [VIOLATION: not covered by any node path]
  A  crates/slipfork-ospp/tests/critical_ospp.rs  [added (always allowed)]
  M  crates/slipfork-vcs/tests/critical_vcs.rs  [authorized by crates/slipfork-vcs/**]
  A  crates/slipfork-vcs/tests/critical_conformance_matrix.rs  [added (always allowed)]
critical-guard: FAILED: 1 unauthorized protected test change(s) in M2.12.   (exit 1)
```

Both M2.12 protected entries are accepted (the new matrix file is an allowed addition, the edited
suite is authorized by `crates/slipfork-vcs/**`); the single violation is another node's file
(`crates/slipfork-engine/tests/critical_engine.rs`, `M2.7`).

`make check` at this moment:

```console
$ make check
… plan-check OK, guard OK (274 files), py-lint OK, py-test OK, ts-test OK, rs-fmt OK …
error[E0507]: cannot move out of `request.headers` which is behind a shared reference
error: unused import: `RoutedWorld`            # crates/slipfork-edge
error[E0603]: function import `hex_str` is private   # crates/slipfork-ospp
make: *** [Makefile:33: rs-lint] Error 101                      # exit 2
```

Individual targets (run because `rs-lint` aborts the chain):

```console
$ make plan-check   → exit 0
$ make guard        → exit 0   (guard: OK (274 file(s) checked))
$ make py-lint      → exit 0
$ make py-test      → exit 0
$ make ts-test      → exit 0
$ make rs-fmt       → exit 0
$ make docs-check   → exit 2   (check_docs_honesty builds `slipfork`, whose slipfork-edge dep is mid-edit)
$ cargo test --workspace --doc → exit 0
```

## Cross-node note (not this node, not touched)

`crates/slipfork-edge/**` and `crates/slipfork-ospp/**` (in-flight `M2.13`/edge work) and
`crates/slipfork-engine/**` (`M2.7`) were being edited concurrently while this ran; their compile
errors are what make `make check`/`docs-check` red right now. `slipfork-vcs` itself compiles and
lints clean (`Checking slipfork-vcs …` with no diagnostics). My delta is exactly
`crates/slipfork-vcs/**` (12 files) plus this node's `plan/` entries.

## Follow-ups

1. **M2.17 wiring:** `remote_helper::serve` needs a `&dyn VcsWorkspace`; the CLI binary must obtain
   one for a live fork (today only `VcsWorkspaceFactory::create_workspace` returns a handle, so the
   daemon has to hand it over — an "attach to existing fork" entry point is still missing by design).
   No new API was invented here; the handler is process- and transport-agnostic.
2. **Repository hygiene requirement:** a fork repository must ignore `.worktrees/` and `.slipfork/`.
   For Git the pathspec excludes hide them; for Jujutsu auto-snapshot only `.gitignore` does. Worth
   documenting in the user-facing docs of `M2.23`/`M2.16` (`slipfork init`/`fork`).
3. **`NODE.md` #4 wording:** replace `<sha>\t<refname>` with `<sha> <refname>` (see erratum above).
4. **Jj host coverage:** the real-jj matrix ran here against a downloaded `jj 0.47.1`; a CI runner
   with `jj` installed should keep that branch green (`jj git init --colocate`, `workspace add
   --revision`, `workspace forget`, `workspace list -T`, `log -T commit_id`, `diff -T` are the only
   commands used — all documented in `jj`'s current CLI reference).
