# Log M3.28.13

## 2026-11-17T16:33 [planner]

created (task) Live re-verification of the 0.1.0 UX pass on the Btrfs DO VM (port-less :80, status, restart, ps, fork)

## 2026-11-17T17:40 [lead]

edited fields

## 2026-11-17T21:48 [lead]

end-to-end restart proof added to evidence: edit an app file AND a snow.yml hook, run slipfork restart, show the old behaviour gone (M3.28.17 could only prove this at the engine-double boundary because ServiceHandle::mint is pub(crate)).

## 2026-11-18T00:08 [lead]

edited fields

## 2026-11-18T00:08 [lead]

edited fields

## 2026-11-18T00:37 [lead]

edited fields

## 2026-11-18T04:15 [lead]

edited fields

## 2026-11-18T05:03 [lead]

status ready -> active

## 2026-11-18T05:03 [lead]

attempt 1/3 started

## 2026-11-18T05:03 [lead]

base 9a79afc81a20eb5dc0a911525a82f6f0aa673df6

## 2026-11-18T05:34 [lead]

attempt 1 LIVE RESULT: 12/13 pass. The one failure is real and is the headline DX promise: restart leaves the old python3 orphaned (stop signals sh, wait_for_inner_pid captures only the first child) and reuses the cached rootfs, so an edited app is never served (old body, row exited, new spawn EADDRINUSE). Created M3.28.24 (option A: fix stop depth + rootfs re-materialization, do NOT declare restart snow.yml-only). Also observed: each daemon handover orphans the previous worlds/canonical-N (9 found, 1-7 predate this run) — M3.27 is the owner of that. fork on a slash-less branch whose sanitized name equals itself refuses with a clear message (acceptable, documented).

## 2026-11-18T05:34 [lead]

status active -> ready

## 2026-11-18T05:34 [lead]

edited fields

## 2026-11-18T05:34 [lead]

edited fields
