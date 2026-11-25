"""Guards the user-facing `make live-test` summary and remedy strings byte-for-byte."""

from __future__ import annotations

import subprocess
import unittest
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
MAKEFILE = REPO_ROOT / "Makefile"
DO_VM = REPO_ROOT / "tools" / "do_vm.py"


def live_test_recipe() -> str:
    """The `live-test:` recipe body, up to the next `# ---` section comment."""
    lines = MAKEFILE.read_text(encoding="utf-8").splitlines(keepends=True)
    start = lines.index("live-test:\n")
    for offset, line in enumerate(lines[start + 1 :], start + 1):
        if line.startswith("# ---"):
            return "".join(lines[start:offset])
    return "".join(lines[start:])


class LiveHarnessStringsTests(unittest.TestCase):
    def test_recipe_carries_the_exact_progress_and_summary_lines(self):
        recipe = live_test_recipe()
        for needle in (
            "printf '[live] RUN %s %s\\n'",
            "printf '[live] PASS %s %s\\n'",
            "printf '[live] FAIL %s %s (exit %s)\\n'",
            'echo "live-test: FAILED $$passed/$$total — see the FAIL lines above"',
            'echo "live-test: $$passed/$$total suites passed"',
            'echo "live-test: cold store $$store"',
            "next: LIVE_RESET=1 make live-test",
            'echo "live-test: $$root must be the Btrfs mount root, because a '
            "subdirectory shares the parent store; next: "
            'SLIPFORK_TEST_ROOT=/mnt/slipfork-live make live-test"',
            'echo "live-test: $$store already holds a warm image cache; next: '
            'LIVE_RESET=1 make live-test (cold suites:$$cold)"',
            "purge_store() {",
            'echo "live-test: cold store $$store (before $$suite)"',
            "cannot purge the warm store",
            "findmnt -n -o TARGET -T",
        ):
            self.assertIn(needle, recipe)

    def test_recipe_has_no_misspelled_or_misplaced_failure_prefix(self):
        recipe = live_test_recipe()
        for typo in ("FALIED", "FAILIED", "[live] FAILED"):
            self.assertNotIn(typo, recipe)

    def test_do_vm_keeps_the_mount_root_and_cold_store_contract(self):
        source = DO_VM.read_text(encoding="utf-8")
        for needle in (
            "SLIPFORK_TEST_ROOT={SUITE_TEST_ROOT}",
            "SUITE_TEST_ROOT = STATE_DIR",
            'echo "cold store $store"',
            "could not delete the warm store",
        ):
            self.assertIn(needle, source)

    def test_summary_formats_render_through_sh(self):
        recipe = live_test_recipe()
        cases = {
            "live-test: FAILED 2/3 — see the FAIL lines above\n": (
                'echo "live-test: FAILED $$passed/$$total — see the FAIL lines above"'
            ),
            "live-test: 2/3 suites passed\n": (
                'echo "live-test: $$passed/$$total suites passed"'
            ),
        }
        for expected, template in cases.items():
            self.assertIn(template, recipe)
            result = subprocess.run(
                ["sh", "-c", "passed=2; total=3; " + template.replace("$$", "$")],
                text=True,
                capture_output=True,
                check=True,
            )
            self.assertEqual(result.stdout, expected)


if __name__ == "__main__":
    unittest.main()
