"""Unit tests for the integration support compliance ceiling (`tools/check_support.py`).

Covers the pass path, every fail-closed evidence case, the automatic evidence-driven `--sync`
demotion (never promotion), and `docs/support-matrix.md` drift detection without mutation.
"""

from __future__ import annotations

import importlib.util
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from types import ModuleType

TOOLS_DIR = Path(__file__).resolve().parent.parent

# Test fixture mirror of `IntegrationTier::required_evidence()`; production code never restates it.
LADDER = {
    "none": ["rationale", "signoff_doc", "signoff_commit"],
    "declared": ["recipe_page", "critical_test"],
    "exercised": ["recipe_page", "critical_test", "ci_job"],
}

CRITICAL_SYMBOL = "slipfork_foo::critical_foo::checked_symbol"
RECIPE = "docs/recipe.md"
SIGNOFF_DOC = "docs/adr.md"
CI_JOB = "ci.yml#verify"

WORKFLOW = """name: CI
on: [push]
jobs:
  verify:
    runs-on: ubuntu-latest
    steps: []
"""


def _load() -> ModuleType:
    path = TOOLS_DIR / "check_support.py"
    spec = importlib.util.spec_from_file_location("check_support", path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules["check_support"] = module
    spec.loader.exec_module(module)
    return module


check = _load()


def _git(root: Path, args: list[str]) -> str:
    result = subprocess.run(
        ["git", *args], cwd=root, capture_output=True, text=True, check=False
    )
    if result.returncode != 0:
        msg = f"git {' '.join(args)} failed: {result.stderr}"
        raise AssertionError(msg)
    return result.stdout


def _commit(root: Path, message: str = "commit") -> str:
    _git(root, ["add", "-A"])
    _git(
        root,
        [
            "-c",
            "user.name=Support Test",
            "-c",
            "user.email=test@localhost",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            message,
        ],
    )
    return _git(root, ["rev-parse", "HEAD"]).strip()


def _repo(tmp_path: Path) -> tuple[Path, str]:
    root = tmp_path / "repo"
    (root / "crates" / "slipfork-foo" / "tests").mkdir(parents=True)
    (root / "docs").mkdir()
    (root / ".github" / "workflows").mkdir(parents=True)
    (root / "crates" / "slipfork-foo" / "tests" / "critical_foo.rs").write_text(
        "#[test]\nfn checked_symbol() {\n    assert!(true);\n}\n",
        encoding="utf-8",
    )
    (root / "docs" / "recipe.md").write_text("# recipe\n", encoding="utf-8")
    (root / "docs" / "adr.md").write_text("# sign-off\n", encoding="utf-8")
    (root / ".github" / "workflows" / "ci.yml").write_text(WORKFLOW, encoding="utf-8")
    _git(root, ["init", "-q"])
    commit = _commit(root, "fixture")
    return root, commit


def _row(
    integration: str = "widget",
    tier: str = "declared",
    evidence: dict[str, str] | None = None,
) -> dict:
    if evidence is None and tier == "declared":
        evidence = {"recipe_page": RECIPE, "critical_test": CRITICAL_SYMBOL}
    if evidence is None and tier == "exercised":
        evidence = {
            "recipe_page": RECIPE,
            "critical_test": CRITICAL_SYMBOL,
            "ci_job": CI_JOB,
        }
    return {
        "id": integration,
        "category": "cli",
        "tier": tier,
        "evidence": dict(evidence or {}),
        "required_evidence": list(LADDER[tier]),
    }


def _write_matrix(root: Path, rows: list[dict]) -> Path:
    path = root / "docs" / "support-matrix.md"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(check.render_matrix(rows), encoding="utf-8")
    return path


class VerifyTests(unittest.TestCase):
    def test_fully_attested_registry_has_no_errors(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root, _ = _repo(Path(tmp))
            rows = [_row(), _row("gadget", "exercised")]
            _write_matrix(root, rows)
            self.assertEqual(check.verify_registry(rows, root, check._git), [])
            self.assertEqual(check.matrix_errors(rows, root), [])

    def test_missing_critical_test_is_reported_by_id(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root, _ = _repo(Path(tmp))
            row = _row("vaporware", evidence={"recipe_page": RECIPE})
            errors = check.verify_registry([row], root, check._git)
            self.assertTrue(
                any("vaporware" in err and "critical_test" in err for err in errors)
            )

    def test_missing_signoff_attestation_is_reported_by_id(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root, _ = _repo(Path(tmp))
            row = _row(
                "unattested",
                "none",
                {"rationale": "no lifecycle", "signoff_doc": SIGNOFF_DOC},
            )
            errors = check.verify_registry([row], root, check._git)
            self.assertTrue(
                any("unattested" in err and "signoff_commit" in err for err in errors)
            )

    def test_missing_recipe_page_file_is_reported(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root, _ = _repo(Path(tmp))
            row = _row(
                evidence={
                    "recipe_page": "docs/missing.md",
                    "critical_test": CRITICAL_SYMBOL,
                }
            )
            errors = check.verify_registry([row], root, check._git)
            self.assertTrue(any("docs/missing.md" in err for err in errors))

    def test_signoff_commit_must_exist_and_touch_the_doc(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root, _ = _repo(Path(tmp))
            (root / "other.txt").write_text("unrelated\n", encoding="utf-8")
            unrelated = _commit(root, "unrelated change")
            row = _row(
                "signed",
                "none",
                {
                    "rationale": "no interface",
                    "signoff_doc": SIGNOFF_DOC,
                    "signoff_commit": unrelated,
                },
            )
            errors = check.verify_registry([row], root, check._git)
            self.assertTrue(any("does not touch" in err for err in errors))

            row["evidence"]["signoff_commit"] = "a" * 40
            errors = check.verify_registry([row], root, check._git)
            self.assertTrue(any("does not exist in git" in err for err in errors))

    def test_ci_job_must_name_an_existing_workflow_job(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root, _ = _repo(Path(tmp))
            row = _row(
                "ci-claimed",
                "exercised",
                {
                    "recipe_page": RECIPE,
                    "critical_test": CRITICAL_SYMBOL,
                    "ci_job": "ci.yml#nonexistent",
                },
            )
            errors = check.verify_registry([row], root, check._git)
            self.assertTrue(any("nonexistent" in err for err in errors))

    def test_unknown_evidence_field_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root, _ = _repo(Path(tmp))
            row = _row(
                evidence={"recipe_page": RECIPE, "critical_test": CRITICAL_SYMBOL}
            )
            row["evidence"]["vibes"] = "trust me"
            errors = check.verify_registry([row], root, check._git)
            self.assertTrue(
                any("unknown evidence field 'vibes'" in err for err in errors)
            )

    def test_matrix_drift_is_reported_without_mutating_the_file(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root, _ = _repo(Path(tmp))
            rows = [_row()]
            path = _write_matrix(root, rows)
            path.write_text(
                path.read_text(encoding="utf-8") + "drifted\n", encoding="utf-8"
            )
            before = path.read_bytes()
            errors = check.matrix_errors(rows, root)
            self.assertTrue(errors)
            self.assertEqual(path.read_bytes(), before)


class DemotionTests(unittest.TestCase):
    def test_sync_demotes_to_the_highest_satisfied_tier_never_promotes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root, _ = _repo(Path(tmp))
            demoted = _row(
                "stale",
                "exercised",
                {"recipe_page": RECIPE, "critical_test": CRITICAL_SYMBOL},
            )
            complete = _row(
                "stronger-evidence",
                "declared",
                {
                    "recipe_page": RECIPE,
                    "critical_test": CRITICAL_SYMBOL,
                    "ci_job": CI_JOB,
                },
            )
            demotions = check.plan_demotions([demoted, complete], root, check._git)
            self.assertEqual(len(demotions), 1)
            self.assertEqual(demotions[0].id, "stale")
            self.assertEqual(demotions[0].from_tier, "exercised")
            self.assertEqual(demotions[0].to_tier, "declared")
            self.assertIn("ci_job", demotions[0].missing)
            rendered = demotions[0].render()
            self.assertIn("exercised -> declared", rendered)

    def test_sync_fails_closed_when_no_lower_tier_is_satisfied(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root, commit = _repo(Path(tmp))
            none_row = _row(
                "none-ok",
                "none",
                {
                    "rationale": "no lifecycle",
                    "signoff_doc": SIGNOFF_DOC,
                    "signoff_commit": commit,
                },
            )
            broken = _row("broken", "exercised", {})
            with self.assertRaises(check.SupportError):
                check.plan_demotions([none_row, broken], root, check._git)

    def test_rewrite_tiers_touches_only_the_demoted_block(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root, _ = _repo(Path(tmp))
            demoted = _row(
                "stale",
                "exercised",
                {"recipe_page": RECIPE, "critical_test": CRITICAL_SYMBOL},
            )
            kept = _row("kept", "declared")
            demotions = check.plan_demotions([demoted, kept], root, check._git)
            self.assertEqual([demotion.id for demotion in demotions], ["stale"])
            original = (
                "# registry\n"
                "[[integration]]\n"
                'id = "stale"\n'
                'category = "cli"\n'
                'tier = "exercised"\n'
                "\n"
                "[integration.evidence]\n"
                f'recipe_page = "{RECIPE}"\n'
                f'critical_test = "{CRITICAL_SYMBOL}"\n'
                "\n"
                "[[integration]]\n"
                'id = "kept"\n'
                'category = "cli"\n'
                'tier = "declared"\n'
            )
            updated = check.rewrite_tiers(original, demotions)
            self.assertIn('id = "stale"\ncategory = "cli"\ntier = "declared"', updated)
            self.assertIn('id = "kept"\ncategory = "cli"\ntier = "declared"', updated)
            self.assertEqual(updated.count('tier = "declared"'), 2)
            self.assertNotIn('tier = "exercised"', updated)


if __name__ == "__main__":
    unittest.main()
