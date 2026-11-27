# Log M3.173

## 2026-11-25T17:06 [conductor]

created (task) Slice 2c: boot pane on ui/report/live.rs (one status line, rows from Report), delete the bespoke renderer

## 2026-11-25T17:21 [conductor]

status ready -> active

## 2026-11-25T17:23 [lead]

edited fields

## 2026-11-25T17:23 [lead]

status active -> active

## 2026-11-25T17:23 [lead]

attempt 1/3 started

## 2026-11-25T17:23 [lead]

base ce575731eaba88eaee80096ab8117c0364498cf7

## 2026-11-25T17:24 [lead]

edited fields

## 2026-11-25T17:24 [lead]

split required (verified): S1 Pane move (ui/report/live.rs + boot/status*, boot.rs); S2 typed failure owner (add world/supervise.rs + world/runtime/frames.rs paths, delete boot/failure.rs, collapse lifecycle_failure); S3 typed rows (add tests/status_surface.rs path, Row/Done mixed + declined/finished outcomes, completed_row(&Row), rename ImageTally, rewrite boot tests, PTY byte-proof). serially: S1 -> S2 -> S3.

## 2026-11-25T17:24 [lead]

status active -> ready: needs split: S1 Pane, S2 typed failure producer+owner, S3 typed rows/status_surface

## 2026-11-25T17:25 [conductor]

Split approved (conductor): S1 Pane mechanics (ui/report/live.rs, boot/status*, boot.rs wiring); S2 typed failure (world/supervise.rs + runtime/frames.rs real FailureFacts, ui/report/failure.rs projection, delete boot/failure.rs, one Failure, fix ui/failure.rs:4); S3 typed rows (tests/status_surface.rs, Row/Done variants incl. an explicit Done::Finished kept byte-identical for the unknown-image row, completed_row(&Row), rename ImageTally, settle planned-count deletion, boot-test rewrite, HEAD-vs-after PTY diff). Serial S1->S2->S3. Decision: keep 'finished' as an explicit Done::Finished (byte-preserving), not a fallback.

## 2026-11-25T17:25 [conductor]

status ready -> superseded
