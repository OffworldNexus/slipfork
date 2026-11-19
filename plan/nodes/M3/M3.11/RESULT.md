# Result M3.11 — Btrfs + crun benchmark report, obsolete report deletion, LESSONS scrub (attempt 3)

## Summary

Attempt 2 fixes only the two gate FAILs (evidence hygiene, incomplete cleanup); no figure was re-measured and no number was added. `docs/research/btrfs-crun-benchmarks.md` (92 lines) still records solely the owner-supplied figures (`19.5 ms` read-only snapshot, `11–24 ms` writable clone, `39 ms` to a warm-SQL world, `6.3 MiB` RAM per world) plus the two kernel facts; `docs/research/spike-option-b-kvm-microvm.md` remains deleted (`git rm`, staged); `docs/knowledge/LESSONS.md` carries the two kept Btrfs/IPC facts and no rejected-architecture lesson.

## Finding-by-finding disposition

### CRUFT GATE (attempt 1)

| # | Severity | Finding | Disposition |
|---|---|---|---|
| C1 | MAJOR | `LESSONS.md` still carried microVM lessons (virtiofs snapshot/restore, Cloud Hypervisor kill timings, Firecracker API/snapshot paths) after the report deletion | **Fixed.** Deleted all three bullets per lead ruling (NODE.md invariant 4). Kept every still-true host fact: XFS/reflink + `tmpfs` `EOPNOTSUPP` (line 7), `buildx type=local` ownership + `userxattr` overlay/`pivot_root` (line 18), unprivileged `crun` quirks (line 17), and the generic `pgrep -x`-zombie and `pkill -f`-over-SSH/`--reflink=always` same-filesystem traps (lines 19–20) stripped of hypervisor name/version/timing content |
| C2 | MAJOR | The four figures had three owners; report must be sole owner | **Fixed.** Report has one results table (lines 24–27) and no prose restatement; the only figure outside the table is the required `19.5 ms` timing-window question in `## 6` (line 88). `LESSONS.md` quotes no figure. ADR-0010/design-doc citations are filed on M3.23/M3.8, outside these paths |
| C3 | MINOR | `crun` `config.json` `ipc`-namespace lesson duplicated `LESSONS.md:17` | **Fixed.** Line 22 now states only the `shmget` key `90000` cause and points at `ADR-0010` §6 for the namespace set; no duplicate `crun` mandate |

### IOC + LEGALITY GATE (attempt 1) / attempt-2 fixes

| # | Severity | Finding | Disposition |
|---|---|---|---|
| L1 | MAJOR | Fabricated measurement span ("`freeze_world` end to end: service stop through `BTRFS_IOC_SNAP_CREATE_V2` returning") | **Fixed.** Removed. Metric 1 is stated exactly as the owner record: "read-only snapshot of a world subvolume" (table row 1, method §3.2). `## 6` asks whether the `19.5 ms` includes `freeze_world` service shutdown (line 88) |
| L2 | MAJOR | "`freeze_world` cost is dominated by stopping services, not by data volume" presented as a result | **Fixed.** Moved into the same `## 6` bullet as an explicit open hypothesis with "no split measurement was supplied" (line 88); removed from §4 |
| L3 | MAJOR | Report restated `ADR-0010` §3/§5/§6 as design rules | **Fixed.** §5 Traps now state observed kernel facts and point at the binding §: destroy-RO ordering → §3/§5 (line 75), `--ipc`/`CLONE_NEWIPC` → §6 (line 74), `user_subvol_rm_allowed` → `slipfork doctor`'s `BtrfsProbeReport::user_subvol_rm_allowed` evidence + §4 (line 76), no-nested-subvolumes → §2 (line 77). "one call, no separate mode switch" and "every `crun` `config.json` must include…" are gone |
| L4 | MINOR | warm-SQL presented as measured procedure; harness/`/proc` ownership unstated | **Fixed.** §3.4 labels it "Slipfork's adopted definition", requires an in-repo `M3.19` harness to take readiness from `ReadinessProbe::Exec` (`ADR-0002` §2, never a bespoke poll), and allows external/host-only measurement; §3.7 makes any RSS sampler a `poc/` benchmark adapter, never `/proc` reads from a domain crate; `## 6` records what the owner figure actually timed as unrecorded |
| L5 | MINOR | `LESSONS.md` snapshot/destroy bullet unscoped; duplicate `--ipc` mandate | **Fixed.** Line 21 scopes to `KernelBtrfsDriver`, `crates/slipfork-engine/src/btrfs.rs` (`KernelBtrfsDriver::destroy_subvolume`, `TestTreeDriver` explicitly unaffected); line 22 keeps only the `shmget` `90000` cause |
| L6 | MINOR | "no Btrfs benchmark script exists in this repository yet" | **Fixed.** Now "no Btrfs run of `poc/run_benchmark.py` has been recorded yet" (`poc/run_benchmark.py` exists) |
| L7 | MINOR | `11–24 ms` vs `39 ms` gap explained as fact | **Fixed.** Labelled "presumably … an inference, not a measured decomposition (§6)" in §4 |
| L8 | — | Figure repetition inside the report | **Fixed.** One results table; prose references metrics by number; table is the only point-value owner (`11–24`/`39 ms`/`6.3 MiB` each appear once) |
| L9 | — | Alternative-architecture framing / `[derived]`/`[guessed]` | **Verified absent** (inverted greps below) |

