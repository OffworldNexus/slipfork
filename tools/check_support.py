#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Integration support compliance ceiling (`ADR-0007` §3).

`--check` is the read-only, fail-closed gate wired into `make check`: it consumes
`slipfork support --json` so tier requirements come strictly from the single Rust ladder
(`IntegrationTier::required_evidence()`, never restated here), verifies every evidence pointer
on disk, in `git`, and in `.github/workflows/`, and proves `docs/support-matrix.md` is
byte-identical to the rendered registry. It never writes a file.

`--sync` is the only mutator: it demotes an entry whose claimed tier evidence is missing to the
highest lower tier whose evidence is fully present (never promoting), rewrites only the affected
`tier = "..."` lines in `support/integrations.toml`, and regenerates `docs/support-matrix.md`.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from collections.abc import Callable, Sequence
from dataclasses import dataclass
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
REGISTRY_PATH = Path("support/integrations.toml")
MATRIX_PATH = Path("docs/support-matrix.md")
WORKFLOWS_DIR = Path(".github/workflows")

# Ordered scale only (`ADR-0007` §1). Per-tier requirements are never duplicated here: they are
# read from the `required_evidence` arrays emitted by `slipfork support --json`.
TIER_SCALE = ("none", "declared", "exercised", "field-tested", "hardened")

# Evidence pointer vocabulary (`EvidenceField`), used to reject unknown keys from the CLI.
EVIDENCE_FIELDS = frozenset(
    {
        "rationale",
        "signoff_doc",
        "signoff_commit",
        "recipe_page",
        "critical_test",
        "ci_job",
        "deploy_ref",
        "production_suite_ref",
        "test_audit",
    }
)
FILE_POINTERS = frozenset(
    {"recipe_page", "signoff_doc", "deploy_ref", "production_suite_ref", "test_audit"}
)
COMMIT_RE = re.compile(r"[0-9a-f]{40}")
INTEGRATION_BLOCK_RE = re.compile(r"^\[\[integration\]\]\s*$")
TIER_LINE_RE = re.compile(r'^tier\s*=\s*"(?P<tier>[^"]+)"\s*$')
ID_LINE_RE = re.compile(r'^id\s*=\s*"(?P<id>[^"]+)"\s*$')
JOB_KEY_RE = re.compile(r"^  (?P<job>[A-Za-z0-9_.-]+):\s*(?:#.*)?$")

GitQuery = Callable[[Path, Sequence[str]], str]


class SupportError(RuntimeError):
    """The compliance tool could not inspect the repository (exit code 2)."""


@dataclass(frozen=True, slots=True)
class Demotion:
    """One evidence-driven tier demotion (`--sync`)."""

    id: str
    from_tier: str
    to_tier: str
    missing: tuple[str, ...]

    def render(self) -> str:
        return (
            f"support demotion: '{self.id}' {self.from_tier} -> {self.to_tier} "
            f"(missing: {', '.join(self.missing)})"
        )


