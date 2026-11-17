# M3.26 — `slipfork fork` fails closed when `.gitignore` ignores `.worktrees/` / `.slipfork/`

**Status:** done. Branch `release/0.1.0`, base `775776f`.

## The bug

`GitWorktreeAdapter::synthesize_snapshot` staged the requesting worktree with an explicitly named
exclusion pathspec:

```
git add -A -- . ':(exclude).slipfork' ':(exclude).worktrees'
```

Git refuses to stage a pathspec that its own ignore rules match, even when that pathspec is only an
exclusion. Any repository that follows the recommended git-worktrees setup (`.worktrees/` in
`.gitignore`) therefore failed its very first `slipfork fork`:

```
The following paths are ignored by one of your .gitignore files:
.slipfork
.worktrees
hint: Use -f if you really want to add them.
```

(`fatal: Pathspec '.worktrees' is in .gitignore` on the Git of the live VM — same refusal, older
wording.) The M3.19 demo only passed because it had no `.gitignore`.

## Mechanism (one canonical path, no fallback)

`stage_snapshot_index` now stages with a bare `git add -A -- .`, so Git's own ignore rules decide
what enters the scratch index, and then drops Slipfork's two owned directories from that same
scratch index with `git rm -r -f --cached --ignore-unmatch -q -- .slipfork .worktrees`. The
invariant is unchanged: a sibling `.worktrees/<other-fork>` can never become an embedded-repository
gitlink (it is staged as `160000` and then removed) and `.slipfork` scratch state can never enter
the snapshot (it is staged and then removed, or skipped outright when ignored).
`--cached` never touches the working tree, `--ignore-unmatch` makes the drop a no-op when nothing
was staged there (the common ignored case), and `-f` is required because the scratch index file
itself lives under `.slipfork/git/` and its bytes change while it is staged. The user's
`.gitignore` and `.git/info/exclude` are never read or written. The two now-dead pathspec constants
(`SLIPFORK_PATHSPEC_EXCLUDE`, `WORKTREES_PATHSPEC_EXCLUDE`) are deleted and the comments describe
the real mechanism. `jj.rs` is untouched: Jujutsu snapshots through its own ignore rules and never
named a pathspec.

## Test that would have caught it

`crates/slipfork-vcs/tests/critical_vcs.rs::fork_succeeds_when_gitignore_ignores_the_state_directories`:
a fixture that commits `.gitignore` = `.worktrees/` + `.slipfork/`, has a pre-existing ignored
sibling checkout `.worktrees/feat-sibling`, and a dirty tree (M / D / ??). It asserts the first fork
succeeds, the snapshot is exactly `.gitignore`, `src/lib.txt`, `tracked.txt`, `untracked.txt` (so
`.worktrees/` and `.slipfork/` are absent and the dirty view is intact), and that a **second** fork
of the same repository still succeeds with the same snapshot.

Proof it fails on the old code (argv temporarily reverted to the pathspec form, then restored):

```
$ cargo test -p slipfork-vcs --test critical_vcs fork_succeeds_when_gitignore_ignores_the_state_directories
test fork_succeeds_when_gitignore_ignores_the_state_directories ... FAILED
panicked at ...: a repository that ignores .worktrees/ and .slipfork/ must still fork:
CommandFailed { command: "git add -A -- . :(exclude).slipfork :(exclude).worktrees (in …/acme-ignored-state)",
  status: "1", stderr: "The following paths are ignored by one of your .gitignore files:\n.slipfork\n.worktrees\n…" }
```

The non-ignored case is covered by the shared conformance matrix (`feat-first` + `feat-second` on a
fixture with no `.gitignore`), which is now stronger: it also asserts the dirty snapshot contains no
`.slipfork` path and that the sibling snapshot contains no `160000` gitlink entry. That matrix runs
for both the Git and the Jujutsu adapter.

## Acceptance

| Command | Exit |
|---|---|
| `cargo test -p slipfork-vcs --all-targets` | 0 (24 `critical_vcs` incl. the new test + jj matrix, 7 lib unit) |
| `cargo test -p slipfork --test critical_cli -- --test-threads=1` | 0 (14 passed) |
| `cargo test -p slipfork --test critical_vcs_remote -- --test-threads=1` | 0 (1 passed) |
| `cargo test -p slipfork --test critical_cli fork` (NODE.md) | 0 (1 passed) |
| `cargo test --workspace --lib` (NODE.md) | 0 (all suites ok) |
| `cargo clippy --workspace --all-targets -- -D warnings` | 0 |
| `cargo fmt --all -- --check` | 0 |
| `uv run --no-project tools/guard.py` | 0 (`guard: OK (500 file(s) checked)`) |
| `uv run --no-project tools/plan.py validate` | 0 (`plan OK`) |
| `uv run --no-project tools/critical_guard.py --base 775776f --node M3.26` | 0 (both critical files authorized by `crates/slipfork-vcs/tests/**`) |
| `make check` | 0 |

## Residual

- When the two directories are **not** ignored, `git add -A -- .` hashes `.slipfork/git/index-<fork>`
  (and any sibling checkout's gitlink) before the drop, so unreachable loose objects are left behind
  until the next `git gc`. Harmless, and the tree never contains them.
- A repository that *tracks* a real path named `.worktrees` or `.slipfork` would see it dropped from
  fork snapshots. That is byte-identical to the previous pathspec behaviour: both names are
  Slipfork's own by contract (`WORKTREES_DIRECTORY`, `STATE_DIRECTORY`).
- `git add` may emit `warning: adding embedded git repository: .worktrees/<other>` on stderr in the
  non-ignored case; stderr is captured and only surfaced on a non-zero exit, so nothing reaches the
  user.
