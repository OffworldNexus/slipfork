# Log M3.24

## 2026-11-16T20:44 [lead]

created (task) Finish bounded-wait consolidation in world-booting suite harnesses and make rs-test-list comments match what the guard enforces

## 2026-11-16T20:45 [lead]

edited fields

## 2026-11-16T20:45 [lead]

SCOPE ADD (M3.1 cruft gate): also factor the duplicated kill -TERM -- -pgid block (critical_cli.rs:422 vs critical_slug.rs:66), the repeated "rt" runtime-dir literal (critical_slug.rs:56/:99), and the unnamed 30s window in support::wait_for_line (:349) into the shared harness constants/helpers.
