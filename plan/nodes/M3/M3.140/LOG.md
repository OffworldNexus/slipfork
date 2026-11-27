# Log M3.140

## 2026-11-25T01:49 [planner]

created (task) __daemon cutover: StateDb writer, ConvergeExecutor, delete worlds.json/registry, single-plan drop, ps lineage field

## 2026-11-25T01:54 [conductor]

Spec resolved (specs/03 §7.2/§7.5/§7.6): no transient worlds.json importer — open_writer unlinks it, registry.rs deleted; pre-upgrade worlds are untracked until slipfork up registers <root>, reaps stale pods, strips ino==2 stubs and adopts .forks/* + worlds/* as stopped. Offline ps never writes and omits untracked checkouts. Root deletion is enforced (RESTRICT on both root_id and parent_id + delete_world_row check + engine refuses while forks exist).

## 2026-11-25T03:57 [conductor]

status ready -> active

## 2026-11-25T03:59 [lead]

attempt 1/3 started

## 2026-11-25T03:59 [lead]

status active -> active

## 2026-11-25T03:59 [lead]

base e460aca2bc0756cef52a33787aa46e5dce8d908c

## 2026-11-25T08:57 [conductor]

edited fields

## 2026-11-25T10:28 [conductor]

Known gap from M3.150: docker build cannot show the tail phase word until ImageFrame::Step carries phase in daemon/image/mod.rs (up/fork are covered). Add it while you own daemon/**.

## 2026-11-25T10:37 [lead]

recovery: no daemon edits existed; base 6b095fa90c6431e21fff7cd80686ed2a6abeea43; re-dispatching the cutover

## 2026-11-25T10:37 [lead]

status active -> active

## 2026-11-25T11:20 [lead]

landed: __daemon opens the ForkLedger writer after flock (Migrator::up before bind, SchemaNewer -> 'next: upgrade slipfork'), unlinks worlds.json, deletes daemon/registry.rs; Core holds WorldRow rows + adoption of surviving live pids; daemon/converge.rs is the ConvergeExecutor (observe -> plan_converge, start/up/fork/drop/ps, 3 rounds, SweepOrphanPods, AdoptFork); drop is one plan spliced by delete_world_row via ProjectScope+resolve; lineage from ForkPaths.root/parent; PsWorld.lineage/broken; ImageFrame::Step.phase; client waits 20s and surfaces daemon.log's error/next.

## 2026-11-25T11:20 [lead]

grep gate: zero fork_checkouts/RegistryEntry/registry::/worlds.json/.slipfork/world.json/store_fingerprint/Fingerprint under crates/slipfork/src/daemon; world_id_for_project/PrepFrame::fixed have no src callers (world_id_for_project remains only in slipfork-world's layout API). Residue: shared world_name() still prefers a stale <project>/.slipfork/world.json on a first root registration (src/world/mod.rs, outside this node's lock and being edited by M3.153).

## 2026-11-25T11:20 [lead]

gates on isolated df054e8+slice: cargo fmt --all --check 0; cargo clippy --workspace --all-targets -D warnings green; cargo test -p slipfork green except the pre-existing HEAD regression status_surface.rs:330 the_live_boot_pane_renders_exact_rows_and_bars (one-space boot status line; red on pristine df054e8 with 0 dirty files, owned by M3.152/M3.141, not this slice); critical_fmt_migrate 3/3 on df054e8.

## 2026-11-25T11:20 [lead]

VM proof (private tree, fresh loop btrfs, as slipfork, XDG_RUNTIME_DIR, never do_vm sync/demo): ghost root+creating merci+untracked bonjour/salut(lease origins)+orphan pods+legacy worlds.json -> root tracked, bonjour under root, salut under bonjour (lineage), merci row/checkout/world data reaped, both pod dirs gone, 0 'checkout missing', worlds.json unlinked; 2 restarts byte-identical; drop bonjour -f one plan [1/4]..[4/4] + salut spliced to root + root drop refused with one 'next: slipfork down ghostroot'; newer-schema __daemon and CLI auto-spawn both 'next: upgrade slipfork'.

## 2026-11-25T11:20 [lead]

status active -> done
