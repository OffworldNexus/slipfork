# Log M2

## 2026-11-14T22:03 [strategist]

created (milestone) snow.yml v2, OSPP v1 & Live Dev/Worktree Worlds (0.2.0)

## 2026-11-15T02:16 [strategist]

edited fields

## 2026-11-15T11:46 [strategist]

edited fields

## 2026-11-15T16:17 [conductor]

OWNER M2/M3 DIRECTIVES: (1) OSPP v1 WebSocket protocol with tpmd/SSH-style challenge-response auth, streaming DB dumps (opt-in for local dev, skipped for hermetic tests), and remote resource descriptors (e.g. shared dev S3 bucket) governed by configurable ResourceStrategy; (2) First-class hook/migration failure debugging (live output, failed world kept inspectable, manual `slipfork hook run` & `slipfork shell` & `slipfork logs`); (3) `slipforkd` runs as current user with port 80 passed via systemd socket activation (`LISTEN_FDS`, plus `--listen` flag for unprivileged tests); (4) Tunnel Weaver 64-char service label truncation (`<prefix>-<short_id>` when over limit); (5) Built-in `slipfork db`, `slipfork redis`, and `slipfork forward` commands; (6) H-009 Option B ratified; (7) Local-only external validation in ~/.cache/slipfork-ext-validation/ (strictly outside git per Confidentiality rule).

## 2026-11-15T16:29 [strategist]

edited fields

## 2026-11-16T03:04 [strategist]

GATE FAILED (4 BLOCKERs; M2 sent back, M2.30..M2.32 filed): (1) bounded 1x1 poc/run_benchmark.py --check times out (RC=143; M2.20 bench2 dir has only incomplete .staging, 0 worlds/snapshots) because Dockerfile installs 1.5GiB Chromium inside L0 rootfs (violating ADR-0009 s2.2) and daemon.rs::ensure_base_snapshot clones an empty dir with 0 freeze_world callers; (2) slipforkd leaves OSPP v1 OsppClient, LayeredSecretProvider, ResourcePlan offer (hardcoded None at daemon.rs:2019), DumpStreamer, and container exec_in_service pg_restore/CHECKPOINT unwired, and rejects missing optional slipfork.toml (daemon.rs:425); (3) JjWorkspaceAdapter has 0 callers in slipforkd while docs/reference/manifest.md:113 falsely claims jj root resolution; (4) docs/index.md & README.md omit M1 host-shared-/usr benchmark topology label (LESSONS.md:16).

## 2026-11-16T06:11 [lead]

M2.30/M2.31/M2.32 committed (0b760ba/541ab37/d6968fc). make check scoped components green, but a full default-thread run is intermittently red on world-boot timeout flakes (critical_slug / critical_oci_runtime) while unrelated poetry python processes peg the host at load ~10; both tests pass in isolation and docs/support/honesty gates are green. Not a code regression; host-pressure flake recorded for the milestone gate.

## 2026-11-16T06:19 [lead]

Verified: RUST_TEST_THREADS=1 make check exit 0 with the full workspace + docs + support gates; the default-thread failure is contention between concurrently booting postgres containers (each world boots one) on a host also loaded by foreign processes. Recommendation for the gate: run rs-test serialized or mark the world-booting suites not-parallel.
