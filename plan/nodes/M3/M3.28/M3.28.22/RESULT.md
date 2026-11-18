# RESULT M3.28.22 — quiet world entry (entry argv starts inside the world)

Base `97efc4a` on `release/0.1.0`. No commit (lead owns staging). Attempt 2 completes the two
one-line items attempt 1 escalated (see "Attempt 2" below).

## Attempt 2 — the two escalated one-line items

Both files landed in this node's `paths` (M3.28.11 released `main.rs`), so attempt 1's two blockers
are closed without weakening anything:

1. **Shared-matrix double complies** (`crates/slipfork-edge/tests/critical_edge.rs`):
   `StubWorldEngine::entry_command` now returns `cwd: PathBuf::from("/")` (and takes `_cwd`), so the
   conformance pin stays as strict as attempt 1 wrote it. No assertion was relaxed.
2. **One orientation line** (`crates/slipfork/src/main.rs`, `fn shell`): after the daemon resolves
   the entry target and immediately before `spawn_entry`, `if !json { eprintln!("world rootfs is the
   app stage (no host tools; use slipfork ps / logs / open)") }`. It lives only in `fn shell`, so
   `exec`, `db`, `redis`, and `hook run` never print it; `--json` never mixes it into the document.
   Nothing wraps the user's shell and nothing is printed by `slipforkd` (its stderr is not the user's
   terminal).
3. **PTY pin** (`critical_cli::interactive_shell_prints_one_orientation_line_and_json_never_does`):
   `require_tty` is a real `isatty(2)` probe, so the test drives the real binary on a real PTY via
   `script(1)` (`cli_on_a_pty` helper), sends `exit\n`, and asserts the line appears **exactly once**,
   that `--json shell` never contains it, and that `exec` stays silent. `docs/` quote no `shell`
   transcript, so no published page needed changing.

## Root cause (verified against util-linux 2.41.5 `sys-utils/nsenter.c`)

`nsenter --wd=<dir>` (`-w`) `open(2)`s `<dir>` **in the caller's namespace** while parsing options and
only `fchdir(2)`s after the `setns(2)` loop. The world's `pivot_root` leaves that dentry detached, so
the entered process inherits an unreachable cwd and `dash` reports a `getcwd` failure at startup —
before its own `cd`. `--wdns=<dir>` (`-W`) opens the path **after** the namespace switch, i.e. inside
the world. That is the whole bug; `M3.28.11` attempt 1 had already measured `--wd=/` and the
`sh -c 'cd ... && exec'` wrapper, neither of which removes the detached cwd.

## Before → after argv

Before (`97efc4a`, `BtrfsEngine::entry_command`):

```text
program: nsenter
args:    --target=<init_pid> --user --preserve-credentials --mount --net --ipc --pid --wd=/ --
         sh -c 'cd -- "$1" && shift && exec "$@"' sh <HOST cwd> <target argv...>
cwd:     <HOST cwd>                       # chdir'd by the thin CLI on the host
```

After (`BtrfsEngine::entry_command`, the same prefix `exec_in_world` and `spawn_world_process` use):

```text
program: nsenter
args:    --target=<init_pid> --user --preserve-credentials --mount --net --ipc --pid
         --wdns=<world_guest_for_host(mounts, HOST cwd) | "/"> --
         <target argv...>                 # the user's shell is never wrapped in a `cd`
cwd:     "/"                              # exists on the host *and* in every world rootfs
```

`entry_prefix(init_pid, mounts, host_cwd)` is now the single builder: interactive `shell`/`exec`
(`entry_command`), non-interactive `exec` and hooks/`db`/`redis` daemon execs
(`exec_in_world` + `service_entry_command`), and every supervised `L0` service
(`spawn_world_process`) go through it. `world_cwd_wrapper` (the `sh -c cd && exec` shim) is deleted,
and `world_guest_for_host` no longer returns a trailing separator for the mount root, so the argv
names the directory verbatim.

`WorldEntryCommand.cwd` is `/` on purpose: the thin CLI `chdir`s there before `exec`ing `nsenter`, so
it must be a host path; the real in-world start directory is `--wdns=<mapped dir>`. `crates/slipfork-engine/src/lib.rs`'s
field docs state exactly that split.

## Contract pins (old host-path expectation deleted, not kept)

- `conformance.rs` item 5: `entry.cwd == "/"` (the one directory that exists on the spawner host and
  in every world rootfs) and neither `--wd=` nor the caller's host directory may appear in
  `args_prefix`. The old `assert_eq!(entry.cwd, entry_cwd)` is gone. The `nsenter`-specific
  `--wdns=<mapped dir>` pin stays in `critical_engine`, where the engine's argv is the subject.
