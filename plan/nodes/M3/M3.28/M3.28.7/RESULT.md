# M3.28.7 RESULT — Bare `slipfork` = Status Dashboard, `slipfork status`, and a Pure `--json` Document

- **Status:** `done`
- **Node:** `M3.28.7` (`release/0.1.0`, base `19658f8`)
- **Scope (only):** `crates/slipfork/src/{cli.rs,status.rs,lib.rs,main.rs}`, `crates/slipfork/tests/critical_cli.rs`
- **Authority:** `docs/design/dx-0.1.0-ux.md` §1/§3/§5/§10 and `ARCHITECTURE.md` §3.4 (`DaemonStatus`, `WorldSummary`, `WorldLineage`)

## What changed

1. **One command, two spellings (`cli.rs` + `main.rs`):** `Cli.command: Option<Commands>` plus a new
   `Commands::Status`. `main.rs` has exactly one arm — `None | Some(Commands::Status) => status(...)` —
   so bare `slipfork` and `slipfork status` run the same function. No alias subcommand, no second
   help mode: `--help` still renders the `Commands:` block (`status` listed first) and `slipfork help`
   works. The `Cli` doc comment stays a single line so `--help`'s tagline is unchanged.
2. **One renderer (`status.rs`, new):** pure functions over the daemon payload. `render` emits
   `slipforkd <pid>, ingress <addrs>` then per project the canonical world followed by its
   indented forks (grouped by the daemon's `WorldSummary.parent`, never by splitting the id; payload
   order preserved; the canonical row always leads). Each row carries state, origin URL, and
   worktree; its `lineage` sub-line is the §8.2 renderer (`render_lineage`, private until `.8`/`.9`
   consume it), and its `next` sub-line comes from the daemon's one `world_next_commands` table,
   filtered to the verbs this binary implements (`cli::implemented_commands()`) — bare for the
   canonical world, with `--world <world>` for a fork (the design's §3 transcript). Zero worlds
   renders the one line `no worlds — run \`slipfork up\` inside a project checkout`. `status` and
   `list` share the one row formatter `status::world_row` plus `status::world_column`.
3. **`--json` (`status::json`):** `{"ok": true, "pid", "ingress": {"addrs", "port"}, "forward_port",
   "worlds": [{"world", "parent", "state", "origin", "worktree", "lineage"}]}` — stdout only, no
   human text, no ANSI, every key always present with `null` when unknown. `origin` is
   `origin.url()`; `lineage` is the daemon's own `WorldLineage` serialized as-is (`rootfs`,
   `services`, `seed`, `base`), never a hand-built subset. No process rows (those are `ps --json`).
4. **No auto-spawn:** `status` uses the existing `connect` (never `connect_or_spawn`), so with
   nothing answering on `<state_dir>/run` the binary prints exactly
   `slipfork: no slipforkd on <path> — run \`slipfork up\`` to stderr, exits `1`, and creates
   nothing. The protected `read_verbs_never_spawn_a_daemon_and_fail_closed_in_one_line` loop gained
   the two new spellings (`[]` and `["status"]`), never weakened.
5. **Loud non-`:80` ingress:** `status::ingress_hint(port)` returns `None` at `INGRESS_PORT` and
   otherwise exactly one actionable line naming the installer:
   `slipfork: ingress is :<port>, not :80 — run \`sudo slipfork daemon install\` once for port-less origins`.
   `main.rs` writes it to stderr in human mode only; under `--json` stderr stays empty (asserted).
6. **Daemon-only facts:** every field comes from `DaemonControl::status`; the CLI reads no process
   table, sysfs tree, environment, or log directory for `status` and has no `--runtime-dir`.

## Protected tests extended (never weakened)

`crates/slipfork/tests/critical_cli.rs` (+2 tests, 1 extended loop):

