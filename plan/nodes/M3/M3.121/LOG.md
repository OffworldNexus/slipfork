# Log M3.121

## 2026-11-24T17:46 [lead]

created (task) Image cache ignores Dockerfile edits: changed RUN reuses the old image

## 2026-11-24T17:47 [conductor]

status ready -> superseded: duplicate of M3.115; image build cache ignores Dockerfile/context changes

## 2026-11-24T19:18 [lead]

edited fields

## 2026-11-24T19:18 [lead]

Lead VM check at 43cfeff: Dockerfile edits now converge (RUN edit rebuilds and fails with exit 9, with and without down) -> that half is covered by M3.115. NOT covered: build-context edits — M3.115's own live test critical_world_journey::changed_build_inputs_rebuild_and_restart_converges fails at clean HEAD (2/2 runs, :3028 'a copied context edit must produce a new image run id'). Keep M3.121 open, scoped to context edits + making that test pass.

## 2026-11-24T19:20 [conductor]

edited fields

## 2026-11-24T19:20 [conductor]

status superseded -> ready

## 2026-11-24T19:20 [conductor]

Owner-relevant split verified by the M3.124 lead: Dockerfile instruction edits now rebuild/fail correctly (M3.115), but editing a file included by COPY does NOT rebuild; M3.115's own live test changed_build_inputs_rebuild_and_restart_converges fails 2/2 on clean HEAD 43cfeff. Fix the build-input digest so per-COPY source content changes the image/step key; the test must pass. Queued behind M3.118 because both edit critical_world_journey.rs.

## 2026-11-24T19:42 [conductor]

status ready -> active

## 2026-11-24T19:55 [lead]

attempt 1/3 started

## 2026-11-24T19:55 [lead]

base b637448ebdeb53810238c0ef55517c4dd8dc02f7 (worktree .worktrees/m3-121)

## 2026-11-24T20:01 [lead]

status active -> done
