# Log M3.166

## 2026-11-25T12:38 [lead]

created (task) Fold resolved build args into image_inputs/plan_epoch so an arg edit invalidates sealed heads

## 2026-11-25T16:53 [conductor]

edited fields

## 2026-11-25T16:57 [conductor]

status ready -> active

## 2026-11-25T16:57 [lead]

status active -> active

## 2026-11-25T16:57 [lead]

attempt 1/3 started

## 2026-11-25T16:57 [lead]

base 1b3c991168cf997e5ebdfe176e68b3318a9f9c43

## 2026-11-25T17:00 [lead]

status active -> review

## 2026-11-25T17:04 [lead]

Fix: build_inputs_digest folds resolved BuildArgs fingerprint; world/inputs.rs owns image_inputs+build_args+INGRESS_SCHEME (single digest owner); plan_epoch/heads.validate inherit it. Live VM (/mnt/sf-m3166/m3166lab, arg PORT = web.port): up port 3000 built web (api cached); port 5173 rebuilt only web's steps, booted python -m http.server 5173, /port.txt=5173; back to 3000 fully cached, /port.txt=3000. make check green (js-test stale-daemon env failure cleared by killing old target/debug daemon); guards clean; Triad arch PASS. Deferred pre-existing gaps (not this node): stage target not folded into digest; image_inputs .ok() swallows Dockerfile errors; build.rs still hand-rolls args bridge.

## 2026-11-25T17:04 [lead]

status review -> done
