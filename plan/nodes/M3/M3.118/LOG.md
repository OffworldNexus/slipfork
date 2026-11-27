# Log M3.118

## 2026-11-24T16:48 [conductor]

created (task) Dev services must run from the live checkout, not the image copy

## 2026-11-24T16:48 [conductor]

status ready -> ready

## 2026-11-24T16:49 [conductor]

edited fields

## 2026-11-24T16:49 [conductor]

OWNER CORRECTION: do not hardcode /project. Derive the live mount/working directory from the Dockerfile: parse the final stage's WORKDIR and the COPY destination(s) of the build context; mount <project>/<image context> at that detected path (e.g. WORKDIR /app + COPY . . => bind the context dir at /app and run there). Keep an explicit WORKDIR that already points under the dev mount. Only when the final WORKDIR is / (no WORKDIR) can the rootfs root not be overlaid: use a dedicated mount (e.g. /project) and run the service there, documented as the fallback. Never bind over /. -F -dev keeps the image copy. Spec must state that Dockerfile WORKDIR is honoured in dev.

## 2026-11-24T16:50 [conductor]

edited fields

## 2026-11-24T16:50 [conductor]

ADDITIONAL OWNER RULE: if the final stage's effective WORKDIR is still / (no WORKDIR instruction), REFUSE to boot a dev world. No fallback mount, no guessing. Fail closed with one line + next: set WORKDIR to the directory that holds the copied context (e.g. WORKDIR /app) in the dev stage. -F -dev is unaffected (pure Docker semantics). Tests must cover: no WORKDIR + dev => refusal naming the Dockerfile stage and the next command; WORKDIR /app => live context mounted at /app; WORKDIR under /project => kept.

## 2026-11-24T19:04 [conductor]

status ready -> active

## 2026-11-24T19:06 [lead]

status active -> active

## 2026-11-24T19:06 [lead]

attempt 1/3 started

## 2026-11-24T19:06 [lead]

base 43cfeff3ef4e3405aac45a648e9417fb23edbc75

## 2026-11-24T19:42 [lead]

VM fresh Btrfs loop (/mnt/sf-m3118, clean 43cfeff+M3.118 tree): no-WORKDIR dev refuses 1 line+next (no world.json); WORKDIR /code pwd=/code, exec+shell see host edits instantly; WORKDIR /project/api kept pwd/live; -F -dev pwd=/code bakes image copy (host edit invisible); critical_world_journey 4/4, make check EXIT=0, guard OK. Triad: tests PASS, arch BLOCKER (ARG/ENV) + ux findings fixed; runtime now uses BuiltImage.workdir as single cwd owner with fail-closed non-absolute/empty-overlay guard.

## 2026-11-24T19:42 [lead]

status active -> review

## 2026-11-24T19:42 [lead]

status review -> done
