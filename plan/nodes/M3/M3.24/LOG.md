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
