#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Guard protected critical tests in Slipfork.

Protects:
- `crates/*/tests/critical_*.rs`
- `**/tests/critical/**`
- `**/*.critical.test.ts`
- `packages/opencode-slipfork/tests/**/*.test.ts` and `*.spec.ts`

Adding new critical test files (including untracked files) is always allowed.
Modifying or deleting an existing critical test requires an explicit covering glob
in the assigned plan node's `paths`.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import TYPE_CHECKING, Final

if TYPE_CHECKING:
    from collections.abc import Iterable, Sequence

EXIT_OK: Final = 0
EXIT_VIOLATIONS: Final = 1
EXIT_GUARD_FAILED: Final = 2

PROTECTED_DIR: Final = ("tests", "critical")
PLUGIN_TESTS_PREFIX: Final = ("packages", "opencode-slipfork", "tests")
BREAKING_STATUSES: Final = frozenset({"M", "D", "T"})


class GuardError(RuntimeError):
    """The guard could not inspect the repository."""


@dataclass(frozen=True, slots=True)
class Change:
    status: str
    path: str

    @property
    def is_breaking(self) -> bool:
        return self.status in BREAKING_STATUSES

    @property
    def is_addition(self) -> bool:
        return self.status == "A"


@dataclass(frozen=True, slots=True)
class Finding:
    change: Change
    pattern: str | None

    @property
    def is_violation(self) -> bool:
        return self.change.is_breaking and self.pattern is None

    @property
    def is_addition(self) -> bool:
        return self.change.is_addition


def glob_to_regex(pattern: str) -> re.Pattern[str]:
    out: list[str] = ["^"]
    index = 0
    while index < len(pattern):
        if pattern.startswith("**/", index):
            out.append("(?:.*/)?")
            index += 3
        elif pattern.startswith("**", index):
            out.append(".*")
            index += 2
        elif pattern[index] == "*":
            out.append("[^/]*")
            index += 1
        elif pattern[index] == "?":
            out.append("[^/]")
            index += 1
        else:
            out.append(re.escape(pattern[index]))
            index += 1
    out.append("$")
    return re.compile("".join(out))


def is_protected(path: str) -> bool:
    parts = PurePosixPath(path).parts
    for index in range(len(parts) - 1):
        if (parts[index], parts[index + 1]) == PROTECTED_DIR:
            return True
    if (
        len(parts) == 4
        and parts[0] == "crates"
        and parts[2] == "tests"
        and parts[3].startswith("critical_")
        and parts[3].endswith(".rs")
    ):
        return True
    name = parts[-1] if parts else ""
    if (
        len(parts) > len(PLUGIN_TESTS_PREFIX)
        and parts[: len(PLUGIN_TESTS_PREFIX)] == PLUGIN_TESTS_PREFIX
        and name.endswith((".test.ts", ".spec.ts"))
    ):
        return True
    return name.endswith((".critical.test.ts", ".critical.spec.ts"))


def authorizing_pattern(path: str, patterns: Iterable[str]) -> str | None:
    for pattern in patterns:
        if glob_to_regex(pattern).match(path):
            return pattern
    return None


def _unquote(value: str) -> str:
    if len(value) >= 2 and value[0] == value[-1] and value[0] in {"'", '"'}:
        return value[1:-1]
    return value


def parse_node_paths(node_file: Path) -> list[str]:
    lines = node_file.read_text(encoding="utf-8").splitlines()
    if not lines or lines[0].strip() != "---":
        msg = f"{node_file}: no YAML frontmatter"
        raise GuardError(msg)
    paths: list[str] = []
    in_paths = False
    for line in lines[1:]:
        stripped = line.strip()
        if stripped == "---":
            break
        if in_paths and stripped.startswith("- "):
            paths.append(_unquote(stripped[2:].strip()))
            continue
        if in_paths and stripped:
            in_paths = False
        if stripped.startswith("paths:"):
            in_paths = True
            inline = stripped[len("paths:") :].strip()
            if inline.startswith("[") and inline.endswith("]"):
                paths.extend(
                    _unquote(item.strip())
                    for item in inline[1:-1].split(",")
                    if item.strip()
                )
    return paths


