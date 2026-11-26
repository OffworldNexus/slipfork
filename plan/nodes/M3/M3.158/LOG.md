# Log M3.158

## 2026-11-25T11:21 [conductor]

created (task) Root registration still reads a stale world record on first launch (residue from M3.140)

## 2026-11-25T12:31 [conductor]

status ready -> active

## 2026-11-25T12:31 [lead]

attempt 1/3 started

## 2026-11-25T12:31 [lead]

base c752423cf61bdbfbd775307a1c066e3f1386dfef

## 2026-11-25T12:31 [lead]

edited fields

## 2026-11-25T12:31 [lead]

edited fields

## 2026-11-25T12:32 [lead]

VM proof constraints: tools/do_vm.py sync is BANNED (wiped owner ~/demo twice). Live proof = private VM tree /home/slipfork/m3158 at HEAD, private CARGO_TARGET_DIR, copy only node files, build there, run world under private SLIPFORK_TEST_ROOT/store with XDG_RUNTIME_DIR on tmpfs as project user; never touch ~/demo or existing worlds/stores; do_vm.py ssh for commands only.

## 2026-11-25T12:58 [lead]

done: WorldIdentities owns identity (requested -> ledger row by checkout_path -> .forks id -> slug); WorldState/state_path/recorded_world/write_state and world_id_for_project deleted; stale record settled (silent+removed when unregistered, fail-closed one line with next: rm .slipfork/world.json when it disagrees with a row); ledger readers fail closed instead of .ok(); core log_store single lookup; tree skips underivable fork ids; strip_ansi kept local so M3.158 does not depend on M3.164. Proof: clean worktree @3877acb+slice: fmt/clippy -D warnings green, 39 world:: unit tests, slipfork-world suite; private Btrfs VM (no do_vm sync, ~/demo untouched): critical_world_journey stale_world_record... 24s cold pass 4x, live up/up/conflict/down proof = single row, record removed silently, remedy clears conflict, 61ms idle second up, no orphan pods/worlds.

## 2026-11-25T12:58 [lead]

status active -> done