## Per-command acceptance evidence

`NODE.md`'s `# Acceptance` block (invariant 4's mandated microVM-lesson deletion is additionally verified by commands 11–12; the node's acceptance block itself was not extended — `NODE.md` is outside this node's `paths`).

| # | Command | Result |
|---|---|---|
| 1 | `test -f docs/research/btrfs-crun-benchmarks.md` | exit `0` |
| 2 | `test ! -e docs/research/spike-option-b-kvm-microvm.md` | exit `0` (deleted via `git rm`, staged) |
| 3 | `grep -n '19.5' docs/research/btrfs-crun-benchmarks.md` | `24` (table row), `88` (Unverified timing-window question) |
| 4 | `grep -n '11' … \| grep -c 'ms'` | `1` (line 25, clone row) |
| 5 | `grep -n '6.3 MiB' docs/research/btrfs-crun-benchmarks.md` | `27` (table row only) |
| 6 | `grep -n 'CLONE_NEWIPC' docs/research/btrfs-crun-benchmarks.md` | `74` |
| 7 | `grep -n 'BTRFS_SUBVOL_RDONLY' docs/research/btrfs-crun-benchmarks.md` | `35`, `40`, `57`, `75` |
| 8 | `grep -n 'CLONE_NEWIPC' docs/knowledge/LESSONS.md` | `22` |
| 9 | `! grep -riE 'option [ab]\|spike [ab]' docs/research/btrfs-crun-benchmarks.md docs/knowledge/LESSONS.md` | no output, exit `0` |
| 10 | `! grep -rn '\[derived\]\|\[guessed\]' docs/research/btrfs-crun-benchmarks.md docs/knowledge/LESSONS.md` | no output, exit `0` |
| 11 | `! grep -nEi 'virtiofs\|cloud ?hypervisor\|firecracker\|microvm\|api-sock\|api-socket\|VMM' docs/knowledge/LESSONS.md` | no output, exit `0` |
| 12 | `grep -nEi 'virtiofs\|cloud hypervisor\|firecracker\|microvm\|VMM' docs/research/btrfs-crun-benchmarks.md` | no match, exit `1` |
| 13 | `uv run --no-project tools/guard.py` | `guard: OK (480 file(s) checked)`, exit `0` |
| 14 | `make docs-check` | `zensical build --strict`: "No issues found"; `check_docs_honesty: OK (9 published doc(s) match real CLI subcommands)`, exit `0` |
| 15 | `uv run --no-project tools/plan.py validate` | `plan OK`, exit `0` |

## Notes / out of scope

- `git diff --numstat docs/knowledge/LESSONS.md` = `3 3` (three microVM bullets removed; two Btrfs/IPC bullets rescoped; generic traps kept). Repo-wide `docs/design/**` still carries option-choosing wording from the previous attempt's note; M3.23 tracks that repo-wide grep and `docs/design/**` is outside this node's paths.
- No DI blind spot and no host access required: this node records owner measurements only; `SubvolumeDriver`/`BtrfsEngine` wiring is untouched. No `STATUS: blocked`.
- Nothing committed; the report is untracked (`??`) and the microVM report deletion is staged (`D `).