def find_node_file(repo: Path, node_id: str) -> Path:
    matches = sorted(
        path
        for path in (repo / "plan" / "nodes").rglob("NODE.md")
        if path.parent.name == node_id
    )
    if not matches:
        msg = f"no plan node named {node_id!r} under {repo / 'plan' / 'nodes'}"
        raise GuardError(msg)
    if len(matches) > 1:
        msg = f"ambiguous node id {node_id!r}"
        raise GuardError(msg)
    return matches[0]


def _git(repo: Path, *args: str) -> str:
    completed = subprocess.run(
        ["git", "-C", str(repo), "-c", "core.quotePath=false", *args],
        capture_output=True,
        text=True,
        check=False,
    )
    if completed.returncode != 0:
        msg = f"git {' '.join(args)} failed ({completed.returncode}): {completed.stderr.strip()}"
        raise GuardError(msg)
    return completed.stdout


def changed_files(repo: Path, base: str) -> list[Change]:
    changes: list[Change] = []
    seen: set[str] = set()
    diff = _git(
        repo, "diff", "--no-renames", "--name-status", "--no-ext-diff", base, "--"
    )
    for line in diff.splitlines():
        if not line.strip():
            continue
        status, _, path = line.partition("\t")
        changes.append(Change(status=status[:1], path=path))
        seen.add(path)
    for path in _git(
        repo, "ls-files", "--others", "--exclude-standard", "--"
    ).splitlines():
        path = path.strip()
        if not path or path in seen:
            continue
        changes.append(Change(status="A", path=path))
        seen.add(path)
    return changes


def evaluate(changes: Iterable[Change], patterns: Iterable[str]) -> list[Finding]:
    covered = list(patterns)
    return [
        Finding(change=change, pattern=authorizing_pattern(change.path, covered))
        for change in changes
        if is_protected(change.path)
    ]


def _describe(finding: Finding) -> str:
    status = finding.change.status
    if finding.is_addition:
        verdict = "added (always allowed)"
    elif finding.pattern is not None:
        verdict = f"authorized by {finding.pattern}"
    else:
        verdict = "VIOLATION: not covered by any node path"
    return f"  {status}  {finding.change.path}  [{verdict}]"


def _run(args: argparse.Namespace) -> int:
    repo = args.repo.resolve()
    changes = changed_files(repo, args.base)
    patterns: list[str] = []
    if args.node is not None:
        patterns = parse_node_paths(find_node_file(repo, args.node))
    findings = evaluate(changes, patterns)

    print(f"critical-guard: {args.base}..working tree, {len(changes)} changed file(s)")
    if not findings:
        print("  no change to a protected critical test")
        return EXIT_OK
    for finding in findings:
        print(_describe(finding))

    violations = [finding for finding in findings if finding.is_violation]
    if args.node is None or args.report:
        return EXIT_OK
    if violations:
        print(
            f"critical-guard: FAILED: {len(violations)} unauthorized protected test change(s) in {args.node}."
        )
        return EXIT_VIOLATIONS
    print(f"critical-guard: OK: authorized by {args.node}")
    return EXIT_OK


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Guard protected critical tests.")
    parser.add_argument("--base", required=True)
    parser.add_argument("--node", default=None)
    parser.add_argument("--repo", default=Path(), type=Path)
    parser.add_argument("--report", action="store_true")
    args = parser.parse_args(argv)
    try:
        return _run(args)
    except GuardError as error:
        print(f"critical-guard: {error}", file=sys.stderr)
        return EXIT_GUARD_FAILED


if __name__ == "__main__":
    sys.exit(main())
