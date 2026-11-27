# Log M3.129

## 2026-11-24T20:44 [conductor]

created (task) fork rebuilds every image: build-context digest includes VCS metadata, so a branch switch invalidates the parent stamp

## 2026-11-24T20:44 [conductor]

status ready -> active

## 2026-11-24T20:45 [lead]

status active -> active

## 2026-11-24T20:45 [lead]

attempt 1/3 started

## 2026-11-24T20:45 [lead]

base d42b8ae7575a98243a503cc227f62f5ec6ef7e37

## 2026-11-24T20:48 [lead]

EVIDENCE (VM private tree /mnt/sf-m3129, pre-fix bin): root-context image (image app ., Dockerfile COPY . ., WORKDIR /code). Parent stamp world_epoch=586aa192bd15991f4b97bd74; fork feat stamp world_epoch=db9c61c057c6b9a49aeba009 (mismatch). Context entry list (canonical reserved filter) parent vs fork: content files byte-identical. Raw .git diff = exactly .git/HEAD, .git/index, .git/logs/HEAD. Fork boot emitted built 5 steps for image app. Cause confirmed: BuildContext::reserved omits .git -> branch switch changes digest -> plan_epoch mismatch -> full rebuild.

## 2026-11-24T21:04 [lead]

attempt 2/3 started

## 2026-11-24T21:04 [lead]

addendum (owner report): repeated `fork <name>` emits a snapshot row then raw could not snapshot ... File exists (os error 17); refuse in preflight before any row with typed fork-exists + one working next.

## 2026-11-24T21:11 [lead]

plan.py rejected a top-level M3.130 id and created a stray child M3.129.1; that child is superseded and this node is now a container (kind epic) only to satisfy the tree invariant

## 2026-11-24T21:11 [lead]

status active -> review

## 2026-11-24T21:11 [lead]

edited fields

## 2026-11-24T21:11 [lead]

attempt 3 done: preflight fork-exists (registry/.forks/fork world/clones) before any row; next=slipfork drop <name> -f (plain drop is refused for a live dirty checkout). Owner-shape live: duplicate refused (no plan rows, no os error 17), drop -f + refork cached. Live tests 2/2 on VM; make check EXIT=0; guard OK.

## 2026-11-24T21:12 [lead]

status review -> done

## 2026-11-24T21:12 [lead]

COMMIT 0e03d00. Triad PASS (arch/tests/ux; minors discarded: clone-enumeration helper outside node paths, check order, exact-message equality). make check EXIT=0, guard OK, image 48+11+1, VM live fork journeys 2/2.
