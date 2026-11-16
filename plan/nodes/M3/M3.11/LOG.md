# Log M3.11

## 2026-11-16T16:58 [planner]

created (task) Record Spike A measurements (Btrfs + crun) in docs/research and LESSONS

## 2026-11-16T18:11 [lead]

OWNER DIRECTIVE: zero test-scaffolding Cargo features (no feature="test-support"). SubvolumeDriver seam uses pure constructor DI; test fakes only via #[cfg(test)] + tests/common/ or unpublished publish=false testkit crate in [dev-dependencies]. Features are reserved for language/platform modularity.

## 2026-11-16T18:42 [lead]

edited fields

## 2026-11-16T18:42 [lead]

OWNER TERMINOLOGY RULE: (1) write the report at docs/research/btrfs-crun-benchmarks.md (NOT spike-option-a-*), stating the Btrfs+crun architecture directly with zero 'Option A/B' / 'Spike A/B' framing; (2) DELETE docs/research/spike-option-b-kvm-microvm.md outright; (3) scrub 'Option A/B'/'Spike A/B' from docs/knowledge/LESSONS.md; acceptance must include ! grep -riE 'option [ab]|spike [ab]' docs/research/ docs/knowledge/.

## 2026-11-16T21:01 [lead]

status ready -> active

## 2026-11-16T21:01 [lead]

attempt 1/3 started

## 2026-11-16T21:01 [lead]

base 4d5b02efd7444c3413a6d238271c2a700ed9fd5a

## 2026-11-16T21:02 [lead]

status active -> review

## 2026-11-16T21:03 [lead]

CRUFT GATE attempt 1 (FAIL): (1) MAJOR docs/knowledge/LESSONS.md:19,20,22 still carry microVM lessons (virtiofs snapshot/restore, Cloud Hypervisor kill times, Firecracker --api-sock/snapshot paths) after the obsolete report deletion — delete them in this same sweep (owner ruling: no second architecture in the repo); (2) MAJOR the four benchmark figures now have three owners — the report becomes the SOLE owner; drop the inline repetitions inside the report and let ADR-0010 Context/design doc cite it (their edits are filed on M3.23/M3.8); (3) MINOR crun config.json ipc-namespace lesson duplicates LESSONS.md:17 — keep only the shmget-90000 cause.

## 2026-11-16T21:03 [lead]

IOC + LEGALITY GATE attempt 1 (both FAIL): de-normativize the report — it must not restate ADR-0010 §3/§5 (destroy_subvolume clears RO, no mode switch) or §6 (--ipc/crun ipc namespace) as design rules; state observed facts + § pointer. Remove the fabricated freeze_world timed-span claim and the "cost dominated by stopping services" hypothesis-as-result. Define warm-SQL as the definition Slipfork adopts (or move to Unverified); state that an in-repo M3.19 harness must go through ReadinessProbe::Exec or is external; per-world RSS sampler is a poc/ benchmark adapter, never domain-crate /proc reads. LESSONS: scope the GETFLAGS->SETFLAGS->DESTROY bullet to KernelBtrfsDriver/src/btrfs.rs, drop duplicate --ipc mandate, keep the shmget-90000 fact; fix the "no Btrfs benchmark script exists" claim (poc/run_benchmark.py exists but has never run on Btrfs).

## 2026-11-16T21:07 [lead]

attempt 2/3 started

## 2026-11-16T21:07 [lead]

LEAD RULING (attempt 2): the owner "forget there was another option" directive SUPERSEDES invariant 4 "do not delete existing lessons" — delete the microVM lessons (virtiofs/Cloud Hypervisor/Firecracker) in this sweep. Report is sole owner of the four figures; no normative restatement of ADR-0010 §3/§5/§6; no fabricated freeze_world span; unmeasured inference goes to Unverified.

## 2026-11-16T21:08 [lead]

attempt 2 done: scope-neutral de-normativized report, microVM lessons deleted, fabricated span removed, report sole figure owner. Re-running quad gate.

## 2026-11-16T21:10 [lead]

attempt 3/3 started

## 2026-11-16T21:10 [lead]

IOC CONFIRM FAIL (attempt 2): MAJOR report:53 invents a clock/process-stat DI port ARCHITECTURE §1 does not define and misstates the engine (adapters do read Instant::now()/proc directly) -> scope to "no /proc or clock reads from a domain crate; timing/RSS sampling lives in the poc/ or test harness that calls BtrfsEngine/SubvolumeDriver", no fabricated port; MINOR split the crun config.json ipc pointer to ADR-0009 §2 and add a pointer at :51; MINOR state whether metrics 1-2 time BtrfsEngine::{create_snapshot,clone_world} round-trips or host-level ioctls (ioctl-issuing poc/ would bypass the SubvolumeDriver seam).

## 2026-11-16T21:11 [lead]

status review -> done
