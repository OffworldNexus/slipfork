"""Unit tests for the hermetic, read-only PoC benchmark harness (`poc/run_benchmark.py`)."""

from __future__ import annotations

import hashlib
import importlib.util
import io
import json
import os
import socket
import subprocess
import sys
import tempfile
import unittest
from contextlib import contextmanager, redirect_stderr, redirect_stdout
from pathlib import Path
from types import ModuleType
from unittest import mock

REPO_ROOT = Path(__file__).resolve().parents[2]
BASELINE = {
    "source_report": "docs/research/m1-poc-benchmark.md",
    "source_report_sha256": "0" * 64,
    "metrics": {
        "pgdata_logical_mib": {"value": 41.5, "max_factor": 1.15, "min_factor": 1.15},
        "pgdata_files": {"value": 1000, "max_factor": 1.5, "min_factor": 1.5},
        "dag_warm_plan_ms": {"value": 10.89, "max_factor": 4.0},
        "dag_front_only_plan_ms": {"value": 10.7, "max_factor": 4.0},
    },
}

# The off-Btrfs `fixes` line is *the shipped binary's output*, faked at the subprocess boundary.
# A distinctive device/state pair proves the harness prints whatever `doctor` emits verbatim
# instead of a local copy of the mount hint.
DOCTOR_FIXES = (
    "sudo mount -o user_subvol_rm_allowed,noatime /dev/bench-device /bench-state",
    "install util-linux (`unshare`) and retry",
)
NAMESPACE_FIX = "install the userns AppArmor profile and retry"
XFS_FSTYPE = "0x58465342"


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
        "first_up_ms": 1200.0,
        "pgdata_logical_mib": 41.5,
        "pgdata_files": 1000,
        "world_up_ms": 800.0,
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


def _doctor_report(**overrides: object) -> dict:
    """The shipped `slipfork doctor --json` shape (`crates/slipfork/src/doctor.rs`)."""
    report: dict[str, object] = {
        "ok": False,
        "uid": 1000,
        "gid": 1000,
        "state_dir": "/bench/state",
        "probed_path": "/bench",
        "fstype": XFS_FSTYPE,
        "filesystem": "xfs",
        "btrfs_super_magic": False,
        "subvolume_ioctl": {"status": "skipped"},
        "noatime": False,
        "unshare_userns": {"status": "ok"},
        "netns": {"status": "ok"},
        "fixes": list(DOCTOR_FIXES),
    }
    report.update(overrides)
    return report


def _btrfs_report(**overrides: object) -> dict:
    base: dict[str, object] = {
        "ok": True,
        "filesystem": "btrfs",
        "fstype": "0x9123683e",
        "btrfs_super_magic": True,
        "subvolume_ioctl": {"status": "ok"},
        "fixes": [],
    }
    base.update(overrides)
    return _doctor_report(**base)


@contextmanager
def _doctor(report: dict, returncode: int = 1):
    """Fake the shipped `slipfork doctor --json` at the subprocess boundary."""
    result = subprocess.CompletedProcess(
        args=["slipfork", "doctor", "--json"],
        returncode=returncode,
        stdout=json.dumps(report),
        stderr="",
    )
    with mock.patch.object(bench.subprocess, "run", return_value=result) as run:
        yield run


@contextmanager
def _off_btrfs():
    """Fake an XFS host's fail-closed `slipfork doctor --json` report."""
    with _doctor(_doctor_report()) as run:
        yield run


@contextmanager
def _no_run_state():
    """Prove the host gate runs before the pre-run wipe and before preflight."""
    with (
        mock.patch.object(
            bench,
            "prepare_run_state",
            side_effect=AssertionError(
                "the run state must not be wiped on a rejected host"
            ),
        ),
        mock.patch.object(
            bench,
            "preflight",
            side_effect=AssertionError("preflight must not run on a rejected host"),
        ),
    ):
        yield