- `bare_slipfork_and_status_render_one_dashboard_and_json_stays_pure` — help/`Commands:` intact,
  byte-identical stdout for bare vs `status`, canonical-then-indented-fork tree, the exact canonical
  and `--world`-qualified `next` lines, byte-identical bare `--json` vs `status --json`, the exact
  world key set (`["lineage","origin","parent","state","worktree","world"]`), `parent: null` for the
  canonical world, `forward_port: null` without a forward listener, and exactly one stderr ingress
  line (empty under `--json`).
- `status_with_zero_worlds_prints_one_line_plus_up` — the exact two-line empty-fleet dashboard plus
  `"worlds": []`.
- `read_verbs_never_spawn_a_daemon_and_fail_closed_in_one_line` — now also asserts the bare and
  `status` spellings.

Renderer unit tests (`cargo test -p slipfork --lib status`, 6 passing): canonical + two forks
(indentation and order), zero worlds, `:80` vs `:40645` origin + hint, lineage digests with `-` for
an unknown fact and no invented duration, the `--json` document, and the clap parse contract.

## Verified `# Acceptance`

```console
$ cargo run -q -p slipfork -- --help | grep -q '^  status' && echo status-listed
status-listed
$ cargo run -q -p slipfork -- --help | grep -q '^Commands:' && echo help-intact
help-intact
$ cargo test -p slipfork --lib status
running 6 tests
test status::tests::status_lineage_prints_short_digests_and_a_dash_for_an_unknown_fact ... ok
test status::tests::status_renders_zero_worlds_as_one_line_plus_the_up_command ... ok
test status::tests::status_renders_port_less_origins_at_80_and_one_hint_on_any_other_port ... ok
test status::tests::status_renders_the_canonical_world_then_its_indented_forks ... ok
test status::tests::status_json_carries_every_key_and_no_human_text ... ok
test cli::tests::bare_invocation_and_status_share_one_command ... ok

test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 61 filtered out; finished in 0.00s
$ cargo test -p slipfork --test critical_cli -- --test-threads=1
running 21 tests
...
test result: ok. 21 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 71.35s
$ ! grep -rn '/proc\|/sys/' crates/slipfork/src && echo no-host-proc
no-host-proc
$ cargo clippy --workspace --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.67s
```

Manual no-daemon proof on this host (no daemon anywhere, fresh state dir):

```console
$ ./target/debug/slipfork --state-dir ~/.cache/slipfork-scratch/manual-state
slipfork: no slipforkd on /home/remy/.cache/slipfork-scratch/manual-state/run/daemon.sock — run `slipfork up`
$ echo $?
1
$ ./target/debug/slipfork --state-dir ~/.cache/slipfork-scratch/manual-state status
slipfork: no slipforkd on /home/remy/.cache/slipfork-scratch/manual-state/run/daemon.sock — run `slipfork up`
$ ls ~/.cache/slipfork-scratch/manual-state
ls: cannot access '.../manual-state': No such file or directory
```

