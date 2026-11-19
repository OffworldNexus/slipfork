# Result M3.17 — PoC benchmark + support registry on the Btrfs engine

Status: **implemented** (attempt 3/3 — awaiting the Quad Review Gate)
Branch: `release/0.1.0`
Base: `053596ee33193d5e947e933bf2c0506f366ff3f8`

## Attempt 2 — what changed and why

The attempt-1 Quad gate was right on the BLOCKER: `poc/run_benchmark.py` had re-implemented the
shipped Btrfs probe (`ctypes.CDLL(None)`, `statfs`, its own `BTRFS_SUPER_MAGIC`, its own
`BTRFS_MOUNT_HINT`, and its own nearest-ancestor walk) instead of consuming the sanctioned
`slipfork doctor --json` seam (`slipfork_engine::btrfs` + `crates/slipfork/src/doctor.rs`). A
duplicated probe and a duplicated hint constant can drift from the engine while the harness stays
green. Attempt 2 deletes all of it and gates on the shipped binary. The two MAJORs (drift-blind
hint assertion, M1 provenance pinned in the baseline) and the MINORs (metric naming, test hygiene,
header truth) are fixed in the same pass.

### `poc/run_benchmark.py`

1. **Gate through the shipped CLI.** `require_btrfs(layout, env, project)` runs
   `slipfork --state-dir <run_dir>/state doctor --json` — the exact state directory every later
   CLI call receives — and parses the report. It passes only when `filesystem == "btrfs"` and
   `btrfs_super_magic == true` (the `--json` projection of the text `btrfs_super_magic: ok`) and
   `subvolume_ioctl.status == "ok"` (every world destroy needs the round-trip). Any other failure
   raises with the report's `fixes` joined verbatim, one per line, on stderr; a failure that is
   only `unshare_userns`/`netns` (which the benchmark does not use) is a printed warning. The
   decision comes from the parsed fields, never from the exit status alone.
2. **Nothing runs or is written before the gate.** `bench_child_env` is now a pure mapping; the
   pinned `HOME`/`TMPDIR` directories are created by `prepare_run_state`, which runs *after* the
   gate. `measure_all` builds the env, probes, then wipes, then preflights. Off Btrfs the harness
   creates nothing, wipes nothing, runs no preflight/build, and prints no metric.
3. **Deleted:** `import ctypes`, `_LIBC`, `_STATFS_BUFFER_BYTES`, `statfs_magic`,
   `_nearest_existing`, `BTRFS_SUPER_MAGIC`, `BTRFS_MOUNT_HINT`. No raw `statfs` helper survives;
   the nearest-existing-ancestor resolution is `doctor`'s (`probed_path`), never re-derived.
4. **Metric rename.** `snapshot_ro_ms` → `first_up_ms`, because the window is the whole first `up`.
   The report row and prose now say exactly
   "first `up` — seals the read-only base snapshot and boots (composite)".
   `world_up_ms` and `worktree_fork_ms` are unchanged.
5. **Header truth.** The verb list now reads `doctor`, `snow resolve`, `auth login`, `dag plan`,
   `up`, `down`, `fork`, `exec`, `drop` over `slipforkd` (it really does call `doctor` and
   `down`), the gate paragraph describes `doctor --json`, and no comment mentions a removed helper.
6. **One absolute run directory.** `main` resolves `--run-dir` once, because `doctor` rejects a
   relative `--state-dir` and every later CLI call must receive exactly the directory the gate
   probed.

### `poc/bench-baseline.json`

- Removed `"topology": "m1-host-shared-usr"`: `refresh_baseline` never rewrote it, so `M3.19`
  would have pinned Btrfs-VM numbers under M1 host provenance.
- Reworded `note` so it is true for whatever host pinned it: "Projection of the committed report;
  only metrics whose measurement semantics are stable are pinned, and `make bench-update` re-pins
  them on the host that measured." No key is written but never refreshed: `refresh_baseline`
  rewrites `metrics`, `main` rewrites `source_report_sha256` + `measured`, and `schema` /
  `source_report` / `note` are constant text.
- No `first_up_ms` pin: it has never been measured on a Btrfs host, and inventing a bound would be
  an illegal guess (`M3.19` measures it live and deliberately does not pin it).

### `tools/tests/test_run_benchmark.py`

- The off-Btrfs guard is one `_off_btrfs()` contextmanager that fakes `slipfork doctor --json` at
  the **subprocess boundary**; `XFS_FSTYPE` is one constant.
- The hint assertion now checks the harness prints the fake **binary's** `fixes` verbatim (a
  distinctive `/dev/bench-device /bench-state` line) and asserts
  `not hasattr(bench, "BTRFS_MOUNT_HINT")`, so harness/engine drift cannot stay green.
- New cases: the probe argv is exactly `--state-dir <run_dir>/state doctor --json`; a
  `subvolume_ioctl` failure is fatal; a userns/netns-only failure is a warning; a doctor run with
  no JSON is fatal.
