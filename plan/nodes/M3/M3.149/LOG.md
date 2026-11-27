# Log M3.149

## 2026-11-25T08:57 [conductor]

created (task) up resets the terminal at exit: ScrollRegionGuard::drop moves the cursor to the last row, erases it and resets margins after the final output

## 2026-11-25T08:57 [conductor]

edited fields

## 2026-11-25T08:57 [conductor]

status ready -> active

## 2026-11-25T08:59 [lead]

status active -> active

## 2026-11-25T08:59 [lead]

attempt 1/3 started

## 2026-11-25T08:59 [lead]

base e460aca2bc0756cef52a33787aa46e5dce8d908c

## 2026-11-25T09:15 [lead]

status active -> done

## 2026-11-25T09:15 [lead]

commit 40c0fce. Removed from ScrollRegionGuard::drop exactly b'\x1b[{rows};1H' + b'\x1b[2K': they moved to the terminal's last row and erased it. drop/reset now writes exactly ESC[r on normal exit, panic unwind and every SIGWINCH; set_size resets margins and re-installs without that exit move/erase. region.rs owns margins only (install = ESC[1;{rows-1}r); pane.rs owns every row address through one clear_row literal and writes one exit stream (pane block rows, panel row, ESC[r, then the park LAST because DECSTBM homes the cursor) before disarming the guard, so nothing is written after the final verdict row. A growing resize clears the old panel row so no frozen bar is left. Byte-exact tests: RESTORE_MARGINS == ESC[r, install margins-only/empty below 3 rows, whole exit stream (clears -> single ESC[r -> park last, only pane rows addressed, no ESC[J / ESC c / alt-screen).

Real 110x30 openpty capture of target/debug/slipfork up (scratch Forkfile, prompt seeded on row 30, DSR answered): epilogue = ESC[30;1H CR ESC[2K  ESC[r  ESC[29;1H CR  then the verdict rows; exactly one ESC[r; post-final-newline bytes b''; rows 25-26 (prompt + ephemeral note) survive intact; row 30 blank with the cursor (shell's next line). NO_COLOR PTY: 0 ESC[ bytes. --json PTY: 0 ESC[ bytes.

LIMITATION: this container has no Btrfs mount and no loop devices, so a live world cannot reach the Ready row here; the captured final row is the typed failure verdict, and the old bytes would have erased exactly that last row (row 30). Live Ready-row proof remains with the Btrfs VM suites; ansi_matrix (real 120x40 PTY, up/restart/down/fork) passes.

Triad: all three reviewers flagged the exit ordering; one consolidated pass moved the park after ESC[r (the first reorder put the verdict on row 1 because DECSTBM homes the cursor - caught by the PTY capture), made the exit stream pure/testable and deleted the tautological tail test. make check, guard.py and critical_guard green; no critical test touched.

## 2026-11-25T09:35 [lead]

status done -> ready

## 2026-11-25T09:35 [lead]

edited fields

## 2026-11-25T09:35 [lead]

status ready -> active

## 2026-11-25T09:35 [lead]

attempt 2/3 started

## 2026-11-25T09:35 [lead]

panic follow-up: owner asciinema shows capacity overflow in <[T]>::repeat (bar cells - filled) at 3/3; base 40c0fce98bcde77cb5cc759b0d7743b8324b6c20

## 2026-11-25T09:35 [lead]

edited fields

## 2026-11-25T09:43 [lead]

panic fix landed as 55fcfab (bar clamp + producer + audit + real-PTY no-panic test; make check green). Append-only rewrite is BLOCKED on a concurrent writer: boot/phase.rs and boot/build.rs were modified 09:37:51/09:37:55 (now 09:43) with an uncommitted TailPhase feature (slipfork-world snapshot tail frames, render/tests, phase::inner_row suffix) that displays tail phases in the anchored live rows this rewrite deletes. No active plan node claims boot/** (M3.149 holds the lock), so the other agent is writing outside its paths. Options for the conductor: (A) land/commit the tail work first, then I rewrite boot/** in one exclusive pass; (B) authorize discarding the tail WIP and I delete block_rows/live rows now; (C) keep the margins-only + clamp fix and defer the rewrite to a dedicated node. Holding the rewrite until a decision - no foreign hunks staged.

## 2026-11-25T09:43 [conductor]

status active -> done