(A real `slipforkd` cannot boot a world on this non-Btrfs host, so the live dashboard assertion runs
through the harness' production in-process daemon graph in `critical_cli`.)

## Gate outputs

```console
$ cargo fmt --all -- --check
(clean)
$ uv run --no-project tools/guard.py
guard: OK (571 file(s) checked)
$ uv run --no-project tools/critical_guard.py --base 19658f8 --node M3.28.7
critical-guard: 19658f8..working tree, 26 changed file(s)
  M  crates/slipfork/tests/critical_cli.rs  [authorized by crates/slipfork/tests/critical_cli.rs]
critical-guard: OK: authorized by M3.28.7
$ uv run --no-project tools/plan.py validate
plan OK
$ uvx --offline ruff check tools poc && uvx --offline ruff format --check tools poc --exclude tools/do_vm.py
All checks passed!
12 files already formatted
$ uv run --no-project --with pytest pytest -q tools/tests
4 failed, 122 passed in 10.30s          # all in tools/tests/test_do_vm.py — see below
$ uv run --no-project tools/check_support.py --check
check_support: OK (10 integration(s); all evidence pointers verified and docs/support-matrix.md in sync)
$ bun test packages/opencode-slipfork/tests
 16 pass, 0 fail
$ RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 9.89s
$ make rs-test
cargo test --workspace --lib --bins        # 67 lib tests, ok
cargo test --workspace --doc               # ok
... 19 integration/critical suites, every one `--test-threads=1` world-booting suite ...
44 x "test result: ok", 0 x "test result: FAILED"; make exit=0
$ uvx --offline zensical build --strict
Build started / No issues found / Build finished in 0.25s
$ uv run --no-project tools/check_docs_honesty.py
check_docs_honesty: OK (10 published doc(s) match real CLI subcommands)
```

## `make check` — one unrelated failure, outside this node's paths

`make check` stops at `py-lint`: `uvx --offline ruff format --check tools poc` reports
`tools/do_vm.py` would be reformatted. `tools/do_vm.py` is **modified in the working tree by another
concurrent node** (`M3.28.12`, +315/−69) and is explicitly outside this node's `paths`, so it was not
touched.

```console
$ git show HEAD:tools/do_vm.py > /tmp/opencode/base_do_vm.py && uvx --offline ruff format --check /tmp/opencode/base_do_vm.py
1 file already formatted
```

The four `tools/tests/test_do_vm.py` failures come from that same uncommitted edit. A clean worktree
at the node base (`git worktree add --detach … 19658f8`) runs that file with
`1 failed, 59 passed` (the single failure is the CLI-rendered AppArmor golden, which needs built
binaries in that scratch checkout), so the other three failures are the concurrent node's, not this
node's. Every other `make check` target passes as listed above.

## Attempt 2 — review fixes (1 BLOCKER + 1 MAJOR + 2 MINOR)

1. **[BLOCKER] `next` hints can no longer name a missing verb.** `cli::implemented_commands()`
   (`BTreeSet<String>`) is derived from the `clap` table itself — the same table `--help` renders
   and `tools/check_docs_honesty.py` parses — and `status::next_hint` filters
   `world_next_commands(state)` through it before rendering. Today `running` therefore renders
   `slipfork db | slipfork logs | slipfork down`; when `M3.28.8`/`M3.28.9` add `restart`/`open` the
   hints become self-enabling with no renderer or test edit (the tests derive the expectation).
   Regression test `status_next_hints_only_name_verbs_the_binary_implements` asserts the invariant
   for all three states against the command set, not a literal list, and the integration test
   re-checks the same invariant on the real binary's stdout.
2. **[MAJOR] One world-row formatter.** `status::world_row(world, indent, world_column)` is the
   single row formatter (column width + indent as parameters); `status::render` and `list` both use
   it, and `list`'s header uses `status::world_column(&worlds)` instead of a literal `16`, so a
   future `NEXT` column cannot drift between the two verbs.
3. **[MINOR] `render_lineage` is private.** It is `fn`, not `pub fn`, until `ps`/`up`/`fork`
   (`M3.28.8`/`.9`) actually consume it. `world_state_label` went private too (only `world_row`
   uses it) — it stays in `status.rs`, never moved to a sibling crate.
4. **[MINOR] Tests no longer re-type rendered lines.** `critical_cli` derives the expected stderr
   from `slipfork::status::ingress_hint(port)`, finds the `lineage`/`next` label column from the
   rendered row, and builds the fork expectation from the canonical hint (`… --world <id>` per row)
   instead of 18-space literals; each exact literal now lives once, in `status.rs`'s own tests.

`list`'s human table now shares the dashboard's alignment (widest world id, minimum 16).

Verified after the fixes:

```console
$ cargo test -p slipfork --lib status
running 7 tests
test status::tests::status_next_hints_only_name_verbs_the_binary_implements ... ok
test status::tests::status_lineage_prints_short_digests_and_a_dash_for_an_unknown_fact ... ok
test status::tests::status_renders_zero_worlds_as_one_line_plus_the_up_command ... ok
test status::tests::status_renders_port_less_origins_at_80_and_one_hint_on_any_other_port ... ok
test status::tests::status_renders_the_canonical_world_then_its_indented_forks ... ok
test status::tests::status_json_carries_every_key_and_no_human_text ... ok
test cli::tests::bare_invocation_and_status_share_one_command ... ok

test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 67 filtered out; finished in 0.01s
$ cargo test -p slipfork --test critical_cli -- --test-threads=1
running 21 tests
...
test result: ok. 21 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 101.71s
$ cargo fmt --all -- --check
(clean)
$ cargo clippy --workspace --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.29s
$ cargo run -q -p slipfork -- --help | grep -q '^  status' && echo status-listed
status-listed
$ cargo run -q -p slipfork -- --help | grep -q '^Commands:' && echo help-intact
help-intact
$ uv run --no-project tools/guard.py
guard: OK (590 file(s) checked)
$ uv run --no-project tools/plan.py validate
plan OK
$ uv run --no-project tools/check_docs_honesty.py
check_docs_honesty: OK (11 doc(s) match real CLI subcommands: published pages plus support/demo/README.md)
```

### Sibling working-tree noise observed during attempt 2

Other nodes are committing and editing in this shared tree while this node runs (`HEAD` moved from
`19658f8` to `9e392f6`, and `crates/slipfork/src/{main.rs,daemon_unit_install.rs}`,
`crates/slipfork-engine/src/linux_cow.rs`, `crates/slipfork-edge/tests/critical_edge.rs`,
`crates/slipfork/tests/critical_slug.rs` carry their uncommitted/committed work). Two repo-wide
gates therefore report sibling changes, not M3.28.7 ones:

```console
$ ! grep -rn '/proc\|/sys/' crates/slipfork/src && echo no-host-proc
crates/slipfork/src/daemon_unit_install.rs:20: ... `/proc/<pid>/exe` ...   # sibling edit (base: 0 hits)
$ ! grep -rn '/proc\|/sys/' crates/slipfork/src/{status,cli,lib,main}.rs && echo no-host-proc
no-host-proc                                  # every file this node owns is clean
$ uv run --no-project tools/critical_guard.py --base 19658f8 --node M3.28.7
critical-guard: 19658f8..working tree, 69 changed file(s)
  M  crates/slipfork-edge/tests/critical_edge.rs  [VIOLATION: not covered by any node path]
  M  crates/slipfork/tests/critical_cli.rs  [authorized by crates/slipfork/tests/critical_cli.rs]
  M  crates/slipfork/tests/critical_slug.rs  [VIOLATION: not covered by any node path]
critical-guard: FAILED: 2 unauthorized protected test change(s) in M3.28.7.
```

At attempt 1 (26 changed files, HEAD still `19658f8`) the same run was
`critical-guard: OK: authorized by M3.28.7`; the two violations belong to sibling nodes' protected
tests, and this node's only protected change is `critical_cli.rs`, which is authorized.

## Iron-law notes

- **Zero invented fields:** the dashboard and the JSON document print only what `DaemonStatus`
  carries; an absent fact is `null` (JSON) or `-` (human), never `0`/`""`/a guessed duration.
- **Zero fallback hiding a missing daemon:** `status` uses `connect` (fail closed, one line, exit 1);
  auto-spawn stays reserved for `up`/`fork`.
- **One renderer:** `status.rs` owns the dashboard, the §8.2 lineage line, the `next` hint, and the
  single `world_row`/`world_column` formatter that `list` reuses; `list` no longer keeps its own
  format string or a literal column width.
- **IoC/DI:** the renderer takes the typed `DaemonStatus` by reference and performs no I/O; only
  `main.rs` (composition root) opens the socket and writes streams.
