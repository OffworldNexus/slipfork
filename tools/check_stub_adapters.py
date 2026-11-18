#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Refuse production composition roots that wire stub, unavailable, or unimplemented adapters.

A CLI command whose only production adapter is fail-closed is vaporware: it parses, it promises,
and it can never succeed (`slipfork share` reached an `UnavailableTunnelRegistrar` whose `register`
always returned `Err`, and `dag plan` had nothing useful to say). Humans caught both by reading the
wiring; this guard catches the next one mechanically.

Checked tree: every production Rust source under `crates/*/src/**/*.rs`, ignoring anything under a
`tests/` directory and every `#[cfg(test)]` item.

1. Fatal: a bare `todo!()` / `unimplemented!()` in a production source file.
2. Fatal: a stub instantiation in a composition root (`crates/*/src/main.rs`, `crates/*/src/bin/*.rs`,
   `crates/*/src/daemon.rs`): `Type::new(..)`, `Arc::new(Type ..)`, `Box::new(Type ..)`,
   `Rc::new(Type ..)` or `let .. = Type {` where `Type` starts with `Unavailable`, `Unimplemented`,
   `Todo`, `Stub`, or `Fake`.
3. Fatal: a composition root importing such an identifier with `use`.
4. Warning only: `TODO` / `FIXME` comments inside a composition root.

Error variants are not adapters: `EdgeError::TunnelUnavailable { .. }`, `DagError::RemoteResourceUnavailable`,
`TpmdUnavailable` and `RegistryUnavailable` are only ever constructed and matched as variants, never
imported as a type, so they never trip the guard.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from collections.abc import Iterable, Sequence

STUB_PREFIXES = ("Unavailable", "Unimplemented", "Todo", "Stub", "Fake")
STUB_TYPE_RE = rf"(?:{'|'.join(STUB_PREFIXES)})[A-Za-z0-9_]*"

MACRO_RE = re.compile(r"\b(todo|unimplemented)!\s*\(")
STUB_NEW_RE = re.compile(rf"\b({STUB_TYPE_RE})\s*::\s*new\s*\(")
STUB_WRAPPER_RE = re.compile(
    rf"\b(?:Arc|Box|Rc)\s*::\s*new\s*(?:<[^<>]*>\s*)?\(\s*({STUB_TYPE_RE})\b"
)
STUB_LITERAL_RE = re.compile(
    rf"\blet\s+(?:mut\s+)?[A-Za-z_][A-Za-z0-9_]*\s*(?::[^=;]+)?=\s*({STUB_TYPE_RE})\s*\{{"
)
STUB_IMPORT_RE = re.compile(rf"\b({STUB_TYPE_RE})\b")
USE_RE = re.compile(r"^\s*(?:pub(?:\s*\([^)]*\))?\s+)?use\b", re.MULTILINE)
TODO_RE = re.compile(r"\b(TODO|FIXME)\b")
CFG_TEST_RE = re.compile(r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]")
CHAR_LITERAL_RE = re.compile(r"'(?:\\.[^']|[^'\\])'")

FAIL = "FAIL"
WARN = "WARN"

EXIT_OK = 0
EXIT_FINDINGS = 1


class CheckError(RuntimeError):
    """The guard could not inspect the tree."""


@dataclass(frozen=True, slots=True)
class Finding:
    path: str
    line: int
    severity: str
    message: str

    def render(self) -> str:
        return f"{self.path}:{self.line}: {self.severity} {self.message}"


def _line_of(text: str, offset: int) -> int:
    return text.count("\n", 0, offset) + 1


