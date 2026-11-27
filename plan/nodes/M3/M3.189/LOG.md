# Log M3.189

## 2026-11-25T23:40 [conductor]

created (task) live-test purge aborts with BTRFS_SUBVOL_RDONLY subvolumes: CI exit 2 before suite 11

## 2026-11-25T23:40 [conductor]

status ready -> active

## 2026-11-25T23:40 [lead]

status active -> active

## 2026-11-25T23:40 [lead]

attempt 1/3 started

## 2026-11-25T23:40 [lead]

base a7c9cb6a83623bc5a3313b550f154d370d11c41e

## 2026-11-26T02:26 [lead]

reproduced the CI shape: purge_store()/LIVE_RESET rm -rf a store whose children are sealed BTRFS_SUBVOL_RDONLY -> ~344k EROFS lines + exit 2 before slipfork/critical_status_live

## 2026-11-26T02:26 [lead]

fix: scripts/purge-store.sh enumerates -xdev -depth -inum 256, unseals then deletes deepest-first, rm -rf remainder; silent success, one diagnosing line on failure. Both Makefile purge sites call it; LIVE_COLD_SUITES moves first; per-suite numbered XDG_RUNTIME_DIR slots + stale-daemon reap

## 2026-11-26T02:26 [lead]

VM cold 12G loop btrfs, unprivileged, CARGO_TARGET_DIR=/var/tmp/m3189-target: single make live-test exit 0 16/16 suites 0 EROFS lines; LIVE_RESET=1 exit 0 16/16 after purging 61 subvols/58 sealed; helper alone exit 0 silent store absent; warm store still exit 2 with the one-liner. make check green, review-tests PASS (2 MAJOR nits fixed in one pass)

## 2026-11-26T02:26 [lead]

status active -> review

## 2026-11-26T02:26 [lead]

status review -> done
