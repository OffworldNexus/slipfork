---
id: M3.74
title: "Add -i/--interactive and --no-interactive (with TTY auto-detection by default) to slipfork exec"
kind: task
status: ready
owner: implementer
priority: 3
depends_on: []
paths: ["crates/slipfork/**", "packaging/man/**", "specs/**", "docs/**", README.md, TESTING_PLAN.md]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-23T21:13"
updated: "2026-11-23T21:13"
---

# Goal

`slipfork exec api -- uv run ipython` from an interactive terminal currently runs over the non-PTY pipe path (`Request::Exec`), causing interactive TUI/REPL tools (`ipython`, `python`, `bash`, `psql`, colored progress bars) to see non-TTY stdio and exit or degrade.

Add `-i, --interactive` and `--no-interactive` to `slipfork exec`, auto-detecting TTY by default and reusing the existing real kernel PTY infrastructure (`Request::ExecPty` + `SCM_RIGHTS` + `RawModeGuard` + `SIGWINCH`) already used by `slipfork shell`!

# Exact Behavior & Contract

1. **CLI Flags on `Command::Exec` (`crates/slipfork/src/cli/verbs.rs:175`):**
   - `-i, --interactive` (`conflicts_with_all = ["no_interactive", "json"]`): require interactive PTY mode.
   - `--no-interactive` (`conflicts_with = "interactive"`): disable PTY allocation even when invoked from an interactive terminal.
   - Regenerate `packaging/man/slipfork.1` (`cargo run -q -p slipfork -- --manpage > packaging/man/slipfork.1`) so `manpage_drift` stays green!

2. **Mode Selection (`crates/slipfork/src/commands/streams.rs:274`):**
   - Let `has_tty = self.interactive() && std::io::stdin().is_terminal();`
   - If `args.interactive && !has_tty`: fail closed (`self.report("no-tty", "`--interactive` requires an interactive terminal", "run from a terminal, or omit `-i` / pass `--no-interactive`", args.json)` -> `ExitCode::FAILURE`).
   - Let `use_pty = !args.json && !args.no_interactive && (args.interactive || has_tty);`
   - **When `use_pty == true`:**
     - Measure `(cols, rows) = crate::commands::pty::geometry()`, call `client.call_pty("exec", json!({ "project": self.cwd(), "target": args.target, "argv": args.argv, "tty": true, "cols": cols, "rows": rows, "select": "any" }))`, attach with `crate::commands::pty::attach(&mut client, &id, &master)`, and return child exit status from `client.finish_pty(&id, "exec")` — sharing a clean helper with `shell` so PTY attach/teardown logic is not duplicated!
   - **When `use_pty == false` (piped stdin/stdout, `--no-interactive`, or `--json`):**
     - When `args.no_interactive && std::io::stdin().is_terminal()`, pass `"stdin": false` (do not pump terminal stdin in explicit `--no-interactive` mode so prompts get immediate EOF instead of hanging; when stdin is a pipe `!std::io::stdin().is_terminal()`, still pass `"stdin": true` so `echo hi | slipfork exec --no-interactive api -- cat` forwards piped input!).
     - Run the existing `exec_like("exec", params, args.json, "exec")` path!

3. **Daemon `"exec"` PTY Relay (`crates/slipfork/src/daemon/mod.rs:242` & `crates/slipfork/src/daemon/lifecycle/stream.rs`):**
   - When `request.method == "exec"` and `request.params.get("tty") == Some(&Value::Bool(true))`, resolve the `exec` target + `argv` (same target validation as `open_exec`: `argv` non-empty, `choose(target, &ids, select)`), send `Request::ExecPty { v: PROTOCOL_VERSION, id: "daemon-exec-pty", pod, argv, cols, rows }` on `control.sock`, receive the PTY master FD via `recv_fd`, and relay resize/exit through the exact same `handle_pty` path used by `"shell"`! Keep `daemon/mod.rs` and `daemon/lifecycle/stream.rs` `<= 500` LOC (extract cohesive helper/submodule if needed).

4. **Tests, Specs & Docs:**
   - Add real PTY + non-PTY lab coverage in `crates/slipfork/tests/` proving:
     1) `slipfork exec` on a real PTY auto-attaches a kernel PTY (`tty -s` / `python3 -c "import sys; assert sys.stdin.isatty() and sys.stdout.isatty()"` exits `0`!),
     2) `slipfork exec --no-interactive` on that same PTY disables PTY (`sys.stdin.isatty()` is `False` and stdin hits EOF without hanging!),
     3) `slipfork exec -i` without a TTY fails closed with `1 line + next:`,
     4) piped `echo hello | slipfork exec ... -- cat` still works identically.
   - Update `specs/02-cli-ux.md`, `docs/cli.md` (and any other doc mentioning `slipfork exec`), `README.md` if applicable, and `TESTING_PLAN.md` (`make docs-check` green!).

5. **Live Upgrade on `165.232.86.174` (DO NOT WIPE `/home/slipfork/demo`!):**
   - Build & install the updated `glibc <= 2.34` `.deb` on `165.232.86.174` and restart `systemctl --user restart slipfork.service` as user `slipfork` **WITHOUT running `do_vm.py sync` or touching `/home/slipfork/demo`** (preserving the owner's live `numpy` + `ipython` packages installed in `/home/slipfork/demo`!).
   - Verify via a real PTY (`script -qec ...` or `python3 -c 'import pty...'`) on `165.232.86.174` as user `slipfork` in `/home/slipfork/demo` that `slipfork exec api -- uv run ipython` opens an interactive `In [1]:` prompt, evaluates `1 + 1` -> `Out[1]: 2` and exits cleanly on `exit()`, AND `slipfork exec --no-interactive api -- python3 -c "import sys; print(sys.stdin.isatty())"` prints `False`!

# Acceptance

- `make check` and `make docs-check` exit `0`.
- Live PTY probe of `slipfork exec api -- uv run ipython` in `/home/slipfork/demo` on `165.232.86.174` evaluates `In [1]:` -> `Out[1]:` interactively and `slipfork exec --no-interactive` prints `False` for `isatty()`.
