"""Unit tests for the hermetic, read-only PoC benchmark harness (`poc/run_benchmark.py`)."""

from __future__ import annotations

import importlib.util
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from types import ModuleType
from unittest import mock

REPO_ROOT = Path(__file__).resolve().parents[2]
BASELINE = {
    "source_report": "docs/research/m1-poc-benchmark.md",
    "source_report_sha256": "0" * 64,
    "metrics": {
        "reflink_64k_us": {"value": 397, "max_factor": 4.0},
        "pgdata_logical_mib": {"value": 41.5, "max_factor": 1.15, "min_factor": 1.15},
        "pgdata_files": {"value": 1000, "max_factor": 1.5, "min_factor": 1.5},
        "dag_warm_plan_ms": {"value": 10.89, "max_factor": 4.0},
        "dag_front_only_plan_ms": {"value": 10.7, "max_factor": 4.0},
    },
}


def _load() -> ModuleType:
    path = REPO_ROOT / "poc" / "run_benchmark.py"
    spec = importlib.util.spec_from_file_location("run_benchmark", path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules["run_benchmark"] = module
    spec.loader.exec_module(module)
    return module


bench = _load()


def _metrics(**overrides: object) -> object:
    values: dict[str, object] = {
        "reflink_64k_us": 397,
        "pgdata_logical_mib": 41.5,
        "pgdata_files": 1000,
        "world_up_ms": 1200.0,
        "worktree_fork_ms": 44.4,
        "worktree_exec_ms": 20.0,
        "parallel_4_forks_ms": 400.0,
        "pg_restore_ms": 100.0,
        "dag_warm_plan_ms": 10.89,
        "dag_front_only_plan_ms": 10.7,
        "num_tests": 4,
        "workers": 2,
        "baseline_serial_s": 2.0,
        "baseline_per_test_ms": 500.0,
        "slipfork_cold_parallel_s": 1.0,
        "slipfork_cold_per_test_ms": 250.0,
        "slipfork_warm_pool_s": 0.5,
        "slipfork_warm_per_test_ms": 125.0,
        "cold_speedup_factor": 2.0,
        "warm_speedup_factor": 4.0,
        "seed": 20261115,
    }
    values.update(overrides)
    return bench.BenchmarkMetrics(**values)


class BenchTestCase(unittest.TestCase):
    def setUp(self) -> None:
        # Unit tests keep their scratch trees under the system temp directory; the dedicated
        # tmpfs test below re-enables the real guard.
        patcher = mock.patch.object(bench, "TOPMOST_MOUNT_FORBIDDEN", ())
        patcher.start()
        self.addCleanup(patcher.stop)


class ChildEnvTests(BenchTestCase):
    def test_allowlist_drops_host_leakage_and_pins_home_and_tmpdir(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            run = Path(tmp) / "run"
            run.mkdir()
            host_env = {
                "PATH": "/usr/bin",
                "LC_ALL": "C.UTF-8",
                "HOME": "/home/developer",
                "TMPDIR": "/tmp",
                "PYTHONPATH": "/leak/site-packages",
                "CARGO_HOME": "/home/developer/.cargo",
                "XDG_DATA_HOME": "/home/developer/.local/share",
            }
            with mock.patch.dict(os.environ, host_env, clear=True):
                env = bench.bench_child_env(run)
            self.assertEqual(env["PATH"], "/usr/bin")
            self.assertEqual(env["LC_ALL"], "C.UTF-8")
            self.assertNotIn("PYTHONPATH", env)
            self.assertNotIn("CARGO_HOME", env)
            self.assertNotIn("XDG_DATA_HOME", env)
            self.assertNotIn("LANG", env, "a missing allowlisted variable is omitted")
            self.assertEqual(env["HOME"], str(run / "home"))
            self.assertEqual(env["TMPDIR"], str(run / "tmp"))
            self.assertTrue((run / "home").is_dir())
            self.assertTrue((run / "tmp").is_dir())

    def test_extras_must_be_explicit_slipfork_variables(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            run = Path(tmp) / "run"
            run.mkdir()
            env = bench.bench_child_env(run, {"SLIPFORK_BENCH_SEED_SQL": "[]"})
            self.assertEqual(env["SLIPFORK_BENCH_SEED_SQL"], "[]")
            with self.assertRaises(bench.BenchError):
                bench.bench_child_env(run, {"PYTHONPATH": "/leak"})

    def test_run_directory_on_tmpfs_is_rejected(self) -> None:
        with (
            mock.patch.object(bench, "TOPMOST_MOUNT_FORBIDDEN", ("/tmp/", "/dev/shm/")),
            self.assertRaises(bench.BenchError),
        ):
            bench.prepare_run_state(Path("/tmp/slipfork-bench-never-created"))


class RunStateTests(BenchTestCase):
    def test_stale_run_directory_is_wiped_to_empty(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            run = Path(tmp) / "run"
            (run / "stale").mkdir(parents=True)
            (run / "stale" / "pgdata").write_text("leftover", encoding="utf-8")
            bench.prepare_run_state(run)
            self.assertEqual(list(run.iterdir()), [])

    def test_wipe_failure_aborts_before_any_timing(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            run = Path(tmp) / "run"
            run.mkdir()

            def failing(_: Path) -> None:
                msg = "device busy"
                raise OSError(msg)

            with self.assertRaises(bench.BenchError):
                bench.prepare_run_state(run, remover=failing)

    def test_a_partial_wipe_is_a_hard_error(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            run = Path(tmp) / "run"
            run.mkdir()
            (run / "leftover").write_text("x", encoding="utf-8")

            def noop(_: Path) -> None:
                return None

            with self.assertRaises(bench.BenchError):
                bench.prepare_run_state(run, remover=noop)


class ReadOnlyCheckTests(BenchTestCase):
    def test_check_is_the_default_action_and_write_report_is_the_only_mutator(
        self,
    ) -> None:
        parser = bench.build_parser()
        args = parser.parse_args([])
        self.assertFalse(args.write_report)
        with self.assertRaises(SystemExit):
            parser.parse_args(["--check", "--write-report"])

    def test_check_never_mutates_the_report_on_drift(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "repo"
            report = root / "docs" / "research" / "m1-poc-benchmark.md"
            report.parent.mkdir(parents=True)
            report.write_text("committed report\n", encoding="utf-8")
            baseline_dir = root / "poc"
            baseline_dir.mkdir()
            (baseline_dir / "bench-baseline.json").write_text("{}", encoding="utf-8")
            before = report.read_bytes()
            problems = bench.check_report(root, BASELINE)
            self.assertTrue(problems)
            self.assertEqual(report.read_bytes(), before)

    def test_compare_to_baseline_detects_regressions_and_accepts_in_bounds(
        self,
    ) -> None:
        self.assertEqual(bench.compare_to_baseline(_metrics(), BASELINE), [])
        drift = bench.compare_to_baseline(_metrics(reflink_64k_us=99_999), BASELINE)
        self.assertTrue(any("reflink_64k_us" in problem for problem in drift))

    def test_compare_to_baseline_flags_a_broken_seed(self) -> None:
        drift = bench.compare_to_baseline(_metrics(pgdata_files=5), BASELINE)
        self.assertTrue(any("pgdata_files" in problem for problem in drift))


class ProjectionTests(BenchTestCase):
    def test_markdown_and_html_tables_render_the_same_rows(self) -> None:
        metrics = _metrics()
        rows = bench.suite_rows(metrics)
        markdown = bench.render_markdown_table(bench.SUITE_HEADERS, rows)
        html = bench.render_html_table(bench.SUITE_HEADERS, rows)
        for header in bench.SUITE_HEADERS:
            self.assertIn(header, markdown)
            self.assertIn(header, html)
        for row in rows:
            for cell in row:
                self.assertIn(cell, markdown)
                self.assertIn(cell, html)

    def test_report_is_honest_about_the_historical_topology(self) -> None:
        report = bench.render_report(
            _metrics(), report_date="2026-11-15", host_kernel="6.12", host_arch="x86_64"
        )
        self.assertIn("host-shared-`/usr`", report)
        self.assertIn("not comparable", report)
        self.assertIn("pg_restore", report)
        self.assertIn("seed `20261115`", report)

    def test_json_record_round_trips_every_metric(self) -> None:
        record = json.loads(_metrics().to_json())
        self.assertEqual(record["pg_restore_ms"], 100.0)
        self.assertEqual(record["seed"], 20261115)


if __name__ == "__main__":
    unittest.main()