def mask_rust(text: str) -> str:
    """Blank line/block comments and string/char literals, preserving offsets and newlines."""
    chars = list(text)
    length = len(text)
    index = 0

    def blank(start: int, stop: int) -> None:
        for pos in range(start, stop):
            if chars[pos] != "\n":
                chars[pos] = " "

    while index < length:
        char = text[index]
        if char == "/" and text.startswith("//", index):
            stop = text.find("\n", index)
            stop = length if stop < 0 else stop
            blank(index, stop)
            index = stop
        elif char == "/" and text.startswith("/*", index):
            depth = 1
            cursor = index + 2
            while cursor < length and depth:
                if text.startswith("/*", cursor):
                    depth += 1
                    cursor += 2
                elif text.startswith("*/", cursor):
                    depth -= 1
                    cursor += 2
                else:
                    cursor += 1
            blank(index, cursor)
            index = cursor
        elif char in {"r", "b"} and _starts_raw_string(text, index):
            stop = _raw_string_end(text, index)
            blank(index, stop)
            index = stop
        elif char == '"':
            cursor = index + 1
            while cursor < length and text[cursor] != '"':
                cursor += 2 if text[cursor] == "\\" else 1
            cursor = min(cursor + 1, length)
            blank(index, cursor)
            index = cursor
        elif char == "'":
            match = CHAR_LITERAL_RE.match(text, index)
            if match is None:
                index += 1
                continue
            blank(index, match.end())
            index = match.end()
        else:
            index += 1
    return "".join(chars)


def _starts_raw_string(text: str, index: int) -> bool:
    if index > 0 and (text[index - 1].isalnum() or text[index - 1] == "_"):
        return False
    cursor = index
    if text[cursor] == "b":
        cursor += 1
    if cursor >= len(text) or text[cursor] != "r":
        return False
    cursor += 1
    while cursor < len(text) and text[cursor] == "#":
        cursor += 1
    return cursor < len(text) and text[cursor] == '"'


def _raw_string_end(text: str, index: int) -> int:
    cursor = index
    if text[cursor] == "b":
        cursor += 1
    cursor += 1  # the `r`
    hashes = 0
    while cursor < len(text) and text[cursor] == "#":
        hashes += 1
        cursor += 1
    closing = '"' + "#" * hashes
    stop = text.find(closing, cursor + 1)
    return len(text) if stop < 0 else stop + len(closing)


def _item_end(masked: str, start: int) -> int:
    """End offset of the item that begins at `start` (brace block or `;`)."""
    depth = 0
    cursor = start
    while cursor < len(masked):
        char = masked[cursor]
        if char in "([":
            depth += 1
        elif char in ")]":
            depth -= 1
        elif char == "{" and depth == 0:
            return _match_brace(masked, cursor)
        elif char == ";" and depth == 0:
            return cursor + 1
        cursor += 1
    return len(masked)


def _match_brace(masked: str, open_index: int) -> int:
    depth = 0
    cursor = open_index
    while cursor < len(masked):
        char = masked[cursor]
        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                return cursor + 1
        cursor += 1
    return len(masked)


def test_item_spans(masked: str) -> tuple[tuple[int, int], ...]:
    """Spans of `#[cfg(test)]`-attributed items, so test-only code never trips a production rule."""
    spans: list[tuple[int, int]] = []
    cursor = 0
    while (match := CFG_TEST_RE.search(masked, cursor)) is not None:
        end = _item_end(masked, match.end())
        spans.append((match.start(), end))
        cursor = end
    return tuple(spans)


def blank_spans(text: str, spans: Iterable[tuple[int, int]]) -> str:
    chars = list(text)
    for start, end in spans:
        for pos in range(start, min(end, len(chars))):
            if chars[pos] != "\n":
                chars[pos] = " "
    return "".join(chars)


def is_composition_root(relative: PurePosixPath) -> bool:
    parts = relative.parts
    if len(parts) < 4 or parts[0] != "crates" or parts[2] != "src":
        return False
    rest = parts[3:]
    if rest in {("main.rs",), ("daemon.rs",)}:
        return True
    return len(rest) == 2 and rest[0] == "bin" and rest[1].endswith(".rs")


def production_files(root: Path) -> tuple[Path, ...]:
    crates = root / "crates"
    if not crates.is_dir():
        msg = f"{crates} is not a directory: no Rust crate sources to inspect"
        raise CheckError(msg)
    files: list[Path] = []
    for path in sorted(crates.glob("*/src/**/*.rs")):
        relative = path.relative_to(root)
        if not path.is_file() or "tests" in relative.parts:
            continue
        files.append(path)
    return tuple(files)


