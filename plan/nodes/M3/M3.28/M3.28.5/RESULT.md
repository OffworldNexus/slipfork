# M3.28.5 — attempt 4/4 result

```
STATUS: done
```

Base `f291377`. Paths: `crates/slipfork-vcs/{src,tests}/**`, `crates/slipfork/src/vcs.rs`,
`crates/slipfork-world/src/**`, `Makefile`, `tools/rs_test_list.py`.

## What shipped

1. **Pinned snapshot identity** (`src/tool_command.rs`, `src/git.rs`, `src/jj.rs`): one author pair
   (`slipfork` / `slipfork@localhost`) rendered as tool *global* options before the subcommand —
   `git -c user.name=slipfork -c user.email=slipfork@localhost commit-tree …` for the snapshot
   commit, `jj --config user.name=… --config user.email=… <subcommand>` for every
   repository-touching Jujutsu call. Nothing reads, writes or requires the developer's config; the
   `jj --version` probe deliberately carries none.
2. **`current_branch(&self, repo: &RepoRoot)`** (`src/ports.rs`, both adapters, `crates/slipfork/src/vcs.rs`):
   backend from `repo.vcs_root`, probe in `repo.worktree_root`, so "never the cwd, never a
   subdirectory" is structural. Git: `symbolic-ref --short --quiet HEAD`, exit 1 **with empty
   stderr** is detached (`Ok(None)`); any other exit 1 is a real failure. Jujutsu:
   `--ignore-working-copy log --no-graph -r @ -T '<local_bookmarks>'`, empty → `None`, several
   bookmarks → fail closed.
3. **One backend ladder** (`crates/slipfork/src/vcs.rs`): both adapters are fields and every port
   method goes through `fn backend(&self, root: &RepoRoot) -> &dyn VcsWorkspaceFactory`; a third
   backend extends one rule, not one ladder per method.
4. **One alphabet**: `slipfork_world::is_valid_dns_label` is now public and owns the label grammar
   (`[a-z0-9-]`, 1..=63, no edge `-`). `sanitize_fork_name` maps a branch (`/` -> `-`, ASCII
   lowercase, repeated `-` collapsed, edge `-` stripped), asks that predicate per character
   (`a<byte>a` probe) and again for the finished candidate, and rejects empty; it never truncates.
   `test-*`, the project name and `domains[].id` collisions stay `validate_fork_name`'s job in
   `slipforkd`, which reports them as collisions.
5. **One-line VCS errors**: `CommandFailed` renders `<tool> <subcommand> failed (exit <n>): <last
   stderr line>`; full argv/cwd survives only in spawn and unexpected-output diagnostics.
6. **Tests**: the matrix is its own protected `[[test]]` target and is listed in `Makefile`'s
   `rs-test`; `expect_create_error` moved to `tests/support`; the duplicated git identity assertion
   and the duplicated `Some("main")` assertion are gone (the matrix owns the branch case, and the
   identity is pinned once in `tool_command.rs`); a new `vcs_failures_render_as_one_line` test backs
   the header claim; the Jujutsu skip arm now prints a loud `eprintln!` naming the missing tool.

## Jujutsu half now runs for real (jj 0.36.0 on this host)

Two latent adapter bugs surfaced the moment `jj` existed and are fixed:

- `jj log --revision` is not a flag in 0.36 (it is `-r`/`--revisions`), so `commit_id` and the branch
  probe failed with exit 2. Both now use `-r`.
- `jj diff -T 'status_char …'` does not exist in 0.36 (`Keyword status_char doesn't exist`), so
  `changes()` had never worked on this version. The template now uses Jujutsu's own word `status`
  (`added`/`modified`/`removed`/`copied`/`renamed`) and the parser maps those words.

The matrix's Jujutsu family executes (≈1.8 s, no `skipped:` line) and asserts both halves of the
probe: the colocated fixture's bookmark-less `@` answers `None`, and a fixture carrying
`main` on `@` answers `Some("main")` — so an implementation that always returned `None` fails.
(`jj bookmark set main -r @` is used because colocation already imports Git's `main` bookmark on the
parent commit, so `create main` would refuse; the booked-working-copy state is identical.)

## Acceptance (exact)

```console
$ GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null cargo test -p slipfork-vcs --test critical_vcs
test result: ok. 26 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
$ cargo test -p slipfork-vcs --test critical_conformance_matrix
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
$ cargo test -p slipfork-vcs --lib
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
$ grep -q 'user\.name=slipfork' crates/slipfork-vcs/src/git.rs && echo identity-pinned
identity-pinned
$ ! grep -rn 'user\.name\|user\.email' crates/slipfork/src && echo cli-owns-no-identity
cli-owns-no-identity
$ cargo clippy --workspace --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s)
```

Before this node the pinned-env run was `FAILED. 18 passed; 6 failed` with
`git commit-tree … status 128, stderr: "Author identity unknown"`; it is green, which is the proof
that the identity is pinned and no user `git config` is required.

Extra gates:

```console
$ cargo fmt --all -- --check                                  # clean
$ cargo clippy -p slipfork-vcs --all-targets -- -D warnings    # Finished
$ RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps   # Finished
$ make rs-test-list                                            # rc=0
$ uv run --no-project tools/guard.py                           # guard: OK (548 file(s) checked)
$ uv run --no-project tools/plan.py validate                   # plan OK
$ uv run --no-project --with pytest pytest -q tools/tests      # 126 passed
$ cargo test -p slipfork --lib vcs                             # 2 passed (routing test)
$ uv run --no-project tools/critical_guard.py --base f291377 --node M3.28.5
  M crates/slipfork-vcs/tests/critical_conformance_matrix.rs  [authorized by crates/slipfork-vcs/tests/**]
  M crates/slipfork-vcs/tests/critical_vcs.rs                 [authorized by crates/slipfork-vcs/tests/**]
  M crates/slipfork-edge/tests/critical_edge.rs               [VIOLATION: not covered by any node path]
```

The single critical-guard violation is `crates/slipfork-edge/tests/critical_edge.rs` (M3.28.3);
`--node M3.28.3` authorizes it, and neither of my protected files is ever unauthorized.

## Notes

- `make check` cannot complete on this host: it stops at `ts-test` with
  `make: bun: No such file or directory` (`bun` is absent). Every other target it runs —
  `plan-check`, `guard`, `py-lint`, `py-test`, `support-check`, `rs-fmt`, `rs-lint`,
  `rs-test-list` — was run separately and is green.
- `rs_test_list.py --world-booting "<one quoted string>"` fails by tool design (`nargs="*"` treats
  the quoted blob as one pair); the unquoted form `make rs-test-list` runs is `rc=0`.
- One whitespace-only line in `crates/slipfork/src/daemon.rs` (`world_id(project, target)`
  re-indented) came from `cargo fmt -p slipfork` during attempt 2; it is what rustfmt requires and
  carries no semantic change.
