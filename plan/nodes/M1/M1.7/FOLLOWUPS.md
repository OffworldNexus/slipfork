# M1.7 Follow-ups (post-Quad-Gate, shipped under the Gate Freeze)

**Gate Freeze rule (conductor, binding):** only `BLOCKER` findings stop the commit. All four
reviewers returned `PASS` with zero blockers; everything below is a known, consciously deferred
non-blocker and ships with `0.1.0`.

**Zero BLOCKERs:** the hermetic repo (`git init -q`, `check=True`), the sanitized child env, the
`XDG_DATA_HOME` isolation (vault lands in `<bench_root>/xdg-data/slipfork/vaults/`, `$HOME` stays
clean), the script-generated report (no fabricated numbers), and the `CHANGELOG.md` claims were all
independently verified by the reviewers.

## Harness hardening (`poc/run_benchmark.py`)

1. **[MAJOR] `bench_child_env` is a denylist.** It strips 8 `GIT_*` vars over the full ambient
   `os.environ`, so host `PG*` (`PGHOST`/`PGPORT`/`PGUSER`/`PGSERVICE`/`PGOPTIONS`),
   `DATABASE_URL`, `SLIPFORK_*`, `TEST_INDEX`, `WORLD_*`, or `FRONT_BUNDLE_HTML` still reaches every
   child and can silently retarget the measured DB/port. Invert to an explicit allowlist
   (`PATH`, `HOME`, `LANG`, `LC_ALL`, `TMPDIR`, `PYTHONPATH`) plus per-call vars — same fail-open
   class as the bug this node fixed.
2. **[MAJOR] `run_cmd(env=None)` keeps ambient inheritance as the default.** Hermeticity is opt-in
   per call site (only the `cargo build` call at ~:1122 omits it). Make `env` required, or set a
   module-level `BENCH_ENV` once so a new call site cannot silently regress.
3. **[MAJOR] `shutil.rmtree(bench_root, ignore_errors=True)` on the pre-run wipe** is the
   hermeticity guarantee, yet it swallows failure: a surviving `state/` makes snapshots be silently
   reused and contaminates timings. Assert the bench root is absent after the wipe (keep
   `ignore_errors` only in the final `finally` cleanup).
4. **[MAJOR] Report header hardcodes host facts** (`XFS (reflink=1)`, `PostgreSQL 18.7`,
   `/usr/bin/google-chrome`, `/home/remy`, `~80%`, `15x–40x`). A "live measured" report that
   silently lies when the host FS/PG/chrome changes → derive from `doctor` JSON (extend it with fs
   type + PG version) or explicitly label the constants as assumed.
5. **[MAJOR] Duplication:** six verbatim `[SLIPFORK_BIN, "--state-dir", …, "drop", name]` blocks
   plus four near-identical `fork` blocks → `drop_world()` / `fork_world()` helpers.
6. **[MINOR] Three divergent Git-sanitisation lists** now exist with no single source of truth:
   `crates/slipfork/src/main.rs` (4 vars + `LC_ALL=C`), the harness (8 vars, no `LC_ALL=C`), and the
   CLI tests (3 vars). Also neither `GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM` nor
   `GIT_CONFIG_NOSYSTEM`/`GIT_TEMPLATE_DIR` is neutralised, so host `~/.gitconfig`
   (`init.templateDir`, `core.hooksPath`, `safe.directory`) still influences the "hermetic"
   `git init`.
7. **[MINOR] `date.today()` + `platform.release()`** make the committed report dirty on any day
   != 2026-11-15 (every `make bench` leaves a spurious diff). Take `--report-date` /
   `SOURCE_DATE_EPOCH` instead of reading the wall clock.
8. **[MINOR] Dead/duplicated harness details:** unused module constant `CHROME_BIN` (~:44);
   `snap_create_ms` measured and stored but never rendered (~:249); `PGDATA` walked twice for bytes
   and file count (~:460); `WORLD_TEST_SCRIPT` written twice to the same path (~:634 and ~:699);
   the report template hardcodes "replays 15,200 rows **16 times**" while `{m.num_tests}` is
   dynamic (~:1092); `--quick-verify` silently ignores `--num-tests`/`--workers` (~:1118).
9. **[MINOR] `RESULT.md` over-claims** that the bench root is always removed; a killed run left
   `~/.cache/slipfork-m1-bench/` behind (that is exactly why follow-up 3 above matters).

## Report / docs consistency

10. **[MINOR] Published figures are hand-synced** across `README.md`, `docs/index.md` and
    `docs/research/m1-poc-benchmark.md` with no mechanical check (`check_docs_honesty.py` verifies
    subcommands, not numbers).
11. **[MINOR] `dag plan` regressed ~8x on an untouched crate** (previous report `1.36 ms`/`1.56 ms`
    → now `10.95 ms`/`10.55 ms`), and the "after editing" row is *faster* than the "cache hit" row
    with no noise note. Add a variance/explanation line, else M2 design assumes the wrong planner
    cost.
12. **[MINOR] Warm-pool result is load-sensitive** (`1.21x` at host load 10.7 vs `3.00x`/`3.05x`
    idle, measured twice). Record `os.getloadavg()` (or median of 3 runs) in the report so a single
    loaded run cannot masquerade as a regression.
13. **[MINOR] "1000 PostgreSQL catalog/heap files"** in the report actually counts *all* files under
    `PGDATA` (WAL, `pg_filenode.map`, control file) → relabel to "files under `PGDATA`".
14. **[MINOR] Stale figures outside this node's `paths`:** `docs/getting-started.md:17`
    (published `doctor` transcript still says `447 us`), `docs/decisions/ADR-0006:13`
    (`429 ms`/`123 ms`), `docs/design/m1-core-and-poc.md:43` (`3.12x`). Re-capture / resync on the
    next docs pass.
15. **[MINOR] `CHANGELOG.md` has no `## Unreleased` section** for post-`0.1.0` work (optional
    Keep-a-Changelog convention).

## Process lesson (operational, highest-value item here)

16. **`make bench` regenerates a *committed* artifact.** Any later re-run of the acceptance command
    (e.g. by a gate reviewer) silently rewrites `docs/research/m1-poc-benchmark.md` with new
    numbers, instantly desynchronising `README.md`/`docs/index.md`. Observed live during this gate:
    a contended run (load avg ~10) produced `warm 5.12 s / 1.21x`, and only a reviewer restoring
    the original bytes kept the tree consistent. The committed artifact must therefore be the
    output of the *final* uncontended run, with the docs synced **after** it — never before
    another bench run. A future `check_docs_honesty` extension comparing cited figures to the
    report would make this mechanical (follow-up 10).
