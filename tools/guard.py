#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Repository hygiene & anti-hallucination guard for Slipfork.

Inspects tracked + staged + untracked non-ignored files and refuses:
1. Any `.env` or `.env.*` secret file (only `.env.example` allowed).
2. Any file > 5 MiB or compiled archive/binary artefact outside `tests/golden/`.
3. Any `[derived]`, `[guessed]`, or `[hallucinated]` marker inside `docs/` specs/designs/research
   (learned from poc-orange: missing external facts must halt in `HUMAN.md`, never be invented!).
"""

from __future__ import annotations

import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path, PurePosixPath

MAX_FILE_BYTES = 5 * 1024 * 1024
GOLDEN_MAX_FILE_BYTES = 1024 * 1024

BANNED_EXTENSIONS = (
    ".whl",
    ".tar",
    ".tar.gz",
    ".tgz",
    ".zip",
    ".so",
    ".dylib",
    ".dll",
    ".pyc",
)
GOLDEN_FOLDER = ("tests", "golden")
ALLOWED_ENV_FILES = frozenset({".env.example"})
SPEC_GUESS_RE = re.compile(r"\[(?:derived|guessed|hallucinated)\]", re.IGNORECASE)

EXIT_OK = 0
EXIT_VIOLATIONS = 1
EXIT_GUARD_FAILED = 2


class GuardError(RuntimeError):
    """The guard could not inspect the repository."""


@dataclass(frozen=True, slots=True)
class Violation:
    path: str
    reason: str


def _mib(size: int) -> str:
    return f"{size / (1024 * 1024):.1f} MiB"


def _git(root: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", *args],
        cwd=root,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        msg = f"git {' '.join(args)} failed in {root}: {result.stderr.strip()}"
        raise GuardError(msg)
    return result.stdout


def repository_root(start: Path) -> Path:
    return Path(_git(start, "rev-parse", "--show-toplevel").strip())


def candidate_paths(root: Path) -> tuple[PurePosixPath, ...]:
    seen: set[str] = set()
    paths: list[PurePosixPath] = []
    for raw in _git(root, "ls-files", "-z").split("\0"):
        if raw and raw not in seen:
            seen.add(raw)
            paths.append(PurePosixPath(raw))
    status = _git(
        root, "status", "--porcelain=v1", "-z", "--untracked-files=all", "--no-renames"
    )
    for record in status.split("\0"):
        if len(record) < 4:
            continue
        idx, wt, raw = record[0], record[1], record[3:]
        if (idx in "AMRC" or (idx == "?" and wt == "?")) and raw not in seen:
            seen.add(raw)
            paths.append(PurePosixPath(raw))
    return tuple(paths)


def is_secret_env(path: PurePosixPath) -> bool:
    name = path.name
    if name in ALLOWED_ENV_FILES:
        return False
    return name == ".env" or name.startswith(".env.")


def in_golden_folder(path: PurePosixPath) -> bool:
    parts = path.parts
    return any(parts[idx : idx + 2] == GOLDEN_FOLDER for idx in range(len(parts) - 1))


def banned_extension(path: PurePosixPath) -> str | None:
    lower = path.name.lower()
    for ext in BANNED_EXTENSIONS:
        if lower.endswith(ext):
            return ext
    return None


def check_file(
    root: Path, path: PurePosixPath, size: int | None
) -> tuple[Violation, ...]:
    violations: list[Violation] = []
    if is_secret_env(path):
        violations.append(
            Violation(str(path), "secret file: commit .env.example or snow.yml instead")
        )
    ext = banned_extension(path)
    if ext is not None:
        if not in_golden_folder(path):
            violations.append(
                Violation(
                    str(path),
                    f"'{ext}' artefact does not belong in git (allowed only in tests/golden/)",
                )
            )
        elif size is not None and size > GOLDEN_MAX_FILE_BYTES:
            violations.append(
                Violation(
                    str(path),
                    f"{_mib(size)} golden file exceeds {_mib(GOLDEN_MAX_FILE_BYTES)} limit",
                )
            )
    if size is not None and size > MAX_FILE_BYTES:
        violations.append(
            Violation(
                str(path), f"{_mib(size)} exceeds {_mib(MAX_FILE_BYTES)} hard limit"
            )
        )
    if (
        path.parts
        and path.parts[0] == "docs"
        and path.parts != ("docs", "knowledge", "LESSONS.md")
        and path.suffix.lower() == ".md"
        and (root / path).is_file()
    ):
        text = (root / path).read_text(encoding="utf-8", errors="replace")
        match = SPEC_GUESS_RE.search(text)
        if match is not None:
            violations.append(
                Violation(
                    str(path),
                    f"forbidden {match.group(0)!r} guess marker in docs — block in HUMAN.md instead of inventing specs",
                )
            )
    return tuple(violations)


def _file_size(root: Path, path: PurePosixPath) -> int | None:
    absolute = root / path
    try:
        return absolute.stat().st_size if absolute.is_file() else None
    except OSError:
        return None


def inspect(root: Path) -> tuple[tuple[Violation, ...], int]:
    paths = candidate_paths(root)
    violations: list[Violation] = []
    for path in paths:
        if not (root / path).exists():
            continue
        violations.extend(check_file(root, path, _file_size(root, path)))
    return tuple(violations), len(paths)


def main() -> int:
    try:
        root = repository_root(Path.cwd())
        violations, checked = inspect(root)
    except GuardError as error:
        print(f"guard: cannot inspect repository: {error}", file=sys.stderr)
        return EXIT_GUARD_FAILED
    if violations:
        lines = [f"guard: {len(violations)} violation(s):"]
        lines.extend(f"  X {v.path}: {v.reason}" for v in violations)
        print("\n".join(lines), file=sys.stderr)
        return EXIT_VIOLATIONS
    print(f"guard: OK ({checked} file(s) checked)")
    return EXIT_OK


if __name__ == "__main__":
    raise SystemExit(main())