- Fixture hygiene: the composite `first_up_ms` 4x pin is gone from `BASELINE` (the real baseline
  deliberately refuses to pin a composite), `first_up_ms = 1200.0` and `world_up_ms = 800.0` are
  distinct, and the env test asserts the builder writes nothing while `prepare_run_state` creates
  `home` (`0700`) + `tmp`.

### `support/integrations.toml` / `docs/support-matrix.md`

Unchanged and verified: one `btrfs-world-engine` entry with pointers that exist after `M3.14`;
`make support-sync` reported `no demotions` and rewrote `docs/support-matrix.md` byte-identically
(`git status` clean for both files).

## New report keys / table (section 1)

| key | unit | window |
|---|---|---|
| `first_up_ms` (renamed from `snapshot_ro_ms`) | ms | first `up` — seals the read-only base snapshot and boots (composite): `L0` rootfs materialization, streamed seed restore, `freeze_world` seal, writable clone, service boot |
| `world_up_ms` (kept) | ms | second `up` from the sealed base: writable clone + `postgres:18`/app readiness (warm-SQL) |
| `worktree_fork_ms` (kept) | ms | `slipfork fork`: worktree + writable clone of the sealed base |
| `pgdata_logical_mib`, `pgdata_files`, `pg_restore_ms`, `worktree_exec_ms`, `parallel_4_forks_ms`, `dag_warm_plan_ms`, `dag_front_only_plan_ms`, suite keys | unchanged | unchanged |

## Exact non-Btrfs failure transcript (this XFS host)

```
$ uv run --no-project poc/run_benchmark.py --check
run_benchmark: bench state directory '/home/remy/.cache/slipfork/bench/state' is not on Btrfs: `slipfork doctor` probed '/home/remy/.cache/slipfork/bench' and reports filesystem 'xfs' with btrfs_super_magic False
sudo mount -o user_subvol_rm_allowed,noatime /dev/<device> <state_dir>
$ echo $?
2
```

The second line is the shipped `doctor` `fixes[0]` printed verbatim; stdout is empty (0 bytes), the
run directory is not created or wiped, and `preflight` never runs.

## Acceptance evidence (attempt 2)

| # | Command | Exit | Result |
|---|---|---|---|
| 1 | `uv run --no-project --with pytest pytest -q tools/tests/test_run_benchmark.py` | 0 | `35 passed in 0.29s` |
| 2 | `uvx --offline ruff check poc tools` | 0 | `All checks passed!` |
| 3 | `uvx --offline ruff format --check poc tools` | 0 | `13 files already formatted` |
| 4 | `! grep -rniE 'incus\|reflink\|ficlone\|zfs' poc/run_benchmark.py support/integrations.toml` | 0 | no output |
| 5 | `! grep -rn 'ctypes\|statfs' poc/run_benchmark.py` | 0 | no output |
| 6 | `make support-sync` | 0 | `no demotions; every claim matches its evidence`; matrix rewritten byte-identically |
| 7 | `make support-check` | 0 | `OK (10 integration(s); all evidence pointers verified and docs/support-matrix.md in sync)` |
| 8 | `uv run --no-project poc/run_benchmark.py --check; test $? -ne 0` | 0 | script exit 2, `test` 0, stdout 0 bytes |
| 9 | `uv run --no-project poc/run_benchmark.py --check 2>&1 \| grep -n 'user_subvol_rm_allowed,noatime'` | 0 | `2:sudo mount -o user_subvol_rm_allowed,noatime /dev/<device> <state_dir>` |
| — | `uv run --no-project tools/guard.py` | 0 | `guard: OK (501 file(s) checked)` |
| — | `! grep -riE 'option [ab]\|spike [ab]' poc/run_benchmark.py tools/tests/test_run_benchmark.py` | 0 | no output |

Not run here (lead owns the serial full gate): `make check`, `make rs-test`, `make ts-test`.
`--write-report` was not run (the report is regenerated on the Btrfs DO VM by `M3.19`).

## Residual / cross-node notes

- `docs/research/m1-poc-benchmark.md` still carries the M1 rows/header; it is outside this node's
  paths and `M3.19` regenerates it with `--write-report`.
- `first_up_ms` is a composite window, not the bare `BTRFS_IOC_SNAP_CREATE_V2` cost: the shipped
  CLI exposes no isolated seal command. The window is documented in the script docstring, the
  function docstring, the report row, and the report's "What the numbers mean" section.
- `docs/research/btrfs-crun-benchmarks.md` §6 lists "RAM per world" (RSS) as an owner figure; this
  harness does not sample RSS. `M3.19` records it as external/host-only or files a follow-up.

## Attempt 3 — what changed and why

`review-cruft` was right on the MAJOR: attempt 2 re-decided the shipped `doctor` verdict field by
field and kept a degraded "warn and continue" path for `unshare_userns`/`netns`, justified by "the
benchmark does not use namespaces". That justification is false — every measured `up`/`fork`/`exec`
boots a world through `userns`/`netns`/`ipcns`/`pidns`/`mntns`
(`crates/slipfork-engine/src/bin/slipfork-world-init.rs`), so on a Btrfs host with broken
namespaces the harness would have proceeded into a run where every metric must fail. The gate now
honours `doctor`'s single canonical verdict.

