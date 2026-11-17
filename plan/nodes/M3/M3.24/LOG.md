# Log M3.24

## 2026-11-16T20:44 [lead]

created (task) Finish bounded-wait consolidation in world-booting suite harnesses and make rs-test-list comments match what the guard enforces

## 2026-11-16T20:45 [lead]

edited fields

## 2026-11-16T20:45 [lead]

SCOPE ADD (M3.1 cruft gate): also factor the duplicated kill -TERM -- -pgid block (critical_cli.rs:422 vs critical_slug.rs:66), the repeated "rt" runtime-dir literal (critical_slug.rs:56/:99), and the unnamed 30s window in support::wait_for_line (:349) into the shared harness constants/helpers.

## 2026-11-16T21:20 [lead]

edited fields

## 2026-11-16T21:20 [lead]

SCOPE ADD (M3.1 attempt 5): both acceptance sequences leaked 46 orphaned unshare/slipfork-world-init worlds (92 ppid-1 processes) from ~/.cache/slipfork-cli-tests scratch. Add harness cleanup: boundedly reap any surviving world-init process group after each CLI suite and assert zero leaked worlds at suite end (no sleep padding, no weakening).

## 2026-11-16T22:09 [lead]

ADD (M3.1 ioc): (1) testkit DAEMON_STARTUP_DEADLINE/SHUTDOWN_DEADLINE re-encode production numbers as prose — once the product exports SPAWN_TIMEOUT/WORLD_STOP_GRACE, derive them from those constants (and keep the margin rationale); (2) convert critical_engine.rs:96 wait_until and critical_l0_wiring.rs:172 Instant loop to the testkit poll_until; (3) state wait_for_line as the one remaining non-poll (blocking recv_timeout) wait.

## 2026-11-17T07:14 [lead]

EVIDENCE for your scope: after hours of world-booting suites the host had 30 orphaned 'unshare' supervisors (ppid 1, SN, do_wait) and critical_cli reds with world init pid survived SIGTERM+SIGKILL 15s. Your orphan reaping must cover supervisors whose inner init ignores ancestor SIGKILL (namespace PID 1 semantics).

## 2026-11-17T11:17 [lead]

ADD: the linux_cow fd-count flake is assigned to M3.21 (owner of linux_cow.rs). Your Makefile/test scope remains bounded-wait consolidation + orphan reaping.

## 2026-11-17T12:36 [lead]

status ready -> active

## 2026-11-17T12:36 [lead]

attempt 1/3 started

## 2026-11-17T12:36 [lead]

base 94831c119233adac1d573cf401ddfe653d923254

## 2026-11-17T13:08 [lead]

edited fields

## 2026-11-17T13:08 [lead]

SCOPE WIDENED (lead): the working tree also carries two product cleanups that appeared while this node was active and are adopted here because they are exactly this node's subject matter: (1) crates/slipfork/src/daemon.rs injects the postmaster-readiness clock+sleeper (PostgresReady) so the bounded wait is testable without a real 30 s poll — this closes the M3.15 review-ioc MAJOR I had deferred; (2) crates/slipfork/src/{cli,main}.rs delete the 'dag plan --repo' flag, a documented user no-op that only accepted the VCS root. Both are covered by tests (unit + critical_slug) and make check exit 0.

## 2026-11-17T13:13 [lead]

edited fields

## 2026-11-17T13:13 [lead]

Quad gate 1/2: legality MAJOR (the 'daemon unit writes no files' assertion checks paths the command never touches — it cannot fail), cruft MAJOR x2 (three poll_until copies incl. testkit; Makefile overclaims 'no tests/ file is left unbuilt'), ioc SPEC_BLIND_SPOT (ReadinessProbe/Clock-Sleeper absent from the DI tree) → DISPOSITION: not a new blind spot, M3.25 ruling already parks ReadinessProbe as 0.2.0 target state (M3.23 LOG 2026-11-17T05:13) and PostgresReady is the 0.1.0 stopgap; not re-escalated. Attempt 2 fixes the rest.

## 2026-11-17T13:32 [lead]

edited fields

## 2026-11-17T13:42 [lead]

Gate passed: Quad reviewer PASS after attempts 2-3 (hollow daemon-unit assertion, three poll_until copies, fail-open /proc scan, over-claiming Makefile comments all fixed; attempt-3 addendum covers the poc --repo argv, a non-production readiness test policy and the TMPDIR wording). make check exit 0 on the final tree; guard OK.

## 2026-11-17T13:42 [lead]

status active -> done
