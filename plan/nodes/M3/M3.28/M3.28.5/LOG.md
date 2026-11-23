# Log M3.28.5

## 2026-11-17T16:32 [planner]

created (task) VCS: branch-named fork source, pinned slipfork snapshot identity, and one-line git errors

## 2026-11-17T18:52 [lead]

status ready -> active

## 2026-11-17T18:52 [lead]

attempt 1/3 started

## 2026-11-17T18:52 [lead]

base f291377108292a04dfe5efe0e7627955466b889f

## 2026-11-17T18:55 [lead]

edited fields

## 2026-11-17T18:55 [lead]

lead ruling on the direction challenge: paths widened with crates/slipfork/src/vcs.rs (the router forward is a 7-line delegation, no fallback, no match ladder). Signature stays current_branch(&self, checkout: &Path) — the router resolves RepoRoot itself and passes <vcs_root>, so no spec change. M3.28.10 also gains daemon.rs + vcs.rs for the CLI-side resolution.

## 2026-11-17T18:55 [lead]

attempt 2/3 started

## 2026-11-17T18:55 [lead]

base f291377108292a04dfe5efe0e7627955466b889f

## 2026-11-17T19:03 [lead]

attempt 2 landed on disk but the worker returned no report and RESULT.md is still the attempt-1 text (treat it as stale). LEAD-VERIFIED acceptance evidence on the working tree: GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null cargo test -p slipfork-vcs --test critical_vcs => 25 passed, 0 failed (incl. snapshot_commit_carries_the_pinned_slipfork_identity, sanitised_branch_forks_once_and_a_second_identical_fork_fails_closed); critical_conformance_matrix => 2 passed; --lib => 15 passed; grep user.name=slipfork src/git.rs => identity-pinned; no user.name/user.email under crates/slipfork/src => cli-owns-no-identity; cargo clippy -p slipfork-vcs --all-targets -D warnings => Finished. No new dependency (Cargo.toml only promotes critical_conformance_matrix to its own test target). Workspace-wide clippy/make check deferred until M3.28.3 stops editing crates/slipfork.

## 2026-11-17T19:03 [lead]

status active -> review

## 2026-11-17T19:07 [lead]

edited fields

## 2026-11-17T19:07 [lead]

status review -> active

## 2026-11-17T19:07 [lead]

attempt 3/3 started

## 2026-11-17T19:09 [lead]

quad round 1 rulings: (1) current_branch takes &RepoRoot (reverses my earlier &Path call — the newtype is what makes 'never cwd' structural); (2) kill the second backend ladder in crates/slipfork/src/vcs.rs (hold both adapters, one backend() accessor); (3) move the duplicated expect_create_error helper into tests/support/mod.rs; (4) the label alphabet must not be encoded twice — export it from slipfork-world or delegate; (5) tools/rs_test_list.py docstring still describes the old #[path]-module topology (path added); (6) sanitize_fork_name/current_branch have no production caller until M3.28.10 — kept deliberately (same epic, M3.28.10 acceptance now greps that vcs.rs forwards current_branch), not speculative API.

## 2026-11-17T19:09 [lead]

edited fields

## 2026-11-17T19:09 [lead]

edited fields

## 2026-11-17T19:11 [lead]

attempt 4/4 started

## 2026-11-17T19:12 [lead]

edited fields

## 2026-11-17T19:39 [lead]

status active -> done

## 2026-11-17T19:39 [lead]

attempt 4: jj installed on host (0.36.0) so the Jujutsu family really executes and it exposed two real adapter bugs (jj log --revision->-r, jj diff -T status_char->status); &RepoRoot restored; one backend() ladder; alphabet delegated to slipfork_world::is_valid_dns_label. critical_guard for this node is clean except crates/slipfork-edge/tests/critical_edge.rs, which was already committed under M3.28.3 (dc9bd20) — the guard compares the *unstaged* working tree against base, so a committed sibling change still shows up; re-run at the epic gate on a clean tree.