def _git(root: Path, args: Sequence[str]) -> str:
    result = subprocess.run(
        ["git", *args],
        cwd=root,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        msg = f"git {' '.join(args)} failed in {root}: {result.stderr.strip()}"
        raise SupportError(msg)
    return result.stdout


def run_support_json(root: Path) -> list[dict]:
    """Read the registry through `slipfork support --json` (embedded Rust source of truth)."""
    result = subprocess.run(
        ["cargo", "run", "-q", "-p", "slipfork", "--", "support", "--json"],
        cwd=root,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        msg = f"`slipfork support --json` failed:\n{result.stderr.strip()}"
        raise SupportError(msg)
    try:
        entries = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        msg = f"`slipfork support --json` did not emit JSON: {error}"
        raise SupportError(msg) from error
    if not isinstance(entries, list) or not entries:
        msg = "`slipfork support --json` must emit a non-empty list of integrations"
        raise SupportError(msg)
    for entry in entries:
        _validate_row(entry)
    return entries


def _validate_row(entry: object) -> None:
    if not isinstance(entry, dict):
        msg = f"support row must be an object, got {type(entry).__name__}"
        raise SupportError(msg)
    for key in ("id", "category", "tier", "evidence", "required_evidence"):
        if key not in entry:
            msg = f"support row is missing '{key}': {entry!r}"
            raise SupportError(msg)
    if not isinstance(entry["evidence"], dict):
        msg = f"support row '{entry['id']}' has a non-object evidence value"
        raise SupportError(msg)
    if not isinstance(entry["required_evidence"], list):
        msg = f"support row '{entry['id']}' has a non-list required_evidence value"
        raise SupportError(msg)
    if entry["tier"] not in TIER_SCALE:
        msg = f"support row '{entry['id']}' claims unknown tier '{entry['tier']}'"
        raise SupportError(msg)


def _present(evidence: dict, field_name: str) -> bool:
    value = evidence.get(field_name)
    return isinstance(value, str) and bool(value.strip())


def _inside(root: Path, pointer: str) -> Path | None:
    """Resolve a repo-relative pointer, refusing absolute paths and `..` escapes."""
    raw = Path(pointer)
    if raw.is_absolute():
        return None
    resolved = (root / raw).resolve()
    try:
        resolved.relative_to(root.resolve())
    except ValueError:
        return None
    return resolved


def _verify_critical_test(root: Path, symbol: str) -> list[str]:
    parts = symbol.split("::")
    if len(parts) != 3:
        return [
            f"critical_test '{symbol}' must be `<crate>::<critical_file>::<test_fn>`"
        ]
    crate, file_stem, test_fn = parts
    if not file_stem.startswith("critical_"):
        return [f"critical_test '{symbol}' must live in a `critical_*` test file"]
    crate_dir = crate.replace("_", "-")
    path = root / "crates" / crate_dir / "tests" / f"{file_stem}.rs"
    if not path.is_file():
        return [f"critical_test '{symbol}' has no file at {path.relative_to(root)}"]
    source = path.read_text(encoding="utf-8", errors="replace")
    if f"fn {test_fn}(" not in source:
        return [
            f"critical_test '{symbol}' names no `fn {test_fn}(` in {path.relative_to(root)}"
        ]
    return []


def _job_keys(workflow_text: str) -> set[str]:
    keys: set[str] = set()
    in_jobs = False
    for line in workflow_text.splitlines():
        if line.rstrip() == "jobs:":
            in_jobs = True
            continue
        if in_jobs:
            if line and not line.startswith(" "):
                break
            match = JOB_KEY_RE.match(line)
            if match is not None:
                keys.add(match.group("job"))
    return keys


def _verify_ci_job(root: Path, pointer: str) -> list[str]:
    workflow, _, job = pointer.partition("#")
    if not workflow or not job:
        return [f"ci_job '{pointer}' must be `<workflow.yml>#<job_id>`"]
    if Path(workflow).name != workflow or not workflow.endswith((".yml", ".yaml")):
        return [f"ci_job '{pointer}' must name a workflow file under {WORKFLOWS_DIR}"]
    path = root / WORKFLOWS_DIR / workflow
    if not path.is_file():
        return [f"ci_job '{pointer}' has no workflow at {path.relative_to(root)}"]
    keys = _job_keys(path.read_text(encoding="utf-8", errors="replace"))
    if job not in keys:
        return [
            (
                f"ci_job '{pointer}' names no job '{job}' under `jobs:` in "
                f"{path.relative_to(root)} (found: {', '.join(sorted(keys)) or 'none'})"
            )
        ]
    return []


def _verify_pointer(
    entry: dict, field_name: str, git: GitQuery, root: Path
) -> list[str]:
    value = entry["evidence"].get(field_name)
    if not isinstance(value, str) or not value.strip():
        return [f"'{field_name}' must be a non-empty string"]
    if field_name in FILE_POINTERS:
        path = _inside(root, value)
        if path is None:
            return [f"'{field_name}' pointer '{value}' must stay inside the repository"]
        if not path.is_file():
            return [f"'{field_name}' pointer '{value}' does not exist"]
        return []
    if field_name == "critical_test":
        return _verify_critical_test(root, value)
    if field_name == "ci_job":
        return _verify_ci_job(root, value)
    if field_name == "signoff_commit":
        if COMMIT_RE.fullmatch(value) is None:
            return [f"'signoff_commit' '{value}' must be a lowercase 40-hex commit SHA"]
        try:
            git(root, ["cat-file", "-e", f"{value}^{{commit}}"])
        except SupportError:
            return [f"'signoff_commit' '{value}' does not exist in git"]
        doc = entry["evidence"].get("signoff_doc")
        if isinstance(doc, str) and doc.strip():
            try:
                touched = git(
                    root,
                    [
                        "diff-tree",
                        "--no-commit-id",
                        "--name-only",
                        "-r",
                        value,
                    ],
                )
            except SupportError:
                return [
                    f"'signoff_commit' '{value}' cannot be inspected against '{doc}'"
                ]
            if doc not in touched.splitlines():
                return [
                    f"'signoff_commit' '{value}' does not touch signoff_doc '{doc}'"
                ]
        return []
    if field_name == "rationale":
        return []
    return [f"unknown evidence field '{field_name}'"]


def verify_entry(entry: dict, root: Path, git: GitQuery) -> list[str]:
    """Every required pointer plus every present pointer, fail-closed."""
    errors: list[str] = []
    integration = entry["id"]
    evidence = entry["evidence"]
    for field_name in sorted(set(evidence) - EVIDENCE_FIELDS):
        errors.append(
            f"{integration}: unknown evidence field '{field_name}' (not in EvidenceField)"
        )
    for field_name in entry["required_evidence"]:
        if field_name not in EVIDENCE_FIELDS:
            errors.append(
                f"{integration}: tier '{entry['tier']}' requires unknown evidence field "
                f"'{field_name}'"
            )
            continue
        if not _present(evidence, field_name):
            errors.append(
                f"{integration}: tier '{entry['tier']}' requires '{field_name}' but it is "
                f"missing"
            )
    for field_name in sorted(set(evidence) & EVIDENCE_FIELDS):
        if not _present(evidence, field_name):
            errors.append(f"{integration}: evidence field '{field_name}' is blank")
            continue
        for problem in _verify_pointer(entry, field_name, git, root):
            errors.append(f"{integration}: {problem}")
    return errors


def verify_registry(entries: list[dict], root: Path, git: GitQuery) -> list[str]:
    errors: list[str] = []
    for entry in entries:
        errors.extend(verify_entry(entry, root, git))
    return errors


def render_matrix(entries: list[dict]) -> str:
    """Deterministic Markdown for the published support table (`--sync` is the only writer)."""
    lines = [
        (
            "<!-- Generated by `make support-sync` from `support/integrations.toml`. "
            "Do not edit by hand. -->"
        ),
        "",
        "# Integration support matrix",
        "",
        "Every tier claim is typed in `support/integrations.toml` and verified by `make check`",
        "(`tools/check_support.py`): each file pointer, `critical_*` test symbol, CI job, and",
        "sign-off commit exists in this repository (`ADR-0007`).",
        "",
        f"Tiers, weakest to strongest: {' < '.join(f'`{tier}`' for tier in TIER_SCALE)}.",
        "",
        "| Integration | Category | Tier | Required evidence | Present evidence |",
        "|---|---|---|---|---|",
    ]
    for entry in entries:
        required = ", ".join(f"`{field}`" for field in entry["required_evidence"])
        present = ", ".join(f"`{field}`" for field in sorted(entry["evidence"]))
        lines.append(
            f"| `{entry['id']}` | `{entry['category']}` | `{entry['tier']}` "
            f"| {required} | {present} |"
        )
    return "\n".join(lines) + "\n"


def matrix_errors(entries: list[dict], root: Path) -> list[str]:
    path = root / MATRIX_PATH
    if not path.is_file():
        return [f"{MATRIX_PATH} is missing; run `make support-sync`"]
    committed = path.read_text(encoding="utf-8")
    rendered = render_matrix(entries)
    if committed == rendered:
        return []
    line = _first_difference(committed, rendered)
    return [
        f"{MATRIX_PATH} is stale at line {line}; run `make support-sync` and commit the table"
    ]


def _first_difference(left: str, right: str) -> int:
    left_lines = left.splitlines()
    right_lines = right.splitlines()
    for index, (a, b) in enumerate(zip(left_lines, right_lines, strict=False), start=1):
        if a != b:
            return index
    return min(len(left_lines), len(right_lines)) + 1


def tier_requirements(entries: list[dict]) -> dict[str, list[str]]:
    """Tier -> required fields exactly as emitted by the Rust ladder, conflicts failing closed."""
    requirements: dict[str, list[str]] = {}
    for entry in entries:
        tier = entry["tier"]
        required = list(entry["required_evidence"])
        known = requirements.setdefault(tier, required)
        if known != required:
            msg = (
                f"`slipfork support --json` reported conflicting required_evidence for tier "
                f"'{tier}': {known} vs {required}"
            )
            raise SupportError(msg)
    return requirements


def _entry_satisfies(
    entry: dict,
    tier: str,
    requirements: dict[str, list[str]],
    root: Path,
    git: GitQuery,
) -> bool:
    required = requirements.get(tier)
    if required is None:
        msg = (
            f"tier '{tier}' has no required_evidence in `slipfork support --json`; cannot "
            f"evaluate a demotion into it (add a registry entry at that tier or extend the CLI)"
        )
        raise SupportError(msg)
    for field_name in required:
        if not _present(entry["evidence"], field_name):
            return False
        if _verify_pointer(entry, field_name, git, root):
            return False
    return True


def plan_demotions(entries: list[dict], root: Path, git: GitQuery) -> list[Demotion]:
    """Demote to the highest lower tier whose evidence is fully present; never promote."""
    requirements = tier_requirements(entries)
    demotions: list[Demotion] = []
    for entry in entries:
        claimed = entry["tier"]
        if not verify_entry(entry, root, git):
            continue
        claimed_index = TIER_SCALE.index(claimed)
        missing = tuple(
            field_name
            for field_name in entry["required_evidence"]
            if not _present(entry["evidence"], field_name)
            or _verify_pointer(entry, field_name, git, root)
        )
        target: str | None = None
        for candidate in reversed(TIER_SCALE[:claimed_index]):
            if _entry_satisfies(entry, candidate, requirements, root, git):
                target = candidate
                break
        if target is None:
            msg = (
                f"integration '{entry['id']}' claims tier '{claimed}' but no lower tier's "
                f"required evidence is satisfied (missing: {', '.join(missing) or 'none'}); "
                f"demotion would be dishonest — fix the evidence or delete the entry"
            )
            raise SupportError(msg)
        demotions.append(
            Demotion(
                id=entry["id"],
                from_tier=claimed,
                to_tier=target,
                missing=missing,
            )
        )
    return demotions


def rewrite_tiers(toml_text: str, demotions: list[Demotion]) -> str:
    """Rewrite only the `tier = "..."` line of each demoted `[[integration]]` block."""
    targets = {demotion.id: demotion.to_tier for demotion in demotions}
    if not targets:
        return toml_text
    out: list[str] = []
    current_id: str | None = None
    replaced: set[str] = set()
    for line in toml_text.splitlines(keepends=True):
        if INTEGRATION_BLOCK_RE.match(line):
            current_id = None
        else:
            id_match = ID_LINE_RE.match(line.rstrip("\n"))
            tier_match = TIER_LINE_RE.match(line.rstrip("\n"))
            if id_match is not None:
                current_id = id_match.group("id")
            elif (
                tier_match is not None
                and current_id in targets
                and current_id not in replaced
            ):
                line = f'tier = "{targets[current_id]}"\n'
                replaced.add(current_id)
        out.append(line)
    missing = set(targets) - replaced
    if missing:
        msg = f"could not rewrite the tier of integration(s) {sorted(missing)}"
        raise SupportError(msg)
    return "".join(out)


def _json_verdict(
    entries: list[dict],
    errors: list[str],
    demotions: list[Demotion] | None,
) -> str:
    verdict: dict[str, object] = {
        "ok": not errors,
        "integrations": [
            {
                "id": entry["id"],
                "category": entry["category"],
                "tier": entry["tier"],
                "required_evidence": entry["required_evidence"],
                "evidence": entry["evidence"],
            }
            for entry in entries
        ],
        "errors": errors,
    }
    if demotions is not None:
        verdict["demotions"] = [
            {
                "id": demotion.id,
                "from": demotion.from_tier,
                "to": demotion.to_tier,
                "missing": list(demotion.missing),
            }
            for demotion in demotions
        ]
    return json.dumps(verdict, indent=2, sort_keys=True)


def check(root: Path, git: GitQuery) -> tuple[list[dict], list[str]]:
    entries = run_support_json(root)
    errors = verify_registry(entries, root, git)
    errors.extend(matrix_errors(entries, root))
    return entries, errors


def sync(root: Path, git: GitQuery) -> tuple[list[dict], list[Demotion]]:
    entries = run_support_json(root)
    demotions = plan_demotions(entries, root, git)
    if demotions:
        path = root / REGISTRY_PATH
        original = path.read_text(encoding="utf-8")
        updated = rewrite_tiers(original, demotions)
        if updated != original:
            path.write_text(updated, encoding="utf-8")
            entries = run_support_json(root)
    (root / MATRIX_PATH).parent.mkdir(parents=True, exist_ok=True)
    (root / MATRIX_PATH).write_text(render_matrix(entries), encoding="utf-8")
    return entries, demotions


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="check_support.py",
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    action = parser.add_mutually_exclusive_group()
    action.add_argument(
        "--check",
        action="store_true",
        help="read-only compliance ceiling (default); exits 1 on any violation or matrix drift",
    )
    action.add_argument(
        "--sync",
        action="store_true",
        help="demote claims whose evidence is missing, rewrite the registry, and regenerate "
        "docs/support-matrix.md (the only mutator)",
    )
    parser.add_argument(
        "--json", action="store_true", help="print the machine-readable verdict"
    )
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        if args.sync:
            entries, demotions = sync(REPO_ROOT, _git)
            if args.json:
                print(_json_verdict(entries, [], demotions))
            else:
                for demotion in demotions:
                    print(demotion.render())
                if not demotions:
                    print(
                        "support-sync: no demotions; every claim matches its evidence"
                    )
                print(f"support-sync: wrote {MATRIX_PATH}")
            return 0
        entries, errors = check(REPO_ROOT, _git)
        if args.json:
            print(_json_verdict(entries, errors, None))
        if errors:
            print(
                f"check_support: FAILED ({len(errors)} violation(s)):",
                file=sys.stderr,
            )
            for error in errors:
                print(f"  X {error}", file=sys.stderr)
            return 1
        if not args.json:
            print(
                f"check_support: OK ({len(entries)} integration(s); all evidence pointers "
                f"verified and {MATRIX_PATH} in sync)"
            )
        return 0
    except SupportError as error:
        print(f"check_support: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