1. **One verdict, no fallback path (MAJOR + legality MINOR 1).** `require_btrfs` proceeds only when
   `report["ok"] is True`; anything else prints the report's `fixes` verbatim (one per line, on
   stderr) and raises before `prepare_run_state`, `preflight`, or any metric. The per-field
   `filesystem`/`btrfs_super_magic`/`subvolume_ioctl` loop and the namespace-warning branch and its
   rationale comment are deleted. `_failed_doctor_checks` only renders the failing fields into the
   diagnostic line — it decides nothing; the decision is `ok` alone.
2. **No wedged hang (MINOR 2).** `_run_doctor` passes `timeout=DOCTOR_TIMEOUT_S` (60 s) and raises
   `BenchError` on `TimeoutExpired`; `_slipfork` — which also had none — passes
   `timeout=CLI_TIMEOUT_S` (900 s) and raises `BenchError` on `TimeoutExpired`. The measured CLI
   calls keep a far larger cap because their duration *is* the measurement.
3. **`poc/bench-baseline.json` (MINOR 3).** Deleted `"schema": 1`: nothing read, validated, or
   rewrote it, and no format-version machinery exists anywhere in the file's lifecycle.
4. **Header truth (MINOR 4).** The verb list no longer claims `doctor` runs "over a harness-owned
   `slipforkd`". It now states the gate first: `slipfork doctor --json` gates the host, then
   `snow resolve` / `auth login` / `dag plan` / `up` / `down` / `fork` / `exec` / `drop` drive a
   harness-owned `slipforkd`. The gate paragraph says `ok: true` instead of restating three fields.
5. **Test hygiene (MINOR 5).** Deleted `test_btrfs_host_with_only_namespace_failures_is_a_warning`
   (it pinned semantics `doctor` rejects) and the tautological `assertFalse(layout.exists())` under
   the faked subprocess. Added `test_namespace_failure_fails_closed_like_any_other_doctor_failure`
   (distinctive `fixes` line echoed verbatim) and `test_the_gate_decides_on_doctor_ok_alone`
   (every field `ok`, `ok: false` still fails). Deduped the repeated `prepare_run_state`+`preflight`
   `AssertionError` patch block into `_no_run_state()` and the repeated
   `_off_btrfs()`+`assertRaises(require_btrfs)` scaffold into `BenchTestCase.gate_fails_closed()`.
   The `subvolume_ioctl` fixture now sets `ok=False`, as a real `doctor` report would.

### Exact failure transcript (this XFS host, attempt 3)

```
$ uv run --no-project poc/run_benchmark.py --check
run_benchmark: bench state directory '/home/remy/.cache/slipfork/bench/state' failed `slipfork doctor` (filesystem 'xfs', btrfs_super_magic, subvolume_ioctl 'skipped')
sudo mount -o user_subvol_rm_allowed,noatime /dev/<device> <state_dir>
$ echo $?
2
```

stdout is 0 bytes; the second line is `doctor`'s `fixes[0]` printed verbatim; the run directory is
neither created nor wiped and `preflight` never runs.

## Acceptance evidence (attempt 3)

| # | Command | Exit | Result |
|---|---|---|---|
| 1 | `uv run --no-project --with pytest pytest -q tools/tests/test_run_benchmark.py` | 0 | `37 passed in 0.34s` |
| 2 | `uvx --offline ruff check poc tools && uvx --offline ruff format --check poc tools` | 0 | `All checks passed!` / `13 files already formatted` |
| 3 | `! grep -rniE 'incus\|reflink\|ficlone\|zfs' poc/run_benchmark.py support/integrations.toml` | 0 | no output |
| 4 | `! grep -rn 'ctypes\|statfs' poc/run_benchmark.py` | 0 | no output |
| 5 | `make support-sync && make support-check` | 0 | `no demotions; every claim matches its evidence`; matrix rewritten byte-identically (`git status` clean) / `check_support: OK (10 integration(s); ...)` |
| 6 | `uv run --no-project poc/run_benchmark.py --check; test $? -ne 0` | 0 | script exit 2, stdout 0 bytes, `test` 0 |
| 7 | `uv run --no-project poc/run_benchmark.py --check 2>&1 \| grep -n 'user_subvol_rm_allowed,noatime'` | 0 | `2:sudo mount -o user_subvol_rm_allowed,noatime /dev/<device> <state_dir>` |
| 8 | `uv run --no-project tools/guard.py` | 0 | `guard: OK (500 file(s) checked)` |
| 9 | `uv run --no-project tools/plan.py validate` | 0 | `plan OK` |
| — | `! grep -riE 'option [ab]\|spike [ab]' poc/run_benchmark.py tools/tests/test_run_benchmark.py` | 0 | no output |

Not run here (another worker holds the serial gate): `make check`, `make rs-test`, `make ts-test`.
`--write-report` was not run (the report is regenerated on the Btrfs DO VM by `M3.19`).

