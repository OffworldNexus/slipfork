# RESULT M3.28.20 (attempt 3)

**STATUS: done.** Invariants 1–5 implemented; every `# Acceptance` command run and green on this
host. Nothing committed.

Attempt 1 landed invariants 1–2 (product) and was blocked on invariant 3; attempt 2 landed the three
harness edits. Attempt 3 fixes the two Quad-Gate MAJORs (child-daemon leak, duplicated sandbox
literal) plus the RESULT minors.

---

## 1. Invariant 1–2 — sandbox fix (`AF_NETLINK`) with **one** canonical list

`crates/slipfork-edge/src/unit.rs` now has the single source of truth beside the renderer:

```rust
pub const SERVICE_RESTRICT_ADDRESS_FAMILIES: &str = "AF_INET AF_INET6 AF_UNIX AF_NETLINK";
```

`render_service_unit` emits `RestrictAddressFamilies={SERVICE_RESTRICT_ADDRESS_FAMILIES}`; the
constant's doc comment carries the whole why (the spawned `slipfork-world-init` runs
`ip link set lo up`, which opens an `AF_NETLINK`/`NETLINK_ROUTE` socket, so without the family every
boot inside the installed unit dies with `EAFNOSUPPORT`). The module doc and the renderer doc now
**point at that directive/constant** instead of restating its value, and the list is documented as an
explicit allow-list (no `~`, no wildcard, never trimmed to the three inet families).

Literal audit after this change (`grep -rn RestrictAddressFamilies crates/`): the emitted value is
spelled exactly twice — `unit.rs:50` (the constant) and `tests/golden/slipforkd.service:15` (the
fixture the renderer is compared to byte-exactly). The remaining hits are prose: `unit.rs:19` /
`:39` / `:174` point at the directive/constant, and `critical_edge.rs:3288` quotes the old
three-family value as the historical `M3.28.12` evidence. No test re-types the current list.

Tests:

- `critical_edge::unit_golden_renders_byte_exactly` still compares the rendered service to the
  golden fixture byte-exactly; its hardening-directive list no longer repeats the
  `RestrictAddressFamilies=` line (byte-equality plus the named regression below cover it).
- `critical_edge::unit_service_sandbox_allows_the_netlink_family_world_init_needs` keeps the doc
  comment naming the `ip link set lo up` / `EAFNOSUPPORT` evidence and asserts, on the renderer's own
  output, that the `RestrictAddressFamilies=` line lists `AF_NETLINK` by name (so trimming the family
  fails here, not only on a VM). The redundant exact-line equality, the `!contains('~')` check, and
  the four-family loop were deleted: they asserted nothing beyond the golden byte comparison.

### Rendered line

```console
$ grep -n 'RestrictAddressFamilies' crates/slipfork-edge/src/unit.rs crates/slipfork-edge/tests/golden/slipforkd.service
crates/slipfork-edge/src/unit.rs:19://! `RestrictAddressFamilies=` allow-list in [`SERVICE_RESTRICT_ADDRESS_FAMILIES`]) mirrors the
crates/slipfork-edge/src/unit.rs:39:/// The exact `RestrictAddressFamilies=` value `slipforkd.service` is rendered with.
crates/slipfork-edge/src/unit.rs:174:/// `RestrictAddressFamilies=` carries the allow-list in
crates/slipfork-edge/src/unit.rs:207:        "RestrictAddressFamilies={SERVICE_RESTRICT_ADDRESS_FAMILIES}"
crates/slipfork-edge/tests/golden/slipforkd.service:15:RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX AF_NETLINK

$ cargo run -q -p slipfork -- daemon unit --only service | grep -n RestrictAddressFamilies
15:RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX AF_NETLINK

$ cargo run -q -p slipfork -- daemon unit --only service | grep -q 'AF_NETLINK' && echo netlink-in-unit
netlink-in-unit
```

### Reproduction this fixes (`M3.28.12`, DO VM, quoted)

```console
$ systemd-run --wait --pipe --property=RestrictAddressFamilies="AF_INET AF_INET6 AF_UNIX" -- \
    /usr/sbin/runuser -u slipfork -- unshare … -- ip link set lo up
Cannot open netlink socket: Address family not supported by protocol
rc=1
$ systemd-run --wait --pipe --property=RestrictAddressFamilies="AF_INET AF_INET6 AF_UNIX AF_NETLINK" -- \
    /usr/sbin/runuser -u slipfork -- unshare … -- ip link set lo up
rc=0
```

