# Log M3.182.1

## 2026-11-25T21:45 [lead]

created (task) Split slipfork-world pod_log/puller and route supervisor teardown failures to the journal

## 2026-11-25T21:46 [conductor]

status ready -> active

## 2026-11-25T21:46 [lead]

status active -> active

## 2026-11-25T21:46 [lead]

attempt 1/3 started

## 2026-11-25T21:46 [lead]

base 4173bd79c4975a03e358934f2b33883b16995422

## 2026-11-25T21:50 [lead]

status active -> review

## 2026-11-25T21:51 [lead]

review-arch: 1 MAJOR (journal row at <logs_root>/<world>.log escaped world capture dir/pruning/ replay). Decided <logs_root>/<world>/supervisor.log via append_stream, matching slirp4netns.log; 1 consolidated fix pass dispatched.

## 2026-11-25T21:51 [lead]

scope deviations: crates/slipfork-image/src/builder/mod.rs = 524 LOC already at base 4173bd7 (outside puller** paths); remaining eprintln! in crates/slipfork-world/src are the world_init binary (outside supervisor/**, separate process). Both belong to later M3.182 slices.

## 2026-11-25T21:51 [lead]

review-arch: 1 MAJOR (journal row at <logs_root>/<world>.log escaped the world capture dir used by capture_ids/pruning/logs replay). Decided <logs_root>/<world>/supervisor.log via append_stream, matching slirp4netns.log; 1 consolidated fix pass dispatched.

## 2026-11-25T21:51 [lead]

scope deviations: crates/slipfork-image/src/builder/mod.rs = 524 LOC already at base 4173bd7 (outside puller** paths); remaining eprintln! in crates/slipfork-world/src are the world_init binary (outside supervisor/**, separate process). Both belong to later M3.182 slices.

## 2026-11-25T21:53 [lead]

green: fmt+clippy --workspace -D warnings + cargo test --workspace EXIT=0 (54 suites, 0 failures); guard OK; critical_guard clean; LOC scan clean in scope.

## 2026-11-25T21:53 [lead]

status review -> done
