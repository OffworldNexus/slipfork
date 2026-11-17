# Log M3.28.6

## 2026-11-17T16:33 [planner]

created (task) slipfork daemon install: :80 socket activation, AppArmor profile, idempotent and fail-closed

## 2026-11-17T17:53 [lead]

edited fields

## 2026-11-17T18:14 [lead]

edited fields

## 2026-11-17T19:46 [lead]

edited fields

## 2026-11-17T22:13 [lead]

status ready -> active

## 2026-11-17T22:13 [lead]

attempt 1/3 started

## 2026-11-17T22:13 [lead]

base 501acf63690013fe6a16ca73c656f9723304dd1d

## 2026-11-17T22:21 [lead]

edited fields

## 2026-11-17T22:21 [lead]

attempt 2/3 started

## 2026-11-17T22:21 [lead]

attempt 1 cap-out after the bulk of the work; cargo check -p slipfork --all-targets is CLEAN on the tree. Paths widened to crates/slipfork/tests/** (removing the global --runtime-dir mechanically touches critical_slug.rs and critical_vcs_remote.rs, both mechanical flag deletions) and to daemon.rs/vaults.rs (layout API). Remaining: docs/reference/cli.md completion + acceptance/uv run --no-project tools/plan.py validate
plan OK
uv run --no-project tools/guard.py
guard: OK (562 file(s) checked)
uvx --offline ruff check tools poc
All checks passed!
uvx --offline ruff format --check tools poc
13 files already formatted
uv run --no-project --with pytest pytest -q tools/tests
........................................................................ [ 57%]
......................................................                   [100%]
126 passed in 15.78s
uv run --no-project tools/check_support.py --check
check_support: OK (10 integration(s); all evidence pointers verified and docs/support-matrix.md in sync)
bun test packages/opencode-slipfork/tests + RESULT.md.

## 2026-11-17T22:42 [lead]

status active -> review

## 2026-11-17T22:43 [lead]

quad round 1 (cruft): FAIL — MAJOR: Layout::for_account's runtime_dir Option is never Some in production and re-implements Layout::explicit; MAJOR: UnitSpec is built three times from the same 127.0.0.1:80/[::1]:80 literals (main.rs:286, daemon_unit_install.rs:161, test :405) -> one UnitSpec::for_account constructor; MAJOR (reassigned to M3.28.14): shipped docs still teach the superseded three-step install and a --runtime-dir client flag; MINORS: render_text prints the table twice, doctor's Inactive label is used for 'active but not :80' (lying hint), SystemdIngressProbe::new duplicates Default, engine's remediation hint still says sudo apparmor_parser, connect_or_spawn repeats connect's prologue, critical_cli keeps XDG_RUNTIME_DIR scaffolding for a deleted branch.

## 2026-11-17T22:43 [lead]

edited fields

## 2026-11-17T22:43 [lead]

quad round 1 (ioc): FAIL — MAJOR: under sudo, 'daemon unit' (preview) prints User=root//root/.cache while 'daemon install' writes User=dev//home/dev/.cache, so the module's core invariant (install writes the exact bytes daemon unit renders) is unenforced: two parallel unit-spec derivations, two account decisions. RULING: one ingress_unit_spec(&ResolvedAccount, &Layout, port, exe, bin_dirs) + one target_account() used by both, one INGRESS_ADDRS const, and the privilege/target decision behind one UserIdentity method. BLIND_SPOT resolved without escalation: IngressCheck's third state conflates 'active on non-:80 listeners' with 'inactive', and its hint (systemctl start) cannot change the outcome — add a fourth typed state ActiveOtherListeners { listen } and update ADR-0011 3 (path added).

## 2026-11-17T22:43 [lead]

quad round 1 (legality): FAIL — BLOCKER: --bin-dir is interpolated verbatim into /etc/apparmor.d/slipfork, so a newline injects arbitrary profile lines (loaded by apparmor_parser -r as root) and {} widens the userns grant; a space breaks the header; relative/empty dirs render /slipfork. Must validate pre-write (absolute, no %, no whitespace/control/{},"\), one validator shared by daemon unit and install, fail closed in one line. MAJORS: the 'install writes the exact bytes daemon unit renders' claim is false under sudo (install uses the target account's getent home; daemon unit uses the invoking root HOME and has no --user) and its proof recomputes expected bytes through the same Layout (tautological); docs sample prints a sudo prefix the binary never emits. MINORS: no rollback story (state convergent-by-rerun and validate-then-write), no parent-dir fsync, doctor active-but-not-80 state. Critical guard OK, no assertion weakened.

## 2026-11-17T22:52 [lead]

edited fields

## 2026-11-17T22:52 [lead]

attempt 3/4 started

## 2026-11-17T22:52 [lead]

quad round 1 (functional): FAIL — 2 BLOCKERs are in docs/getting-started.md (still the deleted three-step | sudo tee recipe; XDG_RUNTIME_DIR/daemon.log path and 'every world command exits' now false) -> assigned to M3.28.14, which owns that file. MAJORS for this node: daemon unit vs install derivations disagree (see ioc/legality); enable_now runs with no shutdown handover, so a zero-setup slipforkd holding daemon.sock makes slipforkd.service die SocketInUse AFTER the files are written — dx 2.3 mandates DaemonControl::shutdown + socket-release await before enable (this is the real caller for the shutdown seam .3 removed); docs sample prints a sudo the binary never emits.

## 2026-11-17T23:11 [lead]

round 2 (legality): PASS — validator reachable only through Result-propagating callers, injection exits 1 writing nothing, account validated via getent with real home, one derivation shared by daemon unit --user and install (real-subprocess byte test), handover ordered release-before-enable with a bounded wait, no sh -c. Three MINORs carried: % in --state-dir renders an unescaped systemd specifier; installed unit pins <home>/.cache/slipfork while the client honours $XDG_CACHE_HOME (a user with it set gets 'no slipforkd'); cli.md lists 3 ingress statuses but doctor emits 4.

## 2026-11-17T23:16 [lead]

edited fields

## 2026-11-17T23:16 [lead]

attempt 4/4 started

## 2026-11-17T23:16 [lead]

round 2 (functional): FAIL only on scope — the Shutdown plumbing touched edge/src/{lib,daemon,daemon/client,daemon/server}.rs, which I have now authorized (paths widened to crates/slipfork-edge/src/**). Plus MINORs: cli.md ingress statuses must list all four; critical_cli's allowlist excludes active_other_listeners; empty Listen= is classified as active-without-80 instead of listeners-unreadable; installer errors do not append the convergent-by-rerun suffix (docs only); % in --state-dir renders an unescaped systemd specifier; installed unit pins <home>/.cache while the client honours $XDG_CACHE_HOME.

## 2026-11-17T23:36 [lead]

status review -> done

## 2026-11-17T23:36 [lead]

pre-commit verified: lib 61, %-specifier rejected for service+socket with one line (apparmor legitimately unaffected: that profile never interpolates --state-dir), clippy clean, critical_guard OK, make check rc=0. Two rounds of quad gate closed: injection validator, one target_account/ingress_unit_spec with a real-subprocess byte test, shutdown handover with its real caller, fourth ingress state, XDG unification, % rejection, convergent-by-rerun suffix.
