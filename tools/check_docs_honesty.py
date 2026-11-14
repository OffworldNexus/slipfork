#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Verify that published documentation (`README.md` and non-excluded `docs/**/*.md`)
never advertises unimplemented `slipfork <subcommand>` CLI commands (Zero Vaporware rule).

Target vision and pre-implementation DX blueprints belong strictly in `docs/VISION.md`
and `docs/design/` (which are excluded from the public Zensical documentation site).
"""

from __future__ import annotations

import re
import subprocess
import sys
import tomllib
from pathlib import Path

FENCE_RE = re.compile(r"```(?:console|bash|sh|shell|text)?\n(.*?)```", re.DOTALL)
CMD_LINE_RE = re.compile(r"^\s*(?:\$\s+)?slipfork\s+([a-z][a-z0-9_-]*)", re.MULTILINE)
INLINE_TABLE_CMD_RE = re.compile(r"\|\s*`slipfork\s+([a-z][a-z0-9_-]*)")

EXCLUDED_PREFIXES = (
    "VISION.md",
    "design/",
    "decisions/",
    "research/",
    "knowledge/",
    "testing/",
)


def extract_help_subcommands(help_text: str) -> set[str]:
    cmds: set[str] = {"help", "--help", "-h", "--version", "-V"}
    in_commands = False
    for line in help_text.splitlines():
        if line.strip() == "Commands:":
            in_commands = True
            continue
        if in_commands:
            if not line.strip():
                break
            parts = line.strip().split()
            if parts and re.fullmatch(r"[a-z][a-z0-9_-]*", parts[0]):
                cmds.add(parts[0])
    return cmds


def published_doc_files(repo: Path) -> list[Path]:
    files: list[Path] = []
    readme = repo / "README.md"
    if readme.is_file():
        files.append(readme)
    docs_dir = repo / "docs"
    if docs_dir.is_dir():
        for md in sorted(docs_dir.rglob("*.md")):
            rel = md.relative_to(docs_dir).as_posix()
            if any(
                rel == p.rstrip("/") or rel.startswith(p) for p in EXCLUDED_PREFIXES
            ):
                continue
            files.append(md)
    # Also ensure every file referenced in zensical.toml nav exists.
    z_path = repo / "zensical.toml"
    if z_path.is_file():
        data = tomllib.loads(z_path.read_text(encoding="utf-8"))
        nav = data.get("project", {}).get("nav", [])
        for rel in _iter_nav_paths(nav):
            p = docs_dir / rel
            if p not in files and p.is_file():
                files.append(p)
    return files


def _iter_nav_paths(node: object) -> list[str]:
    out: list[str] = []
    if isinstance(node, str):
        out.append(node)
    elif isinstance(node, list):
        for item in node:
            out.extend(_iter_nav_paths(item))
    elif isinstance(node, dict):
        for val in node.values():
            out.extend(_iter_nav_paths(val))
    return out


def find_vaporware_commands(
    doc_files: list[Path], valid_subcommands: set[str], repo: Path
) -> list[str]:
    errors: list[str] = []
    for path in doc_files:
        text = path.read_text(encoding="utf-8")
        rel = path.relative_to(repo).as_posix()
        referenced: set[str] = set()
        for fence in FENCE_RE.findall(text):
            for match in CMD_LINE_RE.finditer(fence):
                referenced.add(match.group(1))
        for match in INLINE_TABLE_CMD_RE.finditer(text):
            referenced.add(match.group(1))
        for sub in sorted(referenced):
            if sub not in valid_subcommands:
                errors.append(
                    f"{rel}: published doc advertises unimplemented command 'slipfork {sub}' "
                    f"(shipped commands: {', '.join(sorted(valid_subcommands - {'--help', '-h', '--version', '-V'}))}; "
                    f"move future UX to docs/design/ or docs/VISION.md)"
                )
    return errors


def main() -> int:
    repo = Path(__file__).resolve().parent.parent
    completed = subprocess.run(
        ["cargo", "run", "-q", "-p", "slipfork", "--", "--help"],
        cwd=repo,
        capture_output=True,
        text=True,
        check=False,
    )
    if completed.returncode != 0:
        print(
            f"check_docs_honesty: failed to run `slipfork --help`:\n{completed.stderr}",
            file=sys.stderr,
        )
        return 2
    valid = extract_help_subcommands(completed.stdout)
    docs = published_doc_files(repo)
    errors = find_vaporware_commands(docs, valid, repo)
    if errors:
        print(
            f"check_docs_honesty: FAILED ({len(errors)} vaporware reference(s)):",
            file=sys.stderr,
        )
        for err in errors:
            print(f"  X {err}", file=sys.stderr)
        return 1
    print(
        f"check_docs_honesty: OK ({len(docs)} published doc(s) match real CLI subcommands)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