def _macro_findings(display: str, text: str, code: str) -> list[Finding]:
    findings: list[Finding] = []
    for match in MACRO_RE.finditer(code):
        findings.append(
            Finding(
                display,
                _line_of(text, match.start()),
                FAIL,
                f"`{match.group(1)}!()` in production source — implement the adapter or delete "
                "the caller; a fail-closed placeholder must never ship",
            )
        )
    return findings


def _import_findings(display: str, text: str, code: str) -> list[Finding]:
    findings: list[Finding] = []
    for statement in USE_RE.finditer(code):
        stop = code.find(";", statement.end())
        if stop < 0:
            continue
        body = code[statement.end() : stop]
        for match in STUB_IMPORT_RE.finditer(body):
            findings.append(
                Finding(
                    display,
                    _line_of(text, statement.end() + match.start()),
                    FAIL,
                    f"composition root imports stub adapter `{match.group(1)}` — wire the real "
                    "adapter or remove the import",
                )
            )
    return findings


def _stub_findings(display: str, text: str, code: str) -> list[Finding]:
    findings: list[Finding] = []
    for pattern in (STUB_NEW_RE, STUB_WRAPPER_RE, STUB_LITERAL_RE):
        for match in pattern.finditer(code):
            findings.append(
                Finding(
                    display,
                    _line_of(text, match.start()),
                    FAIL,
                    f"composition root wires unavailable adapter `{match.group(1)}` — a "
                    "fail-closed adapter makes the command vaporware",
                )
            )
    findings.extend(_import_findings(display, text, code))
    return findings


def _todo_findings(display: str, visible: str) -> list[Finding]:
    return [
        Finding(
            display,
            _line_of(visible, match.start()),
            WARN,
            f"{match.group(1)} comment in a composition root — finish the wiring or delete it",
        )
        for match in TODO_RE.finditer(visible)
    ]


def inspect(root: Path) -> tuple[tuple[Finding, ...], int, int]:
    findings: list[Finding] = []
    seen: set[Finding] = set()
    roots = 0
    files = production_files(root)
    for path in files:
        relative = path.relative_to(root)
        display = (root / relative).as_posix()
        text = path.read_text(encoding="utf-8", errors="replace")
        masked = mask_rust(text)
        spans = test_item_spans(masked)
        code = blank_spans(masked, spans)
        file_findings = _macro_findings(display, text, code)
        if is_composition_root(PurePosixPath(relative.as_posix())):
            roots += 1
            file_findings.extend(_stub_findings(display, text, code))
            file_findings.extend(_todo_findings(display, blank_spans(text, spans)))
        for finding in file_findings:
            if finding not in seen:
                seen.add(finding)
                findings.append(finding)
    return tuple(findings), len(files), roots


def _repository_root(start: Path) -> Path:
    result = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        cwd=start,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        msg = (
            f"git rev-parse --show-toplevel failed in {start}: {result.stderr.strip()}"
        )
        raise CheckError(msg)
    return Path(result.stdout.strip())


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Refuse production composition roots that wire stub or unavailable adapters."
    )
    parser.add_argument(
        "--check",
        choices=("all",),
        default="all",
        help="check set to run (only 'all' exists today; it is the default)",
    )
    parser.add_argument(
        "--root",
        type=Path,
        default=None,
        help="repository root to inspect (defaults to the git top level of the CWD)",
    )
    args = parser.parse_args(argv)

    try:
        root = (args.root or _repository_root(Path.cwd())).resolve()
        findings, files, roots = inspect(root)
    except CheckError as error:
        print(f"check_stub_adapters: cannot inspect tree: {error}", file=sys.stderr)
        return EXIT_FINDINGS

    for finding in findings:
        print(finding.render(), file=sys.stderr)
    fatal = sum(1 for finding in findings if finding.severity == FAIL)
    if fatal:
        print(
            f"check_stub_adapters: FAIL ({fatal} fatal, {len(findings) - fatal} warning(s)) "
            f"across {files} production file(s), {roots} composition root(s)",
            file=sys.stderr,
        )
        return EXIT_FINDINGS
    print(
        f"check_stub_adapters: OK ({files} production file(s), {roots} composition root(s))"
    )
    return EXIT_OK


if __name__ == "__main__":
    raise SystemExit(main())