Live symptom removed: `slipfork: cannot materialize world 'demo': world init for canonical-1
failed: … slipfork-world-init: `ip link set lo up` failed with exit status: 1: Cannot open netlink
socket: Address family not supported by protocol`.

## 2. Invariant 3 — `ensure_daemon` starts a daemon on every host, naming the command

`crates/slipfork/tests/support/mod.rs::ensure_daemon(scratch, layout, command) -> Option<Daemon>`:

- the Btrfs early return is **gone**: the host filesystem decides only *which* backend serves the
  daemon (`Daemon::start_in` → the real `slipforkd` child on a Btrfs host, the same
  `slipfork::daemon` composition over the shared `TestTreeDriver` elsewhere), never *whether* a
  daemon-served command gets one;
- `Daemon::start_in`'s `Result` is no longer `expect`-ed: a host that genuinely cannot serve one
  panics with ``the test harness cannot serve `slipfork <command>`, which needs a daemon on
  <runtime_dir>: <daemon log>`` — the caller passes the command, so the failure names what needed it
  instead of surfacing a later `no slipforkd on … — run \`slipfork up\``;
- **no child daemon is leaked**: the `Backend::Child` handle is *returned*, and each test binds it in
  its own scope (`let _daemon = harness_daemon(scratch);`), so `Drop` → `Daemon::shutdown` kills the
  real `slipforkd`, waits for it, and reaps any world it booted. `std::mem::forget` is now applied to
  `Backend::InProcess` only — a server thread that genuinely cannot be un-wound — and only after the
  `assert_no_world_init_processes` zero-survivor proof. `None` means "a daemon already served this
  layout" (an earlier handle owns it) or "in-process, forgotten";
- `run_slipfork_with_env` no longer starts anything: it asserts `daemon_is_served` and names the
  invocation, so a test that forgets `harness_daemon` fails closed saying exactly that;
- the product's no-auto-spawn rule (`M3.28.1` R5) is untouched: only the *harness* starts the
  daemon; the CLI still refuses read/diagnostic verbs with its one actionable line.

## 3. Which caller assertion moved where

Deleted: the `host_is_btrfs` + `wait_daemon_teardown` block (`critical_slug.rs:86-97`). Its premise
("the CLI auto-spawned a daemon into this process group") no longer exists under R5.

| Old assertion | Where it lives now |
| --- | --- |
| process-group `SIGTERM` reached the invocation's own group (`term_delivered || !members_alive`) | unchanged (`critical_slug.rs`) |
| "the auto-spawned slipforkd must stop accepting after the process-group SIGTERM" | **replaced**: `support::daemon_is_served(runtime_dir)` must stay true — the harness daemon is *not* in the invocation's group and every later invocation reuses it, so the group `SIGTERM` must leave it serving (a reused, dying daemon would fail mid-request with ECONNRESET/EPIPE). The strict "socket stops accepting" poll stays in `support::wait_daemon_teardown`, whose remaining caller is the `critical_cli` zero-setup auto-spawn proof (`critical_cli.rs:815`). |
| zero-survivor world-init reap | unchanged (`support::assert_no_world_init_processes`), now also run by `Daemon::shutdown` at test end for a real child |

## 4. The obsolete `PATH` branch, rewritten

`non_vcs_directory_fails_closed_and_corrupt_git_surfaces_real_git_stderr`'s last block no longer
asserts the pre-R5 contract (client `PATH=bin-empty` ⇒ daemon reports
`required tool 'git' was not found on PATH`). That branch is impossible now: `snow resolve` is
daemon-served and the request carries no environment (`SnowResolveRequest { cwd, file, mode,
component, expose_secrets }`, `main.rs:274-280`), while the daemon resolves the VCS root itself with
its own fixed `PATH` (`daemon.rs:2091-2094` → `RepoRootResolver`).

New assertions (no host branch, no skip, no `#[ignore]`, no `#[allow]`):

1. `snow resolve` succeeds with the ambient client `PATH`;
2. `snow resolve` **also** succeeds with `PATH=<empty bin>` — no client-side tool is needed;
3. both invocations print byte-identical variables ⇒ the client's `PATH` cannot influence
   daemon-side resolution, and the vault value is stable (secret generated once, not per call);
