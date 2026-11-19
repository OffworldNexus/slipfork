#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Verify the Zed extension that ships Forkfile editor support.

`editors/zed` must point at a real tree-sitter grammar with a committed
`src/parser.c`, `src/grammar.json`, and `src/node-types.json`, every
Tree-sitter query and corpus case must be well formed, and the language server
it registers must be exactly the shipped `slipfork lsp` with an empty env.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

EXIT_OK = 0
EXIT_FINDINGS = 1

CAPTURE_RE = re.compile(r"@([A-Za-z_][A-Za-z0-9_.!?-]*)")
LITERAL_RE = re.compile(r'"((?:[^"\\]|\\.)*)"')
CORPUS_HEADER_RE = re.compile(r"^={3,}\s*$")
CORPUS_BODY_RE = re.compile(r"^-{3,}\s*$")
REVISION_MIN_LENGTH = 7


def unbalanced(text: str) -> bool:
    """Report whether `text` has unbalanced parens or an unterminated string."""
    depth = 0
    in_string = False
    escaped = False
    for character in text:
        if in_string:
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == '"':
                in_string = False
            continue
        if character == '"':
            in_string = True
        elif character == "(":
            depth += 1
        elif character == ")":
            depth -= 1
            if depth < 0:
                return True
    return depth != 0 or in_string


@dataclass(frozen=True, slots=True)
class Finding:
    """One failed invariant plus the concrete remedy shown to the user."""

    message: str
    next_step: str

    def render(self) -> str:
        return f"{self.message}\n  next: {self.next_step}"


