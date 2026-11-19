# M2.20 follow-ups (shipped past the release gates)

- **Bench matrix is host-time-bound.** `make bench --check` with the default
  `--num-tests 16 --workers 8` does not finish within 50 minutes on this host
  (a 3-hour ceiling was also not reached). The bounded
  `uv run --no-project poc/run_benchmark.py --check --num-tests 1 --workers 1`
  exits `0`, which proves the pipeline; the default matrix was consciously not
  used as gate evidence. Add a `--scaled`/shorter matrix (or an explicit
  wall-clock budget with a deterministic subset) so CI can run the drift check.
- `docs/support-matrix.md` renderer lacks recipe/test columns
  (`tools/check_support.py` owns the table; recorded by M2.19's critics).
- `critical_slug` and `critical_oci_runtime` flake under fully parallel
  `cargo test --workspace`; both pass in isolation and inside a serialized
  `make check` — the workspace test runner should serialize the heavy
  world-booting suites.
