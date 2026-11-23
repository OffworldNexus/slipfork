# M2.11 RESULT — VCS ports, `GitWorktreeAdapter`, and the shared conformance matrix

**Status:** done (attempt 3/3; attempt 1 correctly blocked on the Git D/F refname impossibility, now
resolved by `refs/slipfork/<fork>/wip`). Scope: `crates/slipfork-vcs/**` plus the mechanically
required `Cargo.lock` line for the already-declared workspace dev-dependency `tempfile`.

## Attempt-3 gate findings — all fixed

1. **[BLOCKER] `.worktrees` leaked into sibling fork snapshots.** `git add -A -- .
   ':(exclude).slipfork'` staged an existing `.worktrees/<first>` checkout as an embedded-repository
   gitlink (`160000 commit <first HEAD> .worktrees/feat-a`), so a second fork of an otherwise clean
   tree produced `wip != base`. **Fix:** the snapshot argv now carries
   `':(exclude).worktrees'` (`WORKTREES_PATHSPEC_EXCLUDE`). **Regression case added to the shared
   matrix:** create `feat-first`, then `feat-second` from the clean tree; assert
   `rev(wip_second) == base`, `ls-tree -r wip_second` has no `.worktrees[/…]` path, and
   `changes() == WorkspaceChanges::default()`. Proven to kill the bug: with the exclusion
   temporarily removed the matrix fails with
   `left: "37c86d2…" right: "20239f2…"` (“a clean tree stays clean when a sibling checkout already
   exists”), and passes with it restored.
