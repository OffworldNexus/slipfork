# Log M3.70.4

## 2026-11-23T14:21 [lead]

created (task) Prod-shape runtime: rootfs symlink exec, feature-aware stamp trust & stale crun cleanup

## 2026-11-23T14:21 [lead]

status ready -> active

## 2026-11-23T14:21 [lead]

attempt 1/3 started

## 2026-11-23T14:21 [lead]

base c8237bc8d2ce434a9cce8057a99b1ac9990b4b7d; escalated from M3.70.2 after 3 cheap attempts

## 2026-11-23T15:23 [lead]

expert fixed 3 root causes; local workspace green; critical_guard authorized; live: 3 CLI suites + critical_dockerfile_live 3/3 + pod_runtime 1/1, packaged /usr/bin/slipfork pod cmdlines /app/.venv/bin/python + node build + uvicorn, prod-check 200 and drop -f clean; Triad arch/tests returned empty -> lead self-verified diff/checks

## 2026-11-23T15:23 [lead]

status active -> done

## 2026-11-23T15:24 [lead]

status done -> ready: Triad MAJOR: content-only crun container id can kill a concurrent build

## 2026-11-23T15:24 [lead]

attempt 2/3 started

## 2026-11-23T15:24 [lead]

triad ux: scope sf-<hash> container names per process; add fork rebuild plan row; forkfile read error code/next

## 2026-11-23T15:24 [lead]

status ready -> active

## 2026-11-23T15:40 [lead]

triad fix: sf-<pid>-<hash> containers (foreign-PID isolation test), feature-override fork plan notice, forkfile read remedy; local 318 tests, live dockerfile_live 3/3

## 2026-11-23T15:40 [lead]

status active -> done
