# Log M3.150

## 2026-11-25T09:31 [conductor]

created (task) Image steps stall silently after a heavy RUN (seal/record tail): measure, surface a phase row, drop the extra commit

## 2026-11-25T09:31 [conductor]

edited fields

## 2026-11-25T09:31 [conductor]

status ready -> active

## 2026-11-25T09:34 [lead]

status active -> active

## 2026-11-25T09:34 [lead]

attempt 1/3 started

## 2026-11-25T09:34 [lead]

base 40c0fce98bcde77cb5cc759b0d7743b8324b6c20

## 2026-11-25T09:34 [unknown]

edited fields

## 2026-11-25T09:43 [conductor]

edited fields

## 2026-11-25T09:43 [conductor]

edited fields

## 2026-11-25T10:06 [lead]

evidence: owner-shape RUN (500MB+4k files) step 4/6 clone 0.02 exec 1.77 seal 1.42 record 0.00 publish 0.00 = 3.21s; real apt install step 4/6 clone 0.03 exec 63.00 seal 0.07; kernel probe 500MB dirty: SETFLAGS-RO 1.195s, sync alone 0.790s, SETFLAGS after sync 0.022s, SNAP_CREATE_V2 -r from dirty 1.208s => seal is the RUN's writeback, RO-snapshot alternative is not cheaper

## 2026-11-25T10:23 [lead]

VM proof (private tree /home/slipfork/m3150-src @55fcfab + this node, fresh loop-backed Btrfs volume per run, as slipfork, XDG_RUNTIME_DIR=/run/user/1000/m3150-<label>, 110x45 PTY via script -q -e; never do_vm.py sync / ~/demo). Owner shape 'fast heavy RUN' (sh oni.sh && head -c 500M /dev/zero > fat.bin + 4000 files): PRE step 4/6 clone 0.02 exec 1.77 seal 1.42 record 0.00 publish 0.00 = 3.21s, image 7.7s, up 9.15s; POST step 4/6 clone 0.02 exec 1.95 seal 2.15 (another run: exec 2.39 seal 0.73) = 3.9-4.1s, image 8.0-8.7s, up 9.15-9.89s -> up wall time unchanged within noise (base pull dominates); the 0.7-2.2s dead gap became a labelled ticking row

## 2026-11-25T10:23 [lead]

real apt install (python:3.9-slim, RUN apt-get update && apt-get install -y --no-install-recommends postgresql curl): step 4/6 clone 0.03 exec 63.00 seal 0.07 record 0.00 publish 0.00; image 68.2s -> a child that runs long flushes continuously, so its seal is 0.07s; the stall only appears when the child exits fast after writing a lot. Kernel probe on the same volume (500MB + 4000 dirty files): SETFLAGS(RO) 1.195s, sync alone 0.790s, SETFLAGS right after sync 0.022s, SNAP_CREATE_V2 -r from the same dirty source 1.208s, SNAP_CREATE_V2 writable from it 0.209s then SETFLAGS 0.021s => the seal IS the RUN's writeback barrier (flip = 22ms); the one-ioctl RO snapshot is NOT cheaper and adds a subvolume, so the seal path stays untouched (is_sealed/GC/Cache::Cold/fork zero-regression).

## 2026-11-25T10:24 [lead]

landed: TailPhase/PhaseTiming in the snapshot store (Cloning announced before the slot clone, Sealing/Recording/Publishing before each op; serial observer only, parallel no-op), StageRows observer in the builder (builder/mod.rs 449 prod LOC), StepState::Tail on the wire with the phase label owned by world/runtime/frames.rs, live row 'elapsed · sealing' that never resets its clock, args-only ellipsis so the progress cell survives a full-width RUN, one append-only line per phase under NO_COLOR/pipes, one -v note per settled step, honest settled elapsed = clone+exec+seal+record+publish. Not fixable here: the phase word for  needs a  field on ImageFrame::Step (crates/slipfork/src/daemon/image/mod.rs, M3.140's path) — follow-up for the daemon cutover.

## 2026-11-25T10:24 [lead]

correction to the previous line (unquoted backticks were eaten by the shell): the phase word for the docker image build command needs a phase field on ImageFrame::Step in crates/slipfork/src/daemon/image/mod.rs (M3.140's path), so it is a follow-up for the daemon cutover; up/fork are fully covered by this node.