## Attempt 3 — narrow wording pass (IoC confirm + functional reviewer)

Report content only; no figure re-measured, no number changed, `LESSONS.md` byte-identical to attempt 2.

| # | Severity | Finding | Disposition |
|---|---|---|---|
| I1 | MAJOR | `§3.7` invented a clock/process-stat port not defined in `ARCHITECTURE.md` §1 and misstated the engine (adapter modules do read `Instant::now()`/`/proc`) | **Fixed.** Replaced with "no `/proc` or clock reads from a domain crate; timing/RSS sampling lives in the `poc/` or test harness that calls `BtrfsEngine`/`SubvolumeDriver`". No `Clock` port proposed, no `direction-challenge` filed |
| I2 | MINOR | `§5` trap 1 attributed the `crun` `config.json` `ipc` entry to `ADR-0010` §6 (world `--ipc`/`nsenter` only); `§3.6` namespace list had no pointer | **Fixed.** Trap 1 now splits `--ipc` for the world → `ADR-0010` §6 and the `crun` `ipc` entry → `ADR-0009` §2; the repeated list at `§3.6` carries the `ADR-0009` §2 pointer |
| I3 | MINOR | `§3` reproduced raw `BTRFS_IOC_*` for metrics 1–2 without stating the measurement route | **Fixed.** `§3` now states the in-repo route: `BtrfsEngine` round-trips (`create_snapshot`/`freeze_world`, `clone_world`) through the sole `SubvolumeDriver` seam (`ADR-0010` §5), with an ioctl-issuing `poc/` harness classed as external/host-only, mirroring `§3.4`/`§3.7` |
| F1 | MINOR | `§5` trap 3 described `BtrfsProbeReport`/`slipfork doctor` in the present tense (target state; no `BtrfsProbeReport` in `crates/` yet) | **Fixed.** Now "`ADR-0010` §4 specifies that `slipfork doctor` reports …" |
| F2 | MINOR | "(and port-derived keys)" in `§5` trap 1 is not in the owner record | **Fixed.** Dropped; only the supplied `shmget` key `90000` remains |
| F3 | MINOR (optional) | `§6` prose restated the `19.5 ms` figure | **Fixed.** Now "the read-only-snapshot figure"; the results table is the single owner of every figure (`grep -n '19.5'` → line `24` only) |

### Re-run evidence (attempt 3)

| # | Command | Result |
|---|---|---|
| 1 | `test -f docs/research/btrfs-crun-benchmarks.md` | exit `0` (92 lines) |
| 2 | `test ! -e docs/research/spike-option-b-kvm-microvm.md` | exit `0` |
| 3 | `grep -n '19.5' docs/research/btrfs-crun-benchmarks.md` | `24` (table row only — supersedes the attempt-2 `24`, `88`) |
| 4 | `grep -n '11' … \| grep -c 'ms'` | `1` (line `25`) |
| 5 | `grep -n '6.3 MiB' …` | `27` |
| 6 | `grep -n 'CLONE_NEWIPC' …` | `74` |
| 7 | `grep -n 'BTRFS_SUBVOL_RDONLY' …` | `35`, `40`, `57`, `75` |
| 8 | `grep -n 'CLONE_NEWIPC' docs/knowledge/LESSONS.md` | `22` |
| 9 | `! grep -riE 'option [ab]\|spike [ab]' …` | no output, exit `0` |
| 10 | `! grep -rn '\[derived\]\|\[guessed\]' …` | no output, exit `0` |
| 11 | `! grep -nEi 'virtiofs\|cloud ?hypervisor\|firecracker\|microvm\|api-sock\|api-socket\|VMM' docs/knowledge/LESSONS.md` | no output, exit `0` |
| 12 | `uv run --no-project tools/guard.py` | `guard: OK (480 file(s) checked)`, exit `0` |
| 13 | `make docs-check` | `zensical build --strict`: "No issues found"; `check_docs_honesty: OK (9 published doc(s) match real CLI subcommands)`, exit `0` |
| 14 | `uv run --no-project tools/plan.py validate` | `plan OK`, exit `0` |
