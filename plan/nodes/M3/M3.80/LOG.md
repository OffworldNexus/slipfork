# Log M3.80

## 2026-11-24T00:10 [conductor]

created (task) Never touch the user's checkout: drop up-time adopt, copy plain checkouts on fork

## 2026-11-24T00:10 [conductor]

status ready -> active

## 2026-11-24T00:11 [lead]

edited fields

## 2026-11-24T00:11 [lead]

edited fields

## 2026-11-24T00:12 [lead]

attempt 1/3 started

## 2026-11-24T00:12 [lead]

base c77a2b941f5f6f6ad539a73cfd82afcc4232433f (release/0.1.0-fix; M3.79 parallel on disjoint files)

## 2026-11-24T00:14 [lead]

edited fields

## 2026-11-24T00:21 [lead]

INCIDENT: a detached 'tools/do_vm.py sync' (local PID 123650/123653/123656/124233) reset the owner's live /home/slipfork/demo (api/pyproject.toml + uv.lock) while copying the tree; owner killed it and restored numpy+ipython via 'slipfork exec api -- uv add numpy ipython'. Rule from now on: NEVER run do_vm.py sync against the owner VM; live proof uses a scratch /home/slipfork/plaincheck project + a binary from the synced repo's target/ or a scp'd .deb, with an isolated XDG_RUNTIME_DIR/XDG_STATE_HOME. Do not touch ~/demo.

## 2026-11-24T00:22 [lead]

edited fields

## 2026-11-24T00:31 [lead]

a609205: live on 165.232.86.174 (scratch /home/slipfork/plaincheck, isolated XDG_RUNTIME_DIR=/dev/shm/m380-rt, XDG_STATE_HOME=/var/tmp/m380-state, target/debug binary; NEVER do_vm sync, ~/demo untouched): up project_ino 232348->232348, marker_ino unchanged, adopt_siblings=0, same shell still lists .git/.slipfork/Forkfile/marker.txt/run.sh; fork feat-plain copied a plain tree (files+755, parent ino unchanged) with snapshot_ms=82; drop removed it. Live suites on /mnt/slipfork-live: critical_btrfs_live 1/1, critical_world_journey 2/2, critical_fork_live 3/3 (incl. new fork_copies_a_plain_checkout_without_touching_it). make check: my crates/tests green; full-workspace gate fails only in crates/slipfork-lsp/tests/critical_lsp.rs due to M3.84 ff0a209 4-space formatter (passes at c77a2b9, fails at HEAD) - routed to M3.84. Also note specs/03 still says up converts the checkout (arch finding; specs owned by M3.79/M3.81) - follow-up needed.

## 2026-11-24T00:31 [lead]

status active -> done
