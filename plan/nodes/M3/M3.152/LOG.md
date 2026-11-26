# Log M3.152

## 2026-11-25T09:43 [conductor]

created (task) Append-only boot output: completed rows as normal lines, one in-place status line, no cursor addressing or scroll regions

## 2026-11-25T09:43 [lead]

handover from M3.149 (append-only rewrite contract): keep the TailPhase suffix on the single in-place status line (the phase row is the status line now - tail phases must still refresh there, e.g. '... · sealing' while the Btrfs readonly commit runs, not only on the row printed at Begin). Keep the live-row vocabulary - spinner, phase label [i/n] kind target, elapsed - on that one line; removing block rows must not remove them. Exit: \r + ESC[2K for the status line only, then the verdict rows, zero escapes after the final row; no DECSTBM, no absolute addressing, no SIGWINCH margin handler. Serialize behind M3.150's boot/** hunks and re-read boot.rs/pane.rs/phase.rs/build.rs immediately before editing.

## 2026-11-25T10:10 [conductor]

Owner addenda from the 11-step build: (1) step counters must pad for the plan's widest i/N (e.g. ' 1/11' vs '10/11') for both outer and inner levels, width derived from the plan, never hardcoded; (2) the image row was emitted TWICE ('✓ [1/2] image api built 2 steps 248ms' then '✓ [1/2] image api built 25ms') — one image settles exactly once; (3) a mixed run that shows 11 instruction rows must not summarise as 'built 2 steps' alone: render 'cached 9 · built 2' (or equivalent) so the count matches what was shown.

## 2026-11-25T10:28 [conductor]

status ready -> active

## 2026-11-25T10:29 [lead]

edited fields

## 2026-11-25T10:29 [lead]

status active -> active

## 2026-11-25T10:29 [lead]

attempt 1/3 started

## 2026-11-25T10:29 [lead]

base 09168fa441218b492b21a3971c97325c203a78dd

## 2026-11-25T10:38 [lead]

recovery rescan: live-only critical_status_live.rs:580-989 still asserts DECSTBM/ESC[r/DSR (out of my paths + protected); append-only gate cannot flip it here — flagging for a follow-up node

## 2026-11-25T11:12 [lead]

commit df054e8; five gates green in isolated 6b095fa worktree with separate target (fmt, clippy -D warnings, boot 62, ansi_matrix 7, boot_pty 4+1 ignored fixture; 110x30/80x24 cached+fresh+failing+mid-run resize). Triad: ticker probe injected, status line hard-clamped so it never wraps, LiveSteps split (phase.rs 467), image settle-once + idempotent advance + cached 9 · built 2. lib.rs 2-line mapping exposes BootTui to tests; frames.rs parse helpers cfg(test). OPEN: protected critical_status_live.rs:580-989 still asserts DECSTBM/ESC[r/DSR and needs an authorized node to rewrite before the live epic gate.

## 2026-11-25T11:12 [lead]

status active -> done