@dataclass
class ZedExtensionAudit:
    """Audit one checkout's Zed extension, grammar, and LSP wiring."""

    root: Path
    findings: list[Finding] = field(default_factory=list)

    @property
    def extension_dir(self) -> Path:
        return self.root / "editors" / "zed"

    def fail(self, message: str, next_step: str) -> None:
        self.findings.append(Finding(message, next_step))

    def run(self) -> list[Finding]:
        manifest = self._read_toml(self.extension_dir / "extension.toml")
        if manifest is not None:
            self._check_extension_manifest(manifest)
            grammar_dir = self._grammar_dir(manifest)
            if grammar_dir is not None:
                self._check_grammar(grammar_dir)
                self._check_language(grammar_dir)
        self._check_lsp()
        return self.findings

    def _rel(self, path: Path) -> str:
        try:
            return path.resolve().relative_to(self.root).as_posix()
        except ValueError:
            return path.as_posix()

    def _read_text(self, path: Path) -> str | None:
        try:
            return path.read_text(encoding="utf-8")
        except FileNotFoundError:
            self.fail(f"{self._rel(path)} is missing.", f"restore {self._rel(path)}.")
        except OSError as error:
            self.fail(
                f"{self._rel(path)} is unreadable ({error.strerror}).",
                f"fix permissions on {self._rel(path)}.",
            )
        return None

    def _read_toml(self, path: Path) -> dict[str, object] | None:
        source = self._read_text(path)
        if source is None:
            return None
        try:
            return tomllib.loads(source)
        except tomllib.TOMLDecodeError as error:
            self.fail(
                f"{self._rel(path)} is not valid TOML ({error}).",
                f"fix the TOML syntax in {self._rel(path)}.",
            )
            return None

    def _check_extension_manifest(self, manifest: dict[str, object]) -> None:
        manifest_path = self._rel(self.extension_dir / "extension.toml")
        if manifest.get("id") != "forkfile":
            self.fail(
                f'{manifest_path} must declare id = "forkfile".',
                'set id = "forkfile" so Zed can install the extension.',
            )
        if manifest.get("schema_version") != 1:
            self.fail(
                f"{manifest_path} must declare schema_version = 1.",
                "set schema_version = 1.",
            )
        servers = manifest.get("language_servers")
        server = servers.get("slipfork-lsp") if isinstance(servers, dict) else None
        if not isinstance(server, dict):
            self.fail(
                f"{manifest_path} has no [language_servers.slipfork-lsp] table.",
                "register the shipped `slipfork lsp` server for Forkfile.",
            )
        else:
            if server.get("languages") != ["Forkfile"]:
                self.fail(
                    f'{manifest_path} slipfork-lsp must target ["Forkfile"].',
                    "point the language server at the Forkfile language.",
                )
            language_ids = server.get("language_ids")
            if not isinstance(language_ids, dict) or (
                language_ids.get("Forkfile") != "forkfile"
            ):
                self.fail(
                    f'{manifest_path} must map Forkfile -> "forkfile".',
                    "add [language_servers.slipfork-lsp.language_ids] with "
                    'Forkfile = "forkfile".',
                )
        self._check_extension_cargo()

    def _check_extension_cargo(self) -> None:
        path = self.extension_dir / "Cargo.toml"
        source = self._read_text(path)
        if source is None:
            return
        workspace = re.search(r"(?m)^\[workspace\]\s*$", source)
        if workspace is None:
            self.fail(
                f"{self._rel(path)} must declare an empty [workspace] table.",
                "append an empty [workspace] so Zed builds the extension standalone.",
            )
        elif re.search(r"(?ms)^\[workspace\]\s*\n(?!\[).*\bmembers\b", source):
            self.fail(
                f"{self._rel(path)} [workspace] must stay empty.",
                "remove workspace members; the extension builds as a standalone cdylib.",
            )
        if 'crate-type = ["cdylib"]' not in source:
            self.fail(
                f'{self._rel(path)} must build crate-type = ["cdylib"].',
                'add [lib] crate-type = ["cdylib"].',
            )
        if not re.search(r'zed_extension_api\s*=\s*"0\.7', source):
            self.fail(
                f"{self._rel(path)} must depend on zed_extension_api 0.7.",
                'set zed_extension_api = "0.7.0" under [dependencies].',
            )

    def _grammar_dir(self, manifest: dict[str, object]) -> Path | None:
        grammars = manifest.get("grammars")
        entry = grammars.get("forkfile") if isinstance(grammars, dict) else None
        manifest_path = self._rel(self.extension_dir / "extension.toml")
        if not isinstance(entry, dict):
            self.fail(
                f"{manifest_path} has no [grammars.forkfile] table.",
                "register the forkfile grammar with a path and a pinned rev.",
            )
            return None
        grammar_path = entry.get("path")
        if not isinstance(grammar_path, str) or not grammar_path:
            self.fail(
                f"{manifest_path} [grammars.forkfile] must declare a path.",
                'point path at the in-repo grammar, e.g. "../tree-sitter-forkfile".',
            )
            return None
        revision = entry.get("rev")
        if not isinstance(revision, str) or len(revision) < REVISION_MIN_LENGTH:
            self.fail(
                f"{manifest_path} [grammars.forkfile] must pin a rev.",
                "pin the grammar revision Zed clones/vendors.",
            )
        for candidate in (self.extension_dir / grammar_path, self.root / grammar_path):
            if candidate.is_dir():
                return candidate.resolve()
        self.fail(
            f"{manifest_path} grammar path '{grammar_path}' does not exist.",
            "point [grammars.forkfile].path at editors/tree-sitter-forkfile.",
        )
        return None

    def _check_grammar(self, grammar_dir: Path) -> None:
        grammar_js = grammar_dir / "grammar.js"
        source = self._read_text(grammar_js)
        if source is not None and not re.search(
            r"""name:\s*['"]forkfile['"]""", source
        ):
            self.fail(
                f"{self._rel(grammar_js)} must declare name: 'forkfile'.",
                "keep grammar.js as the single source of truth for the grammar.",
            )
        self._check_generated(grammar_dir)
        self._check_corpus(grammar_dir)

    def _check_generated(self, grammar_dir: Path) -> None:
        parser = grammar_dir / "src" / "parser.c"
        generated: str | None = None
        try:
            generated = parser.read_text(encoding="utf-8", errors="replace")
        except FileNotFoundError:
            self.fail(
                f"{self._rel(parser)} is not committed.",
                "run `bunx tree-sitter-cli generate` and commit src/parser.c.",
            )
        except OSError as error:
            self.fail(
                f"{self._rel(parser)} is unreadable ({error.strerror}).",
                "restore the committed parser.",
            )
        if generated is not None and not generated.strip():
            self.fail(
                f"{self._rel(parser)} is empty.",
                "regenerate the parser and commit the real output.",
            )
        elif generated is not None and "tree_sitter_forkfile" not in generated:
            self.fail(
                f'{self._rel(parser)} does not define "tree_sitter_forkfile".',
                "regenerate src/parser.c from this grammar.js.",
            )
        for name in ("grammar.json", "node-types.json"):
            path = grammar_dir / "src" / name
            payload = self._read_json(path)
            if payload is None:
                continue
            if name == "grammar.json" and payload.get("name") != "forkfile":
                self.fail(
                    f'{self._rel(path)} must describe the "forkfile" grammar.',
                    "regenerate src/grammar.json from grammar.js.",
                )

    def _read_json(self, path: Path) -> dict[str, object] | list[object] | None:
        rel = self._rel(path)
        try:
            source = path.read_text(encoding="utf-8")
        except FileNotFoundError:
            self.fail(
                f"{rel} is not committed.",
                "run `bunx tree-sitter-cli generate` and commit src/.",
            )
            return None
        except OSError as error:
            self.fail(
                f"{rel} is unreadable ({error.strerror}).",
                "restore the generated JSON sources.",
            )
            return None
        try:
            payload = json.loads(source)
        except json.JSONDecodeError as error:
            self.fail(
                f"{rel} is not valid JSON ({error}).",
                "regenerate src/ with `bunx tree-sitter-cli generate`.",
            )
            return None
        if not payload:
            self.fail(
                f"{rel} is empty.",
                "regenerate src/ with `bunx tree-sitter-cli generate`.",
            )
            return None
        return payload

    def _check_corpus(self, grammar_dir: Path) -> None:
        corpus = grammar_dir / "test" / "corpus"
        files = sorted(corpus.glob("*.txt")) if corpus.is_dir() else []
        if not files:
            self.fail(
                f"{self._rel(corpus)} has no corpus cases.",
                "add tree-sitter corpus files that exercise every statement.",
            )
            return
        for path in files:
            source = self._read_text(path)
            if source is not None:
                self._check_corpus_file(path, source)

    def _check_corpus_file(self, path: Path, source: str) -> None:
        rel = self._rel(path)
        lines = source.splitlines()
        index = 0
        cases = 0
        while index < len(lines) and not CORPUS_HEADER_RE.match(lines[index]):
            if lines[index].strip():
                self.fail(
                    f"{rel}:{index + 1} has text before the first case header.",
                    "start every corpus file with a `====` case header.",
                )
                return
            index += 1
        while index < len(lines):
            header_line = index + 1
            index += 1
            title = lines[index].strip() if index < len(lines) else ""
            if not title:
                self.fail(
                    f"{rel}:{header_line} has an empty case title.",
                    "name every corpus case between its two `====` headers.",
                )
                return
            index += 1
            if index >= len(lines) or not CORPUS_HEADER_RE.match(lines[index]):
                self.fail(
                    f"{rel}:{header_line} case `{title}` has no closing `====` header.",
                    "wrap the case title in two `====` header lines.",
                )
                return
            index += 1
            source_start = index
            while index < len(lines) and not (
                CORPUS_BODY_RE.match(lines[index])
                or CORPUS_HEADER_RE.match(lines[index])
            ):
                index += 1
            body = lines[source_start:index]
            if index >= len(lines) or not CORPUS_BODY_RE.match(lines[index]):
                self.fail(
                    f"{rel}:{header_line} case `{title}` has no `---` separator.",
                    "separate the source and the expected tree with a `---` line.",
                )
                return
            index += 1
            tree_start = index
            while index < len(lines) and not CORPUS_HEADER_RE.match(lines[index]):
                index += 1
            tree = [line for line in lines[tree_start:index] if line.strip()]
            cases += 1
            if not any(line.strip() for line in body):
                self.fail(
                    f"{rel}:{header_line} case `{title}` has an empty source.",
                    "add the Forkfile source this case parses.",
                )
                return
            if not tree:
                self.fail(
                    f"{rel}:{header_line} case `{title}` has no expected tree.",
                    "add the expected s-expression tree after the `---` line.",
                )
                return
            if not tree[0].lstrip().startswith("("):
                self.fail(
                    f"{rel}:{header_line} case `{title}` expected tree is not an s-expression.",
                    "start the expected tree with `(source_file`.",
                )
                return
            if unbalanced("\n".join(tree)):
                self.fail(
                    f"{rel}:{header_line} case `{title}` has an unbalanced expected tree.",
                    "balance the parentheses in the expected s-expression.",
                )
                return
        if cases == 0:
            self.fail(
                f"{rel} contains no corpus cases.",
                "add at least one `====` case that parses a Forkfile snippet.",
            )

    def _check_language(self, grammar_dir: Path) -> None:
        language_dir = self.extension_dir / "languages" / "forkfile"
        config_path = language_dir / "config.toml"
        config = self._read_toml(config_path)
        if config is not None:
            if config.get("name") != "Forkfile":
                self.fail(
                    f'{self._rel(config_path)} must declare name = "Forkfile".',
                    'set name = "Forkfile".',
                )
            if config.get("grammar") != "forkfile":
                self.fail(
                    f'{self._rel(config_path)} must declare grammar = "forkfile".',
                    'set grammar = "forkfile".',
                )
            suffixes = config.get("path_suffixes")
            if not isinstance(suffixes, list) or not {"Forkfile", "forkfile"} <= set(
                suffixes
            ):
                self.fail(
                    f'{self._rel(config_path)} must claim "Forkfile" and "forkfile".',
                    'set path_suffixes = ["Forkfile", "forkfile"].',
                )
            comments = config.get("line_comments")
            if not isinstance(comments, list) or "# " not in comments:
                self.fail(
                    f'{self._rel(config_path)} must declare "# " line comments.',
                    'set line_comments = ["# "].',
                )

        grammar_query = self._check_query(grammar_dir / "queries" / "highlights.scm")
        zed_query = self._check_query(language_dir / "highlights.scm")
        self._check_query(language_dir / "brackets.scm")
        if grammar_query is None or zed_query is None:
            return
        missing = sorted(grammar_query - zed_query)
        extra = sorted(zed_query - grammar_query)
        if missing or extra:
            detail = ", ".join(
                part
                for part in (
                    f"missing {missing}" if missing else "",
                    f"only in Zed {extra}" if extra else "",
                )
                if part
            )
            self.fail(
                f"{self._rel(language_dir / 'highlights.scm')} drifted from the "
                f"grammar query ({detail}).",
                "copy editors/tree-sitter-forkfile/queries/highlights.scm verbatim.",
            )

    def _check_query(self, path: Path) -> set[str] | None:
        source = self._read_text(path)
        if source is None:
            return None
        rel = self._rel(path)
        depth = 0
        in_string = False
        escaped = False
        for number, line in enumerate(source.splitlines(), start=1):
            for character in line:
                if in_string:
                    if escaped:
                        escaped = False
                    elif character == "\\":
                        escaped = True
                    elif character == '"':
                        in_string = False
                    continue
                if character == '"':
                    in_string = True
                elif character == ";":
                    break
                elif character == "(":
                    depth += 1
                elif character == ")":
                    depth -= 1
                    if depth < 0:
                        self.fail(
                            f"{rel}:{number} closes a query before it opens.",
                            "balance the parentheses in the Tree-sitter query.",
                        )
                        return None
        if depth != 0 or in_string:
            self.fail(
                f"{rel} has unbalanced parentheses or an unterminated string.",
                "balance the parentheses in the Tree-sitter query.",
            )
            return None
        captures = set(CAPTURE_RE.findall(source))
        if not captures:
            self.fail(
                f"{rel} defines no @captures.",
                "add Tree-sitter captures for the nodes you want highlighted.",
            )
            return None
        return captures

    def _check_lsp(self) -> None:
        lib = self.extension_dir / "src" / "lib.rs"
        source = self._read_text(lib)
        if source is not None:
            if 'which("slipfork")' not in source:
                self.fail(
                    f"{self._rel(lib)} never resolves `slipfork` from the worktree.",
                    'call worktree.which("slipfork") in language_server_command().',
                )
            command = command_body(source)
            if command is None:
                self.fail(
                    f"{self._rel(lib)} never returns `zed::Command` from "
                    "language_server_command().",
                    'return zed::Command { command, args: vec!["lsp".to_string()], '
                    "env: Vec::new() }.",
                )
            else:
                self._check_command(lib, command)
        commands = self.root / "crates" / "slipfork" / "src" / "cli" / "commands.rs"
        cli_source = self._read_text(commands)
        if cli_source is not None and not re.search(r"\bLsp\b", cli_source):
            self.fail(
                "the shipped CLI does not expose `slipfork lsp`.",
                "add the Lsp command to crates/slipfork/src/cli/commands.rs.",
            )

    def _check_command(self, lib: Path, command: str) -> None:
        rel = self._rel(lib)
        args = command_field(command, "args")
        literals = LITERAL_RE.findall(args) if args is not None else []
        if literals != ["lsp"]:
            self.fail(
                f'{rel} never passes the "lsp" argument.',
                'return args: vec!["lsp".to_string()] from the language server.',
            )
        env = command_field(command, "env")
        env_literals = LITERAL_RE.findall(env) if env is not None else ["<missing>"]
        compact = re.sub(r"\s+", "", env or "")
        if env is None or env_literals or compact not in {"Vec::new()", "vec![]"}:
            self.fail(
                f"{rel} must pass exactly an empty env allowlist to `slipfork lsp`.",
                "return env: Vec::new() so the server inherits only the worktree env.",
            )


