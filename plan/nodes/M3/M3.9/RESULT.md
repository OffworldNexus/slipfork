# Result M3.9 — `docs/design/target-dx.md` rewritten for the Btrfs-only world (attempt 2/3)

## Summary

`docs/design/target-dx.md` now matches the shipped `0.3.0` surfaces byte-for-byte where they exist
(`[resources]` renderer, `list`/`up`/`forward` `--json`, hook-failure rescue block, log paths) and the
`M3.25` rulings where they are target state (`doctor` seam, `--world` selector, three-stage host
contract). Scope: `docs/design/target-dx.md` (+ this RESULT). No commit.

## Disposition of the attempt-2 fix list

| # | Finding | Disposition |
|---|---|---|
| 1 | `[resources]` invented `<kind>.<id>:` prefix | **Fixed.** Every example is `[resources] <id>: <strategy> -> <resolved> (<reason>)` with bare ids (`media`, `main`) and the shipped `snake_case` labels (`crates/slipfork/src/main.rs` `print_event`, `crates/slipfork/src/daemon.rs` `plan_summaries`). |
| 2 | `--json` `world` must be qualified; `--world` accepts three forms | **Fixed.** `list --json` / `forward --json` / `up --json` examples use `"acme:billing"`; §1.6 is the single home for the selector rule (`<project>`, `<project>:<fork>`, `<fork>`, `cwd` fallback, `drop` prefix strip) per `ADR-0005` §3 / `M3.15`. |
| 3 | `doctor` must consume the `M3.25` seam | **Fixed, with one deviation.** `BtrfsProbeReport` evidence (`filesystem` as `doctor`'s label of raw `fstype`, `btrfs_super_magic`, `subvolume_ioctl`, advisory `noatime`) + `HostProbe` (`unshare_userns` with the AppArmor `EPERM` hint, `netns`); the fabricated `(uid=1000:1000)` parentheticals and the host-`crun` probe are gone. **Deviation:** `world_rootfs_crun` is **not** emitted — three lenses raised it as a BLOCKER because `ADR-0010` §4 says `doctor` "emits **no** `crun` field" and `ARCHITECTURE.md` §3.3's `doctor::run`/`HostProbe` have no slot for it. The doc instead states the truth in prose (static `crun` is a world-rootfs asset verified by `BaseImageBuilder`). See cross-node note 1. |
| 4 | Boot claim too broad | **Fixed.** §1.7 now states the three-stage truth: magic-only `ensure_supported` at boot; scratch round-trip only in `doctor`; `BTRFS_IOC_SNAP_DESTROY_V2` `EPERM` at first teardown — with the explicit admission that a half-created world tree is still possible. |
| 5 | Hook/forward log paths | **Fixed.** `log:` is `/home/acme/.cache/slipfork/acme/worlds/acme:billing/logs/api.pre-deploy.log`, i.e. the shipped `Layout::world_logs_dir` join over `world_id`. |
| 6 | Cruft sweep | **Fixed.** `canonical` world label gone; `+` legend now names the completing node per row (incl. `M3.15` apparmor, `M4.1` `share`); `SCM_RIGHTS` framed as the daemon-internal `netns.sock` bridge with zero FDs on the CLI boundary; `crun exec` stated once (§1.4) and pointed at from examples/table; `--world`/`cwd` rule has one home (§1.6); `share` fail-closed rule lives in §1.5 and the table cites it; title/`M4.1` tension resolved by the legend. |
| 7 | Alignment + tags + schema bytes | **Fixed.** Fork renamed `billing` so `acme:billing` (12 chars) fits the shipped `{:<16}` column and the transcript reproduces exactly; first §1.2 engine mention tagged `M3.13`; every shipped `--json` schema keeps its key set/order (only the `world` value changed per fix 2). |

## Quad review gate (5 lenses, two rounds)

| Lens | Round 1 | Round 2 (after revision) |
|---|---|---|
| `ioc-upward` | SPEC_BLIND_SPOT: `world_rootfs_crun` unowned; log tree vs `ADR-0010` §2; rootfs clone source; `ResourcePlan::resolve` cannot print | **PASS** on all six after: field deleted, log join named, seed→seal→base→clone wording, resolver/render split |
| `legality` | FAIL: fabricated `[secrets]` line, `[hook]` argv/order, `getpwuid_r`, `world_rootfs_crun`, `~` in a real path | **All fixed:** `[secrets]` line deleted (never emitted), `[hook]` shows joined argv and follows `[ready]`, `id -un`, field deleted, absolute log path, illustrative-wording caveat on the error prose |
| `cruft` | FAIL: 14 items (duplicate rules, legend, share order, meta sentences, changelog residue, duplicate payload, invented formats) | **PASS** after dedupe; remaining MINORs (`[ready]` format) verified shipped-exact against `print_event` |
| `implementability` | FAIL: `world_rootfs_crun` has no data source; exit predicate; log owner; `subvolume_ioctl` failure rendering | **PASS** after the field was deleted and the required-check list/`failed (errno <n>)` were spelled out |
| `testability` | FAIL: `--json` key list, `[hook]` padding, unasserted stages/selector/AppArmor hint | **Doc fixed** (padding, key-list pointer to `ADR-0010` §4, illustrative markers); the unasserted *node* gaps are cross-node notes 2–5 |

## Acceptance (every NODE.md command, run on `release/0.3.0`)

| # | Command | Result |
|---|---|---|
| 1 | `! grep -rniE 'incus\|reflink\|zfs\|ficlone' docs/design/target-dx.md` | **PASS** — no output, exit 0 |
| 2 | `grep -n 'nsenter --target' docs/design/target-dx.md` | **PASS** — lines 73, 129, 132 |
| 3 | `grep -n 'crun exec' docs/design/target-dx.md` | **PASS** — line 91 (the single service-entry rule) |
| 4 | `grep -n 'slipfork test' docs/design/target-dx.md` | **PASS** — lines 51, 144, 229 |
| 5 | `grep -n 'user_subvol_rm_allowed,noatime' docs/design/target-dx.md` | **PASS** — lines 148, 169, 181, 190 |
| 6 | `make docs-check` | **PASS** — `zensical build --strict`: `No issues found`; `check_docs_honesty: OK (9 published doc(s))` |

Owner terminology gate (scoped to this node's path): `! grep -riE 'option [ab]|spike [ab]' docs/design/target-dx.md` → **PASS** (no output).
Extra: `tools/guard.py` → `guard: OK (483 file(s) checked)`; `tools/plan.py validate` → `plan OK`.

## Cross-node notes for the lead (outside M3.9 `paths`; not edited)

1. **`world_rootfs_crun` conflict (needs an owner decision).** `M3.15/NODE.md` invariant 2 and `M3.16/NODE.md` line 29 require `doctor` to report `world_rootfs_crun`, while `ADR-0010` §4 says `doctor` "emits **no** `crun` field" and `ARCHITECTURE.md` §3.3's `doctor::run(driver, host, state_dir, uid, gid)`/`HostProbe` carry no such input. This doc follows the ADR. To restore the field, amend `ADR-0010` §4 (narrow the ban to *host-binary* probes), add the key to the §4 `--json` set, and give `doctor` a typed input (e.g. the compile-time `rootfs::pin::crun_pin`); otherwise delete it from `M3.15`/`M3.16`.
2. **`BootEvent::Secrets` is dead code.** No shipped path constructs it (`crates/slipfork/src/main.rs:1146` only matches it), so the attempt-1 `[secrets]` transcript line was removed. Owner: `M3.13`/`M3.15` — emit it from `boot_world` or delete the arm.
3. **`M3.15` acceptance should assert the `doctor` contract:** `--json` keys (`probed_path`, `fstype` hex, `fixes`), the all-ok exit `0` case, the AppArmor `EPERM` hint text, and `world_selector_*` CLI cases for the three `<project>`/`<project>:<fork>`/`<fork>` forms.
4. **The `user_subvol_rm_allowed`-missing paths are unfixtured:** stages 2 and 3 of §1.7 need a Btrfs mount *without* the option (owner `M3.10`/`M3.16` provision it) and a `critical_btrfs_live` assertion for `doctor` errno + destroy-time `EPERM` (owner `M3.15`).
5. **Retained log GC has no owner:** `prune` releases world records only, so `<state_dir>/<project>/worlds/<world_id>/logs/` grows unbounded; the doc states this honestly. Decide the reaper (likely `M3.2`).
6. **`up --json` cannot prove readiness ordering:** the schema exposes only `world`/`origin`/`resources`, so "readiness before `[origin]`" is only observable on the human stream (owner `M2.16`).