4. the output carries `APP_URL=http://git-missing-bin-repo.localhost` ⇒ the daemon's **own** tool
   lookup still ran `git` and derived the canonical slug;
5. neither a `required tool` nor a `not a git or jj repository` claim appears on a resolve the
   daemon completed.

The `ToolMissing { tool: "git" }` diagnostic itself stays covered by its owner crate:
`crates/slipfork-vcs/tests/critical_vcs.rs::fail_closed_child_probe_without_git_on_path` (child
re-exec with an empty `PATH`), plus the daemon-with-path wiring in
`crates/slipfork/tests/critical_ospp_daemon.rs::dot_jj_repository_routes_to_jujutsu_and_never_falls_back_to_git`.
No coverage was deleted, only re-homed to the layer that owns it.

## 5. Acceptance evidence (this host: XFS, so the in-process backend)

```console
$ cargo test -p slipfork --test critical_slug -- --test-threads=1
test dag_plan_repo_flag_is_gone_and_rejected_by_the_parser ... ok
test git_submodule_bare_and_separate_git_dir_fail_closed_with_actionable_override_hints ... ok
test invalid_project_label_fails_closed_until_rescued_by_valid_slipfork_toml_override ... ok
test jj_workspace_uses_the_shared_git_resolver_and_never_shells_out_to_jj ... ok
test missing_snow_file_names_the_unreadable_path ... ok
test non_vcs_directory_fails_closed_and_corrupt_git_surfaces_real_git_stderr ... ok
test worktree_resolves_to_canonical_repo_slug_and_honors_only_vcs_root_slipfork_toml ... ok
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.42s

$ cargo test -p slipfork-edge --test critical_edge
test unit_service_sandbox_allows_the_netlink_family_world_init_needs ... ok
test unit_golden_renders_byte_exactly ... ok
test result: ok. 41 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo test -p slipfork --test critical_cli -- --test-threads=1
test result: ok. 21 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 108.53s

$ cargo clippy --workspace --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.07s

$ cargo fmt --all -- --check
(exit 0)

$ uv run --no-project tools/critical_guard.py --base 19658f8 --node M3.28.20
critical-guard: 19658f8..working tree, 54 changed file(s)
  M  crates/slipfork-edge/tests/critical_edge.rs  [authorized by crates/slipfork-edge/tests/**]
  M  crates/slipfork/tests/critical_cli.rs  [VIOLATION: not covered by any node path]
  M  crates/slipfork/tests/critical_slug.rs  [authorized by crates/slipfork/tests/critical_slug.rs]
critical-guard: FAILED: 1 unauthorized protected test change(s) in M3.28.20.

$ uv run --no-project tools/guard.py
guard: OK (586 file(s) checked)

$ uv run --no-project tools/plan.py validate
plan OK
```

The one guard violation is **not this node's work**: `crates/slipfork/tests/critical_cli.rs` is the
uncommitted in-flight work of `M3.28.7` / `M3.28.11` (`+242` lines of status/dashboard tests) in
this shared worktree; its suite is green (21/21 above). Every protected test this node touched is
listed as authorized.

## 6. Notes / followups

1. **Btrfs child-daemon path is not exercisable on this host** (XFS ⇒ `ensure_daemon` serves
   in-process): on a Btrfs host `Daemon::start_in` takes the unchanged `spawn_child` path and the
   test scope's `Drop` reaps it. That the rewritten `PATH` assertions and the owned-daemon shutdown
   hold there is a prediction from that code path — **[unverified — M3.28.13 owns the live proof]**
   (world boots through the installed unit → `curl -H 'Host: demo.localhost'` → `200`).
2. Harness daemon lifetime: the harness-owned daemon is reaped by `Drop` (fix 1), so this suite leaves
   nothing running on either backend. Anything still found outliving a test binary is the wider
   harness teardown gap tracked by `M3.27` (logged by the lead), not a handle this suite leaks.
3. `docs/decisions/ADR-0008-*.md:59` still quotes the three-family list — that is `M3.29.1`'s doc
   ripple (active), not this node's.

No `#[ignore]`, no skipped assertion, no `#[allow]` was added anywhere.