- `critical_engine::persistent_init_pid_stays_stable_across_calls_and_is_reaped_on_destroy`: expects
  `--wdns=<worktree>` and `entry.cwd == "/"` (was `--wd=/` + the host path).
- `critical_engine::entry_command_enters_the_persistent_init_pid_with_native_stdio`: stderr must not
  contain `getcwd`.
- New `critical_engine::entry_and_exec_start_on_the_in_world_directory_mapped_from_the_host_path`:
  the worktree is mounted at a **different** guest path (`<world root>/stage`) that does not exist
  on the host. It asserts the entry argv names `--wdns=<root>/stage/pkg` (never the host path), then
  drives the entry command exactly like the thin CLI and gets `pwd == <root>/stage/pkg` with clean
  stderr; `exec_in_world` with the same host cwd starts in the same in-world directory.
- `critical_cli::up_down_lifecycle_is_idempotent_and_one_url_per_world`: `slipfork exec -- sh -c pwd`
  prints the mapped worktree and an **empty** stderr (the pre-fix run printed the `getcwd` line).
- `critical_ospp_daemon::remote_secrets_never_reach_disk_and_claimed_vars_resolve_from_the_local_vault`:
  the real `slipfork exec` transcript must not contain `getcwd`.

## Acceptance

```console
$ cargo test -p slipfork-engine --test critical_engine -- --test-threads=1
running 16 tests
test result: ok. 16 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 23.72s

$ cargo test -p slipfork-edge --test critical_edge
running 35 tests
test result: ok. 35 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.54s

$ cargo test -p slipfork --test critical_cli -- --test-threads=1
running 29 tests
test interactive_shell_prints_one_orientation_line_and_json_never_does ... ok
test result: ok. 29 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 97.62s

$ cargo test -p slipfork --test critical_ospp_daemon -- --test-threads=1
running 6 tests
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 80.09s

$ cargo test -p slipfork-engine --lib
test result: ok. 69 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 5.03s

$ ! grep -rn 'getcwd() failed' crates/ --include=*.rs && echo no-getcwd-noise
no-getcwd-noise

$ cargo clippy --workspace --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.89s

$ cargo fmt --all -- --check
(no output)

$ uv run --no-project tools/critical_guard.py --base 97efc4a --node M3.28.22
critical-guard: OK: authorized by M3.28.22

$ uv run --no-project tools/guard.py
guard: OK (604 file(s) checked)

$ uv run --no-project tools/plan.py validate
plan OK
```

`make check` exits 0.

## make check

```console
$ make check
plan OK
guard: OK (604 file(s) checked)
check_stub_adapters: OK (91 production file(s), 6 composition root(s))
150 passed in 12.59s                     # tools/tests (pytest)
check_support: OK (10 integration(s); all evidence pointers verified and docs/support-matrix.md in sync)
16 pass / 0 fail                         # packages/opencode-slipfork (bun test, 99 expect() calls)
cargo fmt --all -- --check               # clean
cargo clippy --workspace --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
cargo test --workspace --lib --bins      # + --doc, all green
cargo test -p slipfork-engine --test critical_engine -- --test-threads=1       16 passed (24.03s)
cargo test -p slipfork-edge --test critical_edge                               35 passed (0.54s)
cargo test -p slipfork --test critical_cli -- --test-threads=1                 29 passed (99.45s)
cargo test -p slipfork --test critical_ospp_daemon -- --test-threads=1          6 passed (102.22s)
cargo test -p slipfork --test critical_btrfs_live -- --test-threads=1           1 passed (0.10s)
uvx --offline zensical build --strict    # No issues found
check_docs_honesty: OK (10 doc(s) match real CLI subcommands: published pages plus support/demo/README.md)
make-check-exit=0
```

## Staging note

`crates/slipfork/src/main.rs` also carries `M3.28.11`'s landed-but-uncommitted `dag` deletion; this
node's contribution there is only the orientation block in `fn shell`:
`if !json { eprintln!("world rootfs is the app stage (no host tools; use slipfork ps / logs / open)") }`
between `exec_target` and `spawn_entry`.

## Live note

`M3.28.13` re-checks this on the droplet: `slipfork shell` must print the one orientation line and
hand over the prompt with no `getcwd` line; `slipfork exec -- pwd` must print the worktree silently.
