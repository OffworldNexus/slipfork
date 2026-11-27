# Log M3.153

## 2026-11-25T10:10 [conductor]

created (task) Service pods must run as the image's USER (default root) through the same single-owner userns mapping and Docker caps as build steps

## 2026-11-25T10:10 [conductor]

status ready -> active

## 2026-11-25T10:14 [lead]

edited fields

## 2026-11-25T10:14 [lead]

attempt 1/3 started

## 2026-11-25T10:14 [lead]

attempt 1: widen paths to the identity owner's real call sites (pod_env.rs, resources/**, world lib.rs, image runtime.rs) — no heads/daemon edits (M3.140 lock); base 55fcfabda0bd9d6f9d2ac090fbe484f3a0989e48

## 2026-11-25T10:18 [lead]

attempt 2/3 started

## 2026-11-25T10:18 [lead]

attempt 1 partial: identity owner + pod render landed & libs compile; ledger mod became private (regression) and runtime/helpers super::heads import broken; no live test yet

## 2026-11-25T10:19 [lead]

owner addendum: the build/run identity factoring must be explicit and grep-enforced — one cap table + one uid/gid resolver + one /etc/passwd-account owner in slipfork_world::identity; slipfork-image::identity stays a pure error-vocabulary adapter; Identity = who runs inside (uid/gid/caps/home/account), PodIdentity = how it maps into the world userns + pod spec; build RUN and service boot must call the same resolver on the same ImageConfig.user source; review-arch verifies

## 2026-11-25T11:00 [lead]

VM proof (private worktree HEAD 6b095fa + only node files, /mnt/sf-m3153 Btrfs, as slipfork, XDG_RUNTIME_DIR on tmpfs): (a) no-USER image boots uid=0(root) CapEff=0xa80425fb and a real chroot init prints caps-ok; exec -- id -> uid=0 gid=0; exec writes /project/exec-created, host stat 1000:1000; (b) Dockerfile USER 1000:1000 boots uid=1000(app) CapEff=0; (c) USER ghost -> gutter 'the image has no account \`ghost\` in /etc/passwd' + one next:, rc=1. pod_run_live green on VM. Isolated workspace: fmt+clippy --all-targets -D warnings green, cargo test --workspace green except pre-existing M3.155 forkfile golden critical_fmt_migrate (outside paths, HEAD half-landed).

## 2026-11-25T11:00 [lead]

integration note for the epic gate: crates/slipfork-world/tests/pod_run_live.rs (live feature) must join LIVE_SUITES in Makefile and TESTING_PLAN.md — both are M3.131's active paths, so this node did not edit them; ran the suite manually on the VM with SLIPFORK_TEST_ROOT=/mnt/sf-m3153/lab.

## 2026-11-25T11:14 [lead]

attempt 2 (recovered): finished compile, hardened sealed-identity lookup to fail closed on a built image with no image record, typed IdentityFailure cause/remedy (no string re-parse), RESTORED legacy resource-pod env exactly (no invented USER/LOGNAME, empty HOME still gets the in-pod home), resolved PodIdentityMapping once per bundle, trim the lab world on success. Isolated worktree 6b095fa+node files: fmt, clippy --all-targets -D warnings green; cargo test --workspace green except pre-existing M3.155 critical_fmt_migrate golden at HEAD (outside paths). VM live: pod_run_live + critical_pod_runtime + pod_control_lab all green; (a) uid=0 CapEff=0xa80425fb NoNewPrivs=1, chroot init caps-ok, exec -- id uid=0 gid=0, exec-written /project/exec-created host-owned 1000:1000; (b) USER 1000:1000 uid=1000(app) CapEff=0; (c) USER ghost -> gutter cause + remedy, rc=1, one next:.

## 2026-11-25T11:14 [lead]

status active -> done
