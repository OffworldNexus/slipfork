# Log M3.30.6

## 2026-11-18T22:19 [planner]

created (task) Live lab VM proof (165.232.86.174): PTY color/spinner E2E, .localhost :80 demo & before/after llvm-cov audit

## 2026-11-19T00:02 [lead]

carry-over: docs/knowledge/LESSONS.md still references deleted tools/check_stub_adapters.py — update while recording lab lessons

## 2026-11-19T02:25 [lead]

edited fields

## 2026-11-19T02:52 [lead]

status ready -> active

## 2026-11-19T02:52 [lead]

attempt 1/3 started

## 2026-11-19T02:52 [lead]

base c12b84a06ace16b817f756c8092cc45bc514f355

## 2026-11-19T06:16 [lead]

VM proof: do_vm.py test exit 0 (11 suites, 97 tests, 782s wall; per suite: engine-btrfs 32s/1, engine 50s/21, oci 4s/4, edge-live 4s/3, slipfork-btrfs 33s/7, cli 110s/24, l0 35s/8, slug 12s/8, ospp 293s/7, world 110s/7, vcs 10s/7); PTY proof 20/20 (doctor ✓ Ready + 8 ANSI seqs, NO_COLOR/--json 0 escapes, up spinner + http://demo.localhost 200, bare dashboard aligned, edit+restart served, no-arg fork/drop/open --remote/down/up); coverage before->after: LOC 43271->42398, lines 43.77%->43.24%, functions 42.63%, regions 43.22% over the same 16-step journey (slipfork+slipforkd, tests excluded)

## 2026-11-19T06:18 [lead]

status active -> done