def command_body(source: str) -> str | None:
    """Return the body of the `Ok(zed::Command { … })` struct literal."""
    marker = source.find("Ok(zed::Command")
    if marker < 0:
        marker = source.find("zed::Command {")
    if marker < 0:
        return None
    brace = source.find("{", marker)
    if brace < 0:
        return None
    depth = 0
    in_string = False
    escaped = False
    for offset in range(brace, len(source)):
        character = source[offset]
        if in_string:
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == '"':
                in_string = False
            continue
        if character == '"':
            in_string = True
        elif character == "{":
            depth += 1
        elif character == "}":
            depth -= 1
            if depth == 0:
                return source[brace + 1 : offset]
    return None


def command_field(body: str, name: str) -> str | None:
    """Return the raw expression assigned to `name:` inside a struct literal."""
    match = re.search(rf"(?<![A-Za-z0-9_]){name}\s*:", body)
    if match is None:
        return None
    depth = 0
    in_string = False
    escaped = False
    for offset in range(match.end(), len(body)):
        character = body[offset]
        if in_string:
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == '"':
                in_string = False
            continue
        if character == '"':
            in_string = True
        elif character in "([{":
            depth += 1
        elif character in ")]}":
            if depth == 0:
                return body[match.end() : offset]
            depth -= 1
        elif character == "," and depth == 0:
            return body[match.end() : offset]
    return body[match.end() :]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "root", nargs="?", default=".", help="repository checkout to audit"
    )
    arguments = parser.parse_args(argv)
    audit = ZedExtensionAudit(Path(arguments.root).resolve())
    findings = audit.run()
    if findings:
        for finding in findings:
            print(finding.render(), file=sys.stderr)
        return EXIT_FINDINGS
    print(
        "✓ Zed extension: grammar, generated sources, corpus, queries, "
        "and `slipfork lsp` wiring verified."
    )
    return EXIT_OK


if __name__ == "__main__":
    sys.exit(main())
