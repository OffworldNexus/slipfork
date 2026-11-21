"""Unit tests for the pure planning half of the publisher (no network, no mocks)."""

from __future__ import annotations

import importlib.util
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "publish_packages", ROOT / "scripts/publish_packages.py"
)
assert SPEC and SPEC.loader
publisher = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(publisher)


class VersioningTests(unittest.TestCase):
    def test_workspace_version_is_the_one_releases_use(self):
        self.assertEqual(publisher.workspace_version(), "0.1.0")

    def test_nightly_stamp_sorts_below_the_release(self):
        version = publisher.nightly_version("0.1.0", "20261121", 42, "abcdef123456")
        self.assertEqual(version, "0.1.0-0.nightly.20261121.r42.gabcdef1")
        self.assertIn("-0.nightly", version)

    def test_nightly_stamp_round_trips(self):
        version = publisher.nightly_version("0.1.0", "20261121", 7, "1234567deadbeef")
        self.assertEqual(publisher.parse_nightly(version), ("20261121", 7))
        self.assertIsNone(publisher.parse_nightly("0.1.0"))

    def test_bad_sha_is_refused(self):
        with self.assertRaises(SystemExit):
            publisher.nightly_version("0.1.0", "20261121", 1, "not-a-sha")


class RetentionTests(unittest.TestCase):
    def _versions(self, dates):
        out = []
        for date in dates:
            for run in (1, 2):
                out.append(publisher.nightly_version("0.1.0", date, run, "abcdef1"))
        return out

    def test_keeps_seven_runs_and_drops_whole_groups(self):
        versions = self._versions([f"202611{d:02d}" for d in range(10, 19)])
        kept, dropped = publisher.retention_plan(versions, keep=7)
        self.assertEqual(len(kept), 7)
        self.assertEqual(len(dropped), 11)
        self.assertIn("20261110", " ".join(dropped))
        self.assertNotIn("20261118", " ".join(dropped))

    def test_never_splits_an_architecture_pair(self):
        first = publisher.nightly_version("0.1.0", "20261110", 1, "abcdef1")
        second = publisher.nightly_version("0.1.0", "20261111", 1, "abcdef1")
        versions = [first, first, second, second]
        kept, dropped = publisher.retention_plan(versions, keep=1)
        self.assertEqual(kept, [second, second])
        self.assertEqual(dropped, [first, first])

    def test_release_versions_are_never_pruned(self):
        self.assertEqual(publisher.retention_plan(["0.1.0", "0.0.9"], keep=1), ([], []))


class DryRunTests(unittest.TestCase):
    def _run(self, args, env):
        return subprocess.run(
            [sys.executable, str(ROOT / "scripts/publish_packages.py"), *args],
            capture_output=True,
            text=True,
            env=env,
            check=False,
        )

    def test_every_mode_dry_runs_without_network(self):
        cases = [
            ["base-version"],
            ["release-stamp", "--date", "20261121", "--run", "5", "--sha", "abcdef1"],
            [
                "stamp",
                "--format",
                "rpm",
                "--date",
                "20261121",
                "--run",
                "5",
                "--sha",
                "abcdef1",
                "--dry-run",
            ],
            [
                "push",
                "--format",
                "deb",
                "--file",
                "slipfork_0.1.0_amd64.deb",
                "--dry-run",
            ],
            [
                "forgejo",
                "--format",
                "apk",
                "--file",
                "slipfork-0.1.0-r0.apk",
                "--dry-run",
            ],
            ["prune", "--format", "deb", "--keep", "7", "--dry-run"],
        ]
        env = {k: v for k, v in os.environ.items() if k != "FORGEJO_TOKEN"}
        env["CLOUDSMITH_API_KEY"] = "0" * 32
        for args in cases:
            with self.subTest(args=args):
                done = self._run(args, env)
                self.assertEqual(done.returncode, 0, done.stderr)

    def test_cloudsmith_missing_token_fails_closed(self):
        env = {k: v for k, v in os.environ.items() if k != "CLOUDSMITH_API_KEY"}
        with tempfile.NamedTemporaryFile(suffix=".deb") as artifact:
            done = self._run(
                ["push", "--format", "deb", "--file", artifact.name],
                env,
            )
        self.assertEqual(done.returncode, 1)
        self.assertIn("CLOUDSMITH_API_KEY", done.stderr)

    def test_forgejo_missing_token_is_a_notice_and_exit_zero(self):
        env = {k: v for k, v in os.environ.items() if k != "FORGEJO_TOKEN"}
        with tempfile.NamedTemporaryFile(suffix=".rpm") as artifact:
            done = self._run(
                ["forgejo", "--format", "rpm", "--file", artifact.name],
                env,
            )
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertIn("notice:", done.stdout)


if __name__ == "__main__":
    unittest.main()
