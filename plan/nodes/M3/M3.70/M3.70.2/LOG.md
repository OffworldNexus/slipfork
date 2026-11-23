# Log M3.70.2

## 2026-11-23T13:30 [lead]

created (task) Live VM proof: 3 CLI suites + .deb + prod-check -F -dev + demo/feat-auth dev

## 2026-11-23T13:50 [lead]

status ready -> active

## 2026-11-23T13:50 [lead]

attempt 1/3 started

## 2026-11-23T13:50 [lead]

base a8883ac6e18e67f26c848cdd2c206c14d7908ca2; VM access: ssh -i ~/.cache/slipfork/do-vm/id_ed25519 slipfork@165.232.86.174 (XDG_RUNTIME_DIR=/run/user/1000); do_vm.py ssh runs as root; current worlds: demo+feat-auth+tralala running old layout

## 2026-11-23T13:54 [lead]

status active -> ready: blocked on M3.70.1 rollback fixture fix; suites 2/3 green (world_journey 2, status 5)

## 2026-11-23T13:54 [lead]

sync ok; critical_world_journey 2 passed, critical_status_live 5 passed, critical_fork_live 1 passed/1 failed: rollback fixture uses root Dockerfile; .deb/prod-check not started; VM worlds stopped

## 2026-11-23T13:54 [lead]

status ready -> active

## 2026-11-23T13:54 [lead]

attempt 2/3 started

## 2026-11-23T13:54 [lead]

resume after M3.70.1 fixture fix (b1af31c); rerun all 3 suites then .deb + demo sync + prod-check

## 2026-11-23T14:06 [lead]

status active -> ready: blocked on image-engine Dockerfile fidelity; suites 2/3, deb+demo OK

## 2026-11-23T14:06 [lead]

attempt 2: world_journey 2/0, status 5/0, fork_live 1/1 (worker exit 127: base image ENV lost), prod-check frontend build fails (relative COPY ignores WORKDIR); .deb 0.1.0 installed; demo+feat-auth 200 dev; blocker -> M3.70.3

## 2026-11-23T14:16 [lead]

attempt 3: all 3 suites green (2/0, 2/0, 5/0); deb installed 0.1.0 and daemon ExecStart=/usr/share/slipfork/lib/slipfork; demo+feat-auth 200 dev; prod-check worker exits 1 (python /usr/local/bin, app not importable) -> diagnosing feature/stamp/image-workdir evidence before another code fix

## 2026-11-23T14:21 [lead]

edited fields

## 2026-11-23T14:21 [lead]

status ready -> ready: blocked on M3.70.4 prod-shape runtime defects

## 2026-11-23T14:21 [lead]

diagnostic: fresh up -F -dev plans 10 rows (no mailpit, dev off) and cwd /app applied; remaining defects = pod PATH resolves /usr/local/bin/python (rootfs symlink check follows host), fork feature/stamp leak, stale crun container -> escalated M3.70.4 expert

## 2026-11-23T15:23 [lead]

PROVEN by M3.70.4 live run on packaged .deb: world_journey 2/2, fork_live 2/2, status_live 5/5, dockerfile_live 3/3; prod-check 10 rows no mailpit, 200 on / and /api/status, no /project mount, drop -f 203ms; demo+feat-auth 200 dev, tralala stopped

## 2026-11-23T15:23 [lead]

status ready -> done
