# Result — M1.4 Real PostgreSQL 18 + XFS FICLONE + Netns + Playwright Benchmark

- Executed `make bench` (`poc/run_benchmark.py`) against native PostgreSQL 18.7, XFS `ioctl(FICLONE)`, unprivileged `CLONE_NEWUSER|NEWNET|NEWPID`, and headless Chromium.
- Wrote `docs/research/m1-poc-benchmark.md`: 41.5 MiB PG18 cluster forked via `FICLONE`, warm DAG plan in ~1.4 ms, orthogonal front edit keeps all DB snapshots cached, and warm CoW world pool runs 16 browser+SQL checkout tests in 1.96s (122ms/test, 3.12x speedup over serial baseline; empirically proving why warm RAM/pool state beats cold per-test daemon startup).
