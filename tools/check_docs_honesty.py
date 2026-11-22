#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Verify published user documentation never advertises unimplemented `slipfork <subcommand>`
commands (Zero Vaporware rule).

Published docs are `README.md` and the `docs/**/*.md` pages not named by `zensical.toml`'s
`[project] exclude_docs`; `specs/` drafts and design/research notes are excluded because they
describe target behaviour before it ships. Every page named in the site nav is required to exist,
so a nav entry can never point at a page the site does not build.

The shipped surface is the single `slipfork` binary plus its `slipfork-world-init` boot helper and
the standalone `slipfork-lsp` language server; neither is a `slipfork` subcommand.
"""

from __future__ import annotations

import re
import subprocess
import sys
import tomllib
from pathlib import Path

# Any fenced block, whatever language tag it carries: `console`, `bash`, but also `forkfile`,
# `json` and `text`, so a fence never hides a command from the gate.
FENCE_RE = re.compile(r"```[^\n]*\n(.*?)```", re.DOTALL)
CMD_LINE_RE = re.compile(r"^\s*(?:\$\s+)?slipfork\s+([a-z][a-z0-9_-]*)", re.MULTILINE)
INLINE_TABLE_CMD_RE = re.compile(r"\|\s*`slipfork\s+([a-z][a-z0-9_-]*)")

# A published fork wall-clock claim may never be faster than the measured `~0.8s` end-to-end
# journey asserted in `critical_fork_live` (M3.45.2/3); only the `< 1s` ceiling is honest below
# one second. CoW ioctl latency is a separate 0.3-50ms figure and must be its own sentence.
FORK_CLAIM_RE = re.compile(
    r"(?i)(?:slipfork\s+fork|forked\s+[a-z][a-z0-9_-]*)[^.\n]{0,80}?"
    r"(?<![\d.])(\d+(?:\.\d+)?)\s*(ms|milliseconds?|s|seconds?)\b"
)
MEASURED_FORK_MS = 800.0

# Seeded user-facing docs that ship to the dev VM: same Zero-Vaporware rule as published docs.
SEEDED_DOCS = ("support/demo/README.md",)


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


def _nav(repo: Path) -> list[object]:
    z_path = repo / "zensical.toml"
    if not z_path.is_file():
        return []
    data = tomllib.loads(z_path.read_text(encoding="utf-8"))
    nav = data.get("project", {}).get("nav", [])
    return nav if isinstance(nav, list) else []


def nav_pages(repo: Path) -> list[str]:
    """Every docs-relative page path named by the site nav."""
    return _iter_nav_paths(_nav(repo))


def missing_nav_pages(repo: Path) -> list[str]:
    """Nav entries whose page does not exist on disk, in nav order."""
    docs_dir = repo / "docs"
    return [rel for rel in nav_pages(repo) if not (docs_dir / rel).is_file()]


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


def excluded_prefixes(repo: Path) -> tuple[str, ...]:
    """`zensical.toml`'s `exclude_docs` entries as docs-relative prefixes.

    The site config is the single source of truth: `/archive/` becomes
    `archive/` and `/VISION.md` becomes `VISION.md`. A missing key means
    nothing is excluded.
    """
    z_path = repo / "zensical.toml"
    if not z_path.is_file():
        return ()
    data = tomllib.loads(z_path.read_text(encoding="utf-8"))
    raw = data.get("project", {}).get("exclude_docs", "")
    if not isinstance(raw, str):
        return ()
    out: list[str] = []
    for line in raw.splitlines():
        entry = line.strip().lstrip("/")
        if entry:
            out.append(entry)
    return tuple(out)


def published_doc_files(repo: Path) -> list[Path]:
    files: list[Path] = []
    exclusions = excluded_prefixes(repo)
    readme = repo / "README.md"
    if readme.is_file():
        files.append(readme)
    docs_dir = repo / "docs"
    if docs_dir.is_dir():
        for md in sorted(docs_dir.rglob("*.md")):
            rel = md.relative_to(docs_dir).as_posix()
            if any(rel == p.rstrip("/") or rel.startswith(p) for p in exclusions):
                continue
            files.append(md)
    # Nav pages are required even when a broad `exclude_docs` pattern or a stray suffix would
    # otherwise keep them out of the scan.
    for rel in nav_pages(repo):
        p = docs_dir / rel
        if p not in files and p.is_file():
            files.append(p)
    for rel in SEEDED_DOCS:
        seeded = repo / rel
        if seeded.is_file():
            files.append(seeded)
    return files


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
                    f"move future UX to specs/)"
                )
    return errors


def find_fast_fork_claims(doc_files: list[Path], repo: Path) -> list[str]:
    """Published fork duration claims below the measured `~0.8s` journey."""
    errors: list[str] = []
    for path in doc_files:
        rel = path.relative_to(repo).as_posix()
        for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
            for match in FORK_CLAIM_RE.finditer(line):
                value = float(match.group(1))
                unit = match.group(2).lower()
                claimed_ms = value if unit.startswith(("ms", "milli")) else value * 1000
                if claimed_ms < MEASURED_FORK_MS:
                    errors.append(
                        f"{rel}:{number}: published fork claim '{match.group(0).strip()}' is faster "
                        f"than the measured ~0.8s end-to-end fork (`< 1s` ceiling); quote per-ioctl "
                        f"CoW latency (0.3-50ms) only in its own sentence"
                    )
    return errors


def main() -> int:
    repo = Path(__file__).resolve().parent.parent
    docs_dir = repo / "docs"
    if not docs_dir.is_dir():
        print(
            "check_docs_honesty: docs/ is missing; next: restore the published docs tree",
            file=sys.stderr,
        )
        return 1
    missing = missing_nav_pages(repo)
    if missing:
        print(
            f"check_docs_honesty: FAILED ({len(missing)} nav page(s) missing):",
            file=sys.stderr,
        )
        for rel in missing:
            print(
                f"  X docs/{rel} is in zensical.toml nav but does not exist",
                file=sys.stderr,
            )
        return 1
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
    errors.extend(find_fast_fork_claims(docs, repo))
    if errors:
        print(
            f"check_docs_honesty: FAILED ({len(errors)} vaporware reference(s)):",
            file=sys.stderr,
        )
        for err in errors:
            print(f"  X {err}", file=sys.stderr)
        return 1
    print(
        f"check_docs_honesty: OK ({len(docs)} doc(s) match real CLI subcommands: published "
        f"pages plus {', '.join(SEEDED_DOCS)})"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
