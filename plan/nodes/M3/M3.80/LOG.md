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
