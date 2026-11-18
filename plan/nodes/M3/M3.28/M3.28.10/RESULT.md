# M3.28.10 — `slipfork fork` with no argument

Base `a364b31` (`release/0.1.0`); the sibling `M3.28.16` commit (`0c9abdd`) landed while this node
was in progress and is included in the verification tree. No commit was made by this node.

## What changed

- `crates/slipfork/src/cli.rs` — `Fork.fork: Option<String>` with `#[arg(value_name = "FORK")]` and
  help `Fork name (default: the current branch)`; `drop` keeps its required `fork: String`. Usage now
  renders `slipfork fork [OPTIONS] [FORK]`. Two lib tests pin the optional slot, the `[FORK]`/no
  `<FORK>` help, and that `drop` still requires a name.
- `crates/slipfork/src/main.rs` — `ForkOptions.fork: Option<String>` forwarded verbatim into
  `ForkRequest`; the client still performs no VCS work (no `git`/`.git` access anywhere in
  `crates/slipfork/src`, checked by acceptance #4).
- `crates/slipfork/src/daemon.rs` — `fork` resolves a `None` name through `M3.28.5`:
  `VcsWorkspaceFactory::current_branch(&RepoRoot)` → `sanitize_fork_name`, then one `snow.yml` read
  of the requesting checkout, the project-name check, and `validate_fork_name` against the real
  `snow.domain_ids()` (never `&[]`). New free function `branch_fork_name` holds the three
  fail-closed answers (inside `.worktrees/<fork>`, detached/bookmark-less checkout, unsanitisable
  branch); the existing-fork line now names `slipfork up --world <world>`.
- `crates/slipfork/src/vcs.rs` — unchanged: it already delegates `current_branch` through the one
  backend rule (acceptance #5 proves the wiring).
- `crates/slipfork/tests/critical_cli.rs` — four new protected tests: branch-derived
  `feat/ux-pass` → `feat-ux-pass` under `GIT_CONFIG_GLOBAL=/dev/null` (world/worktree/`cd`/origin
  block), detached `HEAD` + in-worktree refusals (one line each, no mutation), existing fork naming
  `slipfork up --world acme-app:feat-ux-pass`, and project/`domains[].id` collisions against a real
  `snow.yml` `domains:` block.

Rendering reuses the one `M3.28.8` block (`restart::render(Verb::Fork, …)`); `--json` reuses
`restart::json`, the same one projection `up`/`restart --json` use (key set asserted in
`restart::tests::json_is_the_daemon_summary_plus_ok_and_nothing_else` and in the `restart --json`
CLI test), so no second document or renderer was added. `--base`, `--no-dump | --refresh-dump`,
`--resources local`, and `--host-caches` are untouched.

## Acceptance (all run on the final tree)

```console
$ cargo run -q -p slipfork -- fork --help | grep -qF '[FORK]' && echo optional-fork
optional-fork
$ cargo run -q -p slipfork -- fork --help | sed -n 3,6p
Usage: slipfork fork [OPTIONS] [FORK]

Arguments:
  [FORK]
          Fork name (default: the current branch)
$ cargo test -p slipfork --lib fork
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 89 filtered out
$ cargo test -p slipfork --test critical_cli -- --test-threads=1
test result: ok. 28 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 174.18s
$ ! grep -rn 'commit-tree\|rev-parse' crates/slipfork/src && echo no-git-in-cli
no-git-in-cli
$ grep -rq 'current_branch' crates/slipfork/src/vcs.rs && echo vcs-branch-wired
vcs-branch-wired
$ cargo clippy --workspace --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s)
```

Additional evidence:

```console
$ cargo fmt --all -- --check
FMT-OK
$ cargo test -p slipfork-edge --test critical_edge
test result: ok. 35 passed; 0 failed
$ make ts-test
16 pass / 0 fail (bun test packages/opencode-slipfork/tests)
$ uv run --no-project tools/guard.py
guard: OK (598 file(s) checked)
$ uv run --no-project tools/check_docs_honesty.py
check_docs_honesty: OK (11 doc(s) match real CLI subcommands …)
$ make check            # every stage green except docs-check, below
make check exit=2
uvx --offline zensical build --strict
error: No solution found when resolving tool dependencies
  cause: Because zensical was not found in the cache and you require zensical …
make: *** [Makefile:114: docs-check] Error 1
```

`make check` ran `plan-check`, `guard`, `stub-check`, `py-lint`, `py-test`, `support-check`,
`ts-test`, `rs-fmt`, `rs-lint`, and the whole `rs-test` suite (all `critical_*` suites, including
`critical_cli`'s 28 tests, green) before failing **only** on `docs-check`: this host has no
`zensical` in the offline uv cache and the network is disabled, so the doc build cannot run. The
honesty half of `docs-check` (`tools/check_docs_honesty.py`) passes; no docs changed here.

## Two findings for the lead (not code defects)

1. **`paths` gap — required by this node, not covered by its globs.** `NODE.md` invariant 1 says
   "`ForkRequest.fork` becomes `Option<String>`", but `ForkRequest` lives in
   `crates/slipfork-edge/src/daemon.rs`, and its only other struct-literal site is the protected
   `crates/slipfork-edge/tests/critical_edge.rs`. Changing the wire field to `Option` without a
   sentinel (an empty-string "derive me" hack is exactly the illegal solution this node forbids)
   cannot compile without both. The superseded `fork --help` tokens also live in
   `packages/opencode-slipfork/tests/fixtures/cli-contract.json`, which `make check`'s `ts-test`
   asserts against the real binary. Files touched outside `paths`:

   - `crates/slipfork-edge/src/daemon.rs` (field type + doc, the node's own requirement)
   - `crates/slipfork-edge/tests/critical_edge.rs` (one literal: `fork: Some("feat".to_owned())`;
     assertion content unchanged — nothing weakened or deleted)
   - `packages/opencode-slipfork/tests/fixtures/cli-contract.json` (fork tokens `[FORK]` +
     `Fork name (default: the current branch)`; required by acceptance "never `<FORK>`")

   `critical_guard.py` therefore reports the one protected edit as unauthorized, as designed:

   ```console
   $ uv run --no-project tools/critical_guard.py --base a364b31 --node M3.28.10
   critical-guard: a364b31..working tree, 18 changed file(s)
     M  crates/slipfork-edge/tests/critical_edge.rs  [VIOLATION: not covered by any node path]
     M  crates/slipfork/tests/critical_cli.rs  [authorized by crates/slipfork/tests/critical_cli.rs]
   critical-guard: FAILED: 1 unauthorized protected test change(s) in M3.28.10.
   ```

   Concrete fix (lead-only authority), then re-run the guard:

   ```console
   uv run --no-project tools/plan.py --agent lead edit M3.28.10 --paths \
     "crates/slipfork/src/cli.rs,crates/slipfork/src/main.rs,crates/slipfork/src/vcs.rs,crates/slipfork/src/daemon.rs,crates/slipfork/tests/critical_cli.rs,crates/slipfork-edge/src/daemon.rs,crates/slipfork-edge/tests/critical_edge.rs,packages/opencode-slipfork/tests/fixtures/cli-contract.json"
   ```

   Alternative if widening is refused: re-plan the `ForkRequest`/fixture edits as a sibling node
   under the `M3.28` epic that owns `crates/slipfork-edge/src/**` and
   `packages/opencode-slipfork/**`, and land it before this one.

2. **Message voice.** `§7`'s four bullets cannot show nested code spans, so they write
   ``slipfork fork <name>`` unbracketed; the implemented lines use the CLI's one backtick voice
   (as `§3`/`§4` do): ``slipfork: checkout has no active branch; pass an explicit name with
   `slipfork fork <name>` ``. Wording, order, and named next commands are otherwise §7's verbatim.
