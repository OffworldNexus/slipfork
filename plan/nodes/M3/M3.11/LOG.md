# Log M3.11

## 2026-11-16T16:58 [planner]

created (task) Record Spike A measurements (Btrfs + crun) in docs/research and LESSONS

## 2026-11-16T18:11 [lead]

OWNER DIRECTIVE: zero test-scaffolding Cargo features (no feature="test-support"). SubvolumeDriver seam uses pure constructor DI; test fakes only via #[cfg(test)] + tests/common/ or unpublished publish=false testkit crate in [dev-dependencies]. Features are reserved for language/platform modularity.

## 2026-11-16T18:42 [lead]

edited fields

## 2026-11-16T18:42 [lead]

OWNER TERMINOLOGY RULE: (1) write the report at docs/research/btrfs-crun-benchmarks.md (NOT spike-option-a-*), stating the Btrfs+crun architecture directly with zero 'Option A/B' / 'Spike A/B' framing; (2) DELETE docs/research/spike-option-b-kvm-microvm.md outright; (3) scrub 'Option A/B'/'Spike A/B' from docs/knowledge/LESSONS.md; acceptance must include ! grep -riE 'option [ab]|spike [ab]' docs/research/ docs/knowledge/.
