# Log M3.107.7

## 2026-11-24T14:35 [lead]

created (task) Keyword role = magenta 35 on every surface + byte-exact role test

## 2026-11-24T14:35 [lead]

status ready -> active

## 2026-11-24T14:35 [lead]

attempt 1/3 started

## 2026-11-24T14:35 [lead]

base 43ef83cdff78a61ca43cbf144940c4a2334405fb

## 2026-11-24T14:37 [lead]

status active -> ready

## 2026-11-24T14:37 [lead]

edited fields

## 2026-11-24T14:37 [lead]

status ready -> review

## 2026-11-24T14:40 [lead]

attempt 2/3 started

## 2026-11-24T14:40 [lead]

VM PTY capture: roles correct (35 verbs, 1 target, 2 meta, 31/32/33/36); defects: 0-col pty -> 8-char wraps; failing-step expansion printed ┃ before earlier │; bottom-bar spinner cyan; live row truncated without …

## 2026-11-24T14:43 [lead]

status review -> ready

## 2026-11-24T14:43 [lead]

edited fields

## 2026-11-24T14:43 [lead]

status ready -> review

## 2026-11-24T14:45 [lead]

attempt 3/3 started

## 2026-11-24T14:49 [lead]

status review -> ready

## 2026-11-24T14:49 [lead]

edited fields

## 2026-11-24T14:49 [lead]

status ready -> review

## 2026-11-24T14:49 [lead]

attempt3: root cause = racing stdout/stderr reader threads in world_init; merged poll reader (stdout-first on simultaneous readiness)

## 2026-11-24T14:54 [lead]

live VM 8/8 suites; PTY 120x32 + zero-size + NO_COLOR + --json captured; workspace check green; boot/** <=500

## 2026-11-24T14:54 [lead]

status review -> done