2. **[MAJOR] `jj.rs` stale doc.** Now `@- -> refs/heads/<fork>` and `@ -> refs/slipfork/<fork>/wip`
   (collapsing to `@-`'s commit when clean).
3. **[MAJOR] duplicated teardown.** `forget()` and `rollback()` both delegate to one private
   `teardown(vcs_root, fork, worktree)`: remove checkout (or prune stale metadata), delete
   `refs/slipfork/<fork>/wip`, delete the branch, remove the scratch index. `forget` propagates
   errors; `rollback` discards them (`let _ = teardown(...)`) because an earlier error is already
   being reported.
4. **[MAJOR] shared matrix wasn't importable.** `assert_vcs_workspace_conformance` + `request`,
   `FixedOverride`, `FailingOverride` now live in `tests/support/mod.rs`; `tests/critical_vcs.rs`
   calls the matrix from `git_conformance_matrix_satisfies_shared_suite` (`M2.12` can reuse it with
   `mod support;`). Protected test names/coverage are unchanged (`expect_create_error` stays in
   `critical_vcs.rs`).
5. **[MINOR] duplicated scratch-index path.** One exported
   `slipfork_vcs::git::scratch_index_path(vcs_root, fork)` is used by both the adapter and
   `Fixture::scratch_index`. Unused `fork_branch_ref`/`fork_snapshot_ref` re-exports dropped from
   `lib.rs` (still `pub` in `ports`, used by `git.rs` and `M2.12`).

## Delivered

| File | Content |
|---|---|
| `src/ports.rs` | `VcsError`, `RepoRoot`, `WorkspaceRequest`, `WorkspaceRefs`, `WorkspaceChanges`, `ProjectSlugOverride` + `NoProjectOverride`, `RepoRootResolver`, `VcsWorkspace`, `VcsWorkspaceFactory`, ref-layout helpers, `MAX_WORKSPACE_CHANGES`. Zero subprocess/env access. |
| `src/repo_root.rs` | `GitRepoRootResolver`: bare → toplevel → superproject → git-dir/common-dir probes; injected `Arc<dyn ProjectSlugOverride>`; never derives a slug from `--git-common-dir`/`.git/modules/<id>`. |
| `src/git_command.rs` (private) | One `git` runner: explicit argv, `LC_ALL/LANGUAGE=C`, scrubbed `GIT_DIR`/`GIT_WORK_TREE`/`GIT_INDEX_FILE`/`GIT_COMMON_DIR`/object-dir vars; `NotFound` → `ToolMissing`. |
| `src/git.rs` | `GitWorktreeAdapter` + `GitWorkspace`: `.worktrees/<fork>`, scratch `GIT_INDEX_FILE=<vcs_root>/.slipfork/git/index-<fork>`, `':(exclude).slipfork'` + `':(exclude).worktrees'`, clean-tree `wip == base`, dirty `commit-tree`, `read-tree -u --reset <wip>` + `read-tree <base>` view, shared idempotent `teardown`, fail-closed duplicate refusal, exported `scratch_index_path`. |
| `src/jj.rs` | Correct ref projection doc for `M2.12`. |
| `tests/support/mod.rs` | Disk-backed (`$HOME/.cache/slipfork-critical-tests`, never `/tmp`) fixtures + `assert_vcs_workspace_conformance` matrix (importable by `M2.12`). |
| `tests/critical_vcs.rs` (protected) | 11 tests: shared matrix, dirty M/D/?? view, untouched live index, fail-closed fork collision/escape/non-repo/bare/submodule/separate-git-dir/override-error/missing-`git` re-exec, linked-worktree resolution + fork-from-linked, truncation cap. |
| `Cargo.toml` | `[dev-dependencies] tempfile.workspace = true` (already in the workspace). |

## `# Acceptance` (exact, attempt 3)

```console
$ cargo test -p slipfork-vcs --all-targets                                        # exit 0
src/lib.rs: 2 passed; tests/critical_vcs.rs: 11 passed; 0 failed
$ cargo test -p slipfork-vcs --test critical_vcs -- --nocapture git_conformance    # exit 0
1 passed; 10 filtered out   (includes the two-sibling-fork regression)
$ cargo test -p slipfork-vcs --test critical_vcs -- --nocapture dirty_fork         # exit 0
2 passed; 9 filtered out
$ cargo test -p slipfork-vcs --test critical_vcs -- --nocapture index_untouched    # exit 0
1 passed; 10 filtered out
$ cargo test -p slipfork-vcs --test critical_vcs -- --nocapture fail_closed        # exit 0
4 passed; 7 filtered out
$ rg -q "std::process" crates/slipfork-vcs/src/ports.rs; echo $?                  # 1
$ rg -q "slipfork-dag" crates/slipfork-vcs/Cargo.toml; echo $?                    # 1
$ cargo clippy -p slipfork-vcs --all-targets -- -D warnings                       # exit 0
$ cargo fmt --all -- --check                                                      # exit 0
$ RUSTDOCFLAGS="-D warnings" cargo doc -p slipfork-vcs --no-deps                  # exit 0
$ cargo test -p slipfork-vcs --doc                                               # exit 0 (0 tests)
$ uv run --no-project tools/critical_guard.py --base 1b4cf79 --node M2.11 --report  # exit 0
  A crates/slipfork-vcs/tests/critical_vcs.rs  [added (always allowed)]
$ uv run --no-project tools/guard.py                                              # guard: OK (261 files)
$ make check                                                                      # exit 0
  plan OK · guard: OK · ruff clean · pytest 6 passed · bun 3 pass · cargo fmt ·
  cargo clippy --workspace -D warnings · workspace tests · rustdoc · zensical --strict ·
  check_docs_honesty: OK
```

## Verified invariants (adversarial)

- Live `.git/index` **inode + mtime unchanged** across a dirty fork and across `forget()`: the
  snapshot only opens `.slipfork/git/index-<fork>`; the checkout uses the linked worktree's index.
- Dirty fork reproduces exactly `M tracked.txt`, `D deleted.txt`, `?? untracked.txt` after
  `read-tree -u --reset <wip>` then `read-tree <base>`; `wip^ == base`; both refs coexist as
  distinct commits.
- Clean fork: `refs/slipfork/<fork>/wip == refs/heads/<fork> == base`; `changes() == default`.
- **Two sibling forks on a clean tree:** `wip == base`, no `.worktrees` path in `ls-tree -r`, empty
  `changes()` (the attempt-3 BLOCKER regression).
- Second create for the same fork / pre-existing branch / wip ref / worktree path →
  `VcsError::WorkspaceExists`; `../escape` → `WorldError::InvalidForkName`.
- `forget()` twice succeeds (shared `teardown`); refs, checkout, scratch index gone.
- Non-repository → `NotARepository { start }`; bare → `UnsupportedTopology` + `git clone` hint;
  linked `.worktrees/<fork>` → `vcs_root` = main root (worktree-local `slipfork.toml` ignored);
  submodule / `--separate-git-dir` (including a store named `.git`) → `UnsupportedTopology` unless
  the injected override names them; override errors propagate; missing `git` (re-exec child with
  empty `PATH`) → `ToolMissing`.
- Forking from a linked worktree snapshots **that** worktree's dirty files while refs stay in the
  main checkout.
- `.slipfork/` and `.worktrees/` never leak into a snapshot; `.worktrees/<fork>` is inside the
  repository, never `/tmp`.
- `changes()` caps at `MAX_WORKSPACE_CHANGES` (500) with `truncated = true` (501 untracked files).

## Decisions / flags for review

1. **Snapshot before `worktree add`** (order swap vs NODE §2 wording): creating the fresh
   `.worktrees/<fork>` first would make it an untracked path inside its own snapshot. Commands and
   invariants are unchanged; this is the order the attempt-1 live probe validated.
2. **Scratch index is repo-local** (`<vcs_root>/.slipfork/git/index-<fork>`, NODE §2 literal): a
   daemon-global state dir would collide `index-<fork>` across repositories sharing a fork name.
3. **`MAX_WORKSPACE_CHANGES = 500`** declared once at the port so both adapters truncate
   identically; `truncated` has no meaning without a bound.
4. **`base: None` → `HEAD`** of `repo.worktree_root` (the requesting checkout).
5. **Fork names reuse `slipfork_world::validate_fork_name(fork, &[])`** (single lowercase DNS label,
   no `test-*`): the name becomes a path component and a refname, so the world layer's canonical
   validator is the only honest guard.
6. **Submodule / `--separate-git-dir`**: basename derivation is always refused; an explicit injected
   `ProjectSlugOverride` is the sole rescue, exactly as `ADR-0005` §3's failure hint states (also
   preserves M1's protected `critical_slug.rs` rescue contract).
7. **Create is refused, `forget` is idempotent** (NODE §4 + adversarial assertion).
8. `Cargo.lock` gains one line (`slipfork-vcs` → `tempfile`).

## Followups

- `M2.16`: `slipforkd` must implement `ProjectSlugOverride` over `slipfork_dag::SlipforkConfig` and
  delete M1's now-duplicated private `git_root`/`project_slug_for` probing in `crates/slipfork`.
- `M2.12`: fill `src/jj.rs` + `src/remote_helper.rs`; `mod support;` + call
  `assert_vcs_workspace_conformance` for the same matrix.
- Sweep published docs that still say `refs/heads/<fork>/wip` (`M2.19`/`M2.23`).
