# Log M3.112

## 2026-11-24T16:14 [conductor]

created (task) Service start failures capture their own output and exit in the run

## 2026-11-24T16:14 [conductor]

edited fields

## 2026-11-24T16:18 [conductor]

edited fields

## 2026-11-24T16:18 [conductor]

Owner live: web crashes immediately (missing blastit shim) yet up waits ~10s for the port. ready.rs::wait_tcp polls the socket with no process-exit check. Requirement: on each readiness poll (or a cheap pod liveness probe), if the service process has exited, fail immediately with service web exited (exit N) + its captured out/err, no full timeout, no silent restart loop; tee the service's stdout/stderr into the run capture so logs -r shows it (see previous log).

## 2026-11-24T16:28 [conductor]

status ready -> active

## 2026-11-24T16:31 [lead]

status active -> active

## 2026-11-24T16:31 [lead]

attempt 1/3 started

## 2026-11-24T16:31 [lead]

base 39e33ea4a7951561796df6f74124daa9d888d3d1

## 2026-11-24T16:38 [lead]

status active -> ready: triad FAIL (arch/ux/tests): portless+unix no tee, duplicate exit vocabulary, cap silent, missing record/fail-fast assertions

## 2026-11-24T16:38 [lead]

status ready -> active

## 2026-11-24T16:38 [lead]

attempt 2/3 started

## 2026-11-24T16:38 [lead]

attempt 2: consolidated fix pass (paths unchanged)

## 2026-11-24T16:41 [lead]

attempt2 live VM: critical_world_journey 2/2 ok (22.27s) incl crash fail-fast/capture/record asserts; pre-existing M3.108 stagefail fixture repaired (Missing-Stage)

## 2026-11-24T16:41 [lead]

attempt2 live manual (scratch /var/lib/slipfork/t/m3112-crash): warm up fails 287ms, stdout '┃ crash-marker-m3112', '✗ start web failed (exit 1)'; logs -r fcf6f9 shows '┃ crash-marker-m3112' + '(exit 1)'; logs web shows marker; down rc=0, world.json absent; scratch world removed. make check green.

## 2026-11-24T16:41 [lead]

status active -> done
