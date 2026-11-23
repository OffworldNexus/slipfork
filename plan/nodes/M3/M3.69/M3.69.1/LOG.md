# Log M3.69.1

## 2026-11-23T13:09 [lead]

created (task) Forkfile built-in dev + FeatureSet resolution, threaded through world plan & ingress

## 2026-11-23T13:10 [lead]

status ready -> active

## 2026-11-23T13:10 [lead]

attempt 1/3 started

## 2026-11-23T13:10 [lead]

base 1da2cdb4f5e78459741ad10194497905bc9e0bcd

## 2026-11-23T13:17 [lead]

status active -> review

## 2026-11-23T13:19 [lead]

status review -> ready: Triad fix pass: split eval/features.rs, canonical feature index, ingress gating test, DX nits

## 2026-11-23T13:19 [lead]

attempt 2/3 started

## 2026-11-23T13:19 [lead]

base 1da2cdb4f5e78459741ad10194497905bc9e0bcd; triad: arch FAIL (685-LOC eval/mod.rs, duplicated on-walk), tests CHANGES (0%-covered from_forkfile_with, vacuous !mail), ux FAIL (double next:, --mail parses, uncapped remedy); decision: M3.69.2 will rename eval_all_env_with_features->eval_all_env_with and delete shims

## 2026-11-23T13:19 [lead]

status ready -> active

## 2026-11-23T13:26 [lead]

status active -> done

## 2026-11-23T13:55 [lead]

status done -> ready: semantics fix: disabling a group must drop its implications; explicit disable outranks implication

## 2026-11-23T13:55 [lead]

attempt 3/3 started

## 2026-11-23T13:55 [lead]

live prod-check: -F -dev leaves mail active via dev=[mail] because disable removed only dev; fix resolution with blacklist semantics

## 2026-11-23T13:55 [lead]

status ready -> active

## 2026-11-23T13:57 [lead]

semantics fix committed; -dev now drops dev=[mail] implications, explicit enables survive, default members are roots

## 2026-11-23T13:57 [lead]

status active -> done