class BenchTestCase(unittest.TestCase):
    def setUp(self) -> None:
        # Unit tests keep their scratch trees under the system temp directory; the dedicated
        # tmpfs test below re-enables the real guard.
        patcher = mock.patch.object(bench, "TOPMOST_MOUNT_FORBIDDEN", ())
        patcher.start()
        self.addCleanup(patcher.stop)

    def gate_fails_closed(self, report: dict) -> tuple[str, mock.Mock]:
        """Run the host gate against a faked `doctor` report; return its message and the call."""
        with tempfile.TemporaryDirectory() as tmp:
            layout = Path(tmp) / "bench"
            with _doctor(report) as run, self.assertRaises(bench.BenchError) as raised:
                bench.require_btrfs(layout, {}, Path(tmp))
        return str(raised.exception), run


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
            self.assertFalse(
                (run / "home").exists(),
                "building the env must write nothing before the host gate",
            )
            bench.prepare_run_state(run)
            self.assertEqual((run / "home").stat().st_mode & 0o777, 0o700)
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
            self.assertEqual(
                sorted(path.name for path in run.iterdir()), ["home", "tmp"]
            )
            self.assertEqual(list((run / "home").iterdir()), [])
            self.assertEqual(list((run / "tmp").iterdir()), [])

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


class BtrfsProbeTests(BenchTestCase):
    def test_doctor_gate_probes_the_state_directory_the_harness_measures(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            layout = Path(tmp) / "bench"
            with _doctor(_btrfs_report()) as run:
                bench.require_btrfs(layout, {}, Path(tmp))
            self.assertEqual(
                run.call_args.args[0],
                [
                    str(bench.SLIPFORK_BIN),
                    "--state-dir",
                    str(layout / "state"),
                    "doctor",
                    "--json",
                ],
            )
            self.assertEqual(run.call_args.kwargs["timeout"], bench.DOCTOR_TIMEOUT_S)

    def test_non_btrfs_doctor_report_fails_closed_with_its_verbatim_fixes(self) -> None:
        message, _ = self.gate_fails_closed(_doctor_report())
        lines = message.splitlines()
        for fix in DOCTOR_FIXES:
            self.assertIn(fix, lines, "every `doctor` fix is printed verbatim")
        self.assertIn("filesystem 'xfs'", message)
        self.assertFalse(
            hasattr(bench, "BTRFS_MOUNT_HINT"),
            "the mount hint belongs to the shipped binary, never to this harness",
        )

    def test_the_gate_decides_on_doctor_ok_alone(self) -> None:
        message, _ = self.gate_fails_closed(
            _btrfs_report(ok=False, fixes=[DOCTOR_FIXES[0]])
        )
        self.assertIn(DOCTOR_FIXES[0], message.splitlines())

    def test_namespace_failure_fails_closed_like_any_other_doctor_failure(self) -> None:
        # Every measured `up`/`fork`/`exec` boots a world through userns/netns, so `doctor`'s
        # `ok: false` for a broken namespace must stop the run, never warn and continue.
        report = _btrfs_report(
            ok=False,
            unshare_userns={
                "status": "failed",
                "detail": "unshare: Operation not permitted",
            },
            netns={"status": "skipped"},
            fixes=[NAMESPACE_FIX],
        )
        message, _ = self.gate_fails_closed(report)
        self.assertIn(NAMESPACE_FIX, message.splitlines())
        self.assertIn("unshare_userns 'failed'", message)
        self.assertIn("netns 'skipped'", message)

    def test_btrfs_host_with_a_failed_subvolume_ioctl_fails_closed(self) -> None:
        report = _btrfs_report(
            ok=False,
            subvolume_ioctl={"status": "failed", "errno": 1, "hint": DOCTOR_FIXES[0]},
            fixes=[DOCTOR_FIXES[0]],
        )
        message, _ = self.gate_fails_closed(report)
        self.assertIn("subvolume_ioctl 'failed'", message)
        self.assertIn(DOCTOR_FIXES[0], message)

    def test_wedged_doctor_times_out_instead_of_hanging_the_harness(self) -> None:
        expired = subprocess.TimeoutExpired(
            cmd=["slipfork", "doctor", "--json"], timeout=bench.DOCTOR_TIMEOUT_S
        )
        with (
            tempfile.TemporaryDirectory() as tmp,
            mock.patch.object(bench.subprocess, "run", side_effect=expired) as run,
            self.assertRaises(bench.BenchError) as raised,
        ):
            bench.require_btrfs(Path(tmp) / "bench", {}, Path(tmp))
        self.assertEqual(run.call_args.kwargs["timeout"], bench.DOCTOR_TIMEOUT_S)
        self.assertIn("did not answer within", str(raised.exception))

    def test_doctor_without_json_is_fatal(self) -> None:
        result = subprocess.CompletedProcess(
            args=["slipfork", "doctor", "--json"],
            returncode=1,
            stdout="",
            stderr="state dir must be absolute",
        )
        with (
            tempfile.TemporaryDirectory() as tmp,
            mock.patch.object(bench.subprocess, "run", return_value=result),
            self.assertRaises(bench.BenchError) as raised,
        ):
            bench.require_btrfs(Path(tmp) / "bench", {}, Path(tmp))
        self.assertIn("state dir must be absolute", str(raised.exception))

    def test_check_exits_non_zero_with_the_doctor_fixes_and_prints_no_metric(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            run_dir = Path(tmp) / "bench"
            stdout, stderr = io.StringIO(), io.StringIO()
            with (
                _off_btrfs(),
                _no_run_state(),
                redirect_stdout(stdout),
                redirect_stderr(stderr),
            ):
                code = bench.main(["--check", "--run-dir", str(run_dir)])
        self.assertNotEqual(code, 0)
        self.assertEqual(stdout.getvalue(), "", "no fabricated metric may be printed")
        for fix in DOCTOR_FIXES:
            self.assertIn(fix, stderr.getvalue().splitlines())
        self.assertFalse(run_dir.exists(), "the fail-closed run creates nothing")


class BtrfsMeasurementTests(BenchTestCase):
    def test_first_up_is_the_composite_seal_and_boot_window(self) -> None:
        summary = {"origin": "http://acme.localhost", "resources": []}
        with mock.patch.object(bench, "_slipfork_json", return_value=summary) as fake:
            elapsed, measured = bench.measure_up_ms(
                Path("/run"), {}, Path("/run/project")
            )
        fake.assert_called_once_with(Path("/run"), {}, Path("/run/project"), "up")
        self.assertEqual(measured, summary)
        self.assertGreaterEqual(elapsed, 0.0)

    def test_fork_is_the_writable_clone_window(self) -> None:
        with mock.patch.object(
            bench, "_fork", return_value="http://acme.localhost"
        ) as fake:
            elapsed = bench.measure_fork_ms(
                Path("/run"), {}, Path("/run/project"), "fork-probe"
            )
        fake.assert_called_once_with(
            Path("/run"), {}, Path("/run/project"), "fork-probe"
        )
        self.assertGreaterEqual(elapsed, 0.0)

    def test_measure_all_fails_closed_before_any_measurement_off_btrfs(self) -> None:
        with (
            tempfile.TemporaryDirectory() as tmp,
            _off_btrfs(),
            _no_run_state(),
            mock.patch.object(
                bench,
                "_slipfork_json",
                side_effect=AssertionError("no CLI call may run off Btrfs"),
            ),
            self.assertRaises(bench.BenchError),
        ):
            bench.measure_all(1, 1, 20261115, Path(tmp) / "bench")

    def test_refresh_baseline_drops_retired_metrics_and_updates_values(self) -> None:
        baseline = {
            "metrics": {
                "first_up_ms": {"value": 1.0, "max_factor": 4.0},
                "retired_metric": {"value": 2.0, "max_factor": 4.0},
            }
        }
        refreshed = bench.refresh_baseline(baseline, _metrics(first_up_ms=9.0))
        self.assertEqual(set(refreshed["metrics"]), {"first_up_ms"})
        self.assertEqual(refreshed["metrics"]["first_up_ms"]["value"], 9.0)
        self.assertEqual(refreshed["metrics"]["first_up_ms"]["max_factor"], 4.0)


class ReadOnlyCheckTests(BenchTestCase):
    def test_check_is_the_default_action_and_write_report_is_the_only_mutator(
        self,
    ) -> None:
        parser = bench.build_parser()
        self.assertEqual(parser.parse_args([]).action, "check")
        self.assertEqual(parser.parse_args(["--check"]).action, "check")
        self.assertEqual(parser.parse_args(["--write-report"]).action, "write_report")
        with self.assertRaises(SystemExit):
            parser.parse_args(["--check", "--write-report"])

    def test_teardown_always_stops_the_daemon_and_seed_server(self) -> None:
        daemon = mock.Mock()
        server = mock.Mock()
        failing = iter(
            [
                bench.BenchError("down failed"),
                bench.BenchError("drop failed"),
            ]
        )

        def fake_slipfork(*args: object, **kwargs: object) -> None:
            raise next(failing)

        with mock.patch.object(bench, "_slipfork", side_effect=fake_slipfork):
            problems = bench._teardown(
                Path("/run"), {}, Path("/run/project"), daemon, server
            )
        daemon.stop.assert_called_once_with()
        server.stop.assert_called_once_with()
        self.assertEqual(problems, ["down failed", "drop failed"])

    def test_teardown_reports_a_daemon_stop_failure_after_releasing_worlds(
        self,
    ) -> None:
        daemon = mock.Mock()
        daemon.stop.side_effect = bench.BenchError("leftover world processes")
        server = mock.Mock()
        with mock.patch.object(bench, "_slipfork", return_value=None):
            problems = bench._teardown(
                Path("/run"), {}, Path("/run/project"), daemon, server
            )
        server.stop.assert_called_once_with()
        self.assertEqual(problems, ["leftover world processes"])

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
        drift = bench.compare_to_baseline(_metrics(dag_warm_plan_ms=99_999.0), BASELINE)
        self.assertTrue(any("dag_warm_plan_ms" in problem for problem in drift))

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
        self.assertIn(".localhost", report)
        self.assertIn("host** headless Chromium (`CHROME_BIN`)", report)
        self.assertIn("shared **host** browser pool", report)

    def test_report_names_the_btrfs_snapshot_and_clone_rows(self) -> None:
        report = bench.render_report(
            _metrics(), report_date="2026-11-17", host_kernel="6.12", host_arch="x86_64"
        )
        self.assertIn("Btrfs (`slipfork doctor`: `filesystem: btrfs`", report)
        self.assertIn(
            "first `up` — seals the read-only base snapshot and boots (composite)",
            report,
        )
        self.assertIn("writable clone", report)
        self.assertIn("`BtrfsEngine`", report)
        self.assertNotIn("reflink ioctl", report)

    def test_json_record_round_trips_every_metric(self) -> None:
        record = json.loads(_metrics().to_json())
        self.assertEqual(record["pg_restore_ms"], 100.0)
        self.assertEqual(record["first_up_ms"], 1200.0)
        self.assertEqual(record["world_up_ms"], 800.0)
        self.assertEqual(record["seed"], 20261115)


class TopologyTests(BenchTestCase):
    def test_app_image_is_lean_with_zero_in_world_browser(self) -> None:
        dockerfile = bench.DOCKERFILE
        self.assertIn("FROM python:3.12-slim-bookworm AS app", dockerfile)
        self.assertIn('"psycopg[binary]"', dockerfile)
        lowered = dockerfile.lower()
        self.assertNotIn("playwright", lowered)
        self.assertNotIn("chromium", lowered)

    def test_components_api_serves_real_http_on_the_declared_internal_port(
        self,
    ) -> None:
        snow = bench.SNOW_YML
        self.assertIn("internal_ports: [8000]", snow)
        self.assertIn('prefix: "/"', snow)
        self.assertIn("exec python3 api/app.py", snow)
        self.assertIn("resources::app.url", snow)

    def test_browser_driver_runs_on_the_host_against_a_localhost_origin(self) -> None:
        script = bench.WORLD_TEST_SCRIPT
        self.assertIn("SLIPFORK_BENCH_CHROME", script)
        self.assertIn(".localhost", script)
        self.assertNotIn(
            "psycopg", script, "in-world DB access must go through the app HTTP API"
        )

    def test_pairing_url_keeps_the_handle_only_in_the_fragment(self) -> None:
        server = bench.SeedOsppServer()
        try:
            url = server.pairing_url
            before, fragment = url.split("#", 1)
            self.assertNotIn("handle=", before)
            self.assertEqual(fragment, f"handle={server.handle_hex}")
            self.assertEqual(len(server.handle_hex), 64)
            self.assertTrue(before.startswith("http://127.0.0.1:"))
        finally:
            server.stop()

    def test_seed_server_describes_a_zstd_pg_custom_dump(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            archive = Path(tmp) / "seed.pgdump.zst"
            archive.write_bytes(b"zstd-frame")
            offer = bench.SeedDumpOffer(
                db_id="shop",
                compressed=archive,
                sha256="a" * 64,
                compressed_bytes=archive.stat().st_size,
            )
            server = bench.SeedOsppServer()
            try:
                self.assertEqual(server._describe(), {})
                server.serve_dump(offer)
                self.assertEqual(
                    server._describe(),
                    {
                        "shop": {
                            "db_id": "shop",
                            "sha256_compressed": "a" * 64,
                            "compressed_bytes": len(b"zstd-frame"),
                            "format": "pg_custom_zstd",
                        }
                    },
                )
            finally:
                server.stop()

    def test_faster_than_pinned_latency_is_not_drift_but_a_pinned_floor_is(
        self,
    ) -> None:
        baseline = {
            "first_up_ms": {"value": 1200.0, "max_factor": 4.0},
            "pgdata_files": {"value": 1000, "max_factor": 1.5, "min_factor": 1.5},
        }
        self.assertIsNone(
            bench._metric_bound("first_up_ms", 1.0, baseline["first_up_ms"])
        )
        self.assertIsNotNone(
            bench._metric_bound("pgdata_files", 5, baseline["pgdata_files"])
        )


class WebSocketTests(BenchTestCase):
    def test_masked_client_frame_round_trips_and_ping_is_answered(self) -> None:
        server_end, client_end = socket.socketpair()
        websocket = bench._WebSocket(server_end)
        payload = b'{"type":"hello"}'
        mask = b"\x01\x02\x03\x04"
        masked = bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload))
        # A ping queued ahead of the data frame must be answered transparently.
        client_end.sendall(b"\x89\x80" + mask)
        client_end.sendall(b"\x81" + bytes([0x80 | len(payload)]) + mask + masked)
        opcode, received = websocket.recv()
        self.assertEqual((opcode, received), (0x1, payload))
        self.assertEqual(client_end.recv(2), b"\x8a\x00")
        server_end.close()
        client_end.close()


class SignatureTests(BenchTestCase):
    def test_p256_verify_rejects_malformed_inputs(self) -> None:
        self.assertFalse(bench.p256_verify("p256:" + "00" * 33, b"x", "00" * 64))
        self.assertFalse(bench.p256_verify("not-a-key", b"x", "00" * 64))
        self.assertFalse(
            bench.p256_verify(
                "p256:" + self._compressed_generator().hex(), b"x", "ff" * 64
            )
        )

    def test_p256_verify_accepts_a_valid_prehash_signature(self) -> None:
        # Deterministic ECDSA with private key `d = 1` and nonce `k = 1`: the public point is the
        # curve generator, so this exercises real verification math without a crypto package.
        generator_x, _generator_y = bench._P256_G
        key_id = "p256:" + self._compressed_generator().hex()
        transcript = b"ospp-v1" + b"\x11" * 32 + b"\x22" * 32 + b"127.0.0.1"
        digest = int.from_bytes(hashlib.sha256(transcript).digest(), "big")
        r = generator_x % bench._P256_N
        s = (digest + r) % bench._P256_N
        signature = r.to_bytes(32, "big") + s.to_bytes(32, "big")
        self.assertTrue(bench.p256_verify(key_id, transcript, signature.hex()))
        self.assertFalse(bench.p256_verify(key_id, transcript + b"x", signature.hex()))

    @staticmethod
    def _compressed_generator() -> bytes:
        x, y = bench._P256_G
        return bytes([2 + (y & 1)]) + x.to_bytes(32, "big")


if __name__ == "__main__":
    unittest.main()
