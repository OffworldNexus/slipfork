#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Mechanically derive the Rust integration-test targets `make rs-test` must list.

`make rs-test` lists every integration binary explicitly (never one `--workspace
--all-targets` sweep) so its parallel and serialized phases stay auditable. This tool is the
mechanical guard for that hand-maintained list and is consumed by the `rs-test-list` target:

- workspace members are read from the root `Cargo.toml` `[workspace] members` (simple globs are
  expanded), never from a hard-coded `crates/*` walk, so a member outside `crates/` cannot
  silently lose coverage;
- each `<member>/tests/*.rs` and `<member>/tests/<name>/main.rs` maps to
  `<package.name>:<test-name>`, keeping two crates that own the same test name distinct;
- `[package] autotests = false` means Cargo only builds the member's `[[test]]` declarations, so
  each declared `[[test]]` is its own target and must be listed (e.g. `slipfork-vcs` declares
  `[[test]] name = "critical_vcs"` and `[[test]] name = "critical_conformance_matrix"`, and
  `make rs-test` lists both), while a `#[path = "..."]` module pulled into a declared target is
  never listed as a separate target. Under `autotests = true` Cargo autodiscovers every
  `tests/*.rs` regardless of `#[path]`, so such a file is listed as its own target;
- in an `autotests = false` member every `tests/*.rs` must be either a declared `[[test]]` path or
  a `#[path = "..."]` module of one; a file that is neither is dead (Cargo never builds it and
  `rs-test` never runs it) and fails the guard, so a future `critical_*.rs` cannot silently stop
  running;
- every `cargo test ... -p <package> --test <name>` line in the `Makefile` is parsed back, and
  the guard fails on a missing pair, a duplicated pair, an orphaned test file, or a classified
  world-booting pair that is not listed with the canonical `-- --test-threads=1` form.

`--world-booting` is required and always enforced; passing the classification explicitly (possibly
empty) is the only way to say which binaries serialize, so a missing classification is a usage
error rather than a silently skipped invariant. There is no other switch: every invocation enforces
every invariant.

The world-booting classification itself is explicit and human-maintained in the `Makefile`; this
tool can only enforce that every classified pair is serialized, not discover new world-booting
suites.
"""

from __future__ import annotations

import argparse
import re
import shlex
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_MAKEFILE = "Makefile"

EXIT_OK = 0
EXIT_VIOLATION = 1
EXIT_CANNOT_INSPECT = 2
EXIT_USAGE = 64

PATH_ATTRIBUTE_RE = re.compile(
    r'^#\[path\s*=\s*"(?P<path>[^"]+)"\s*\]\s*$', re.MULTILINE
)


class ListError(RuntimeError):
    """The guard could not inspect the workspace or the Makefile."""


@dataclass(frozen=True, slots=True)
class Workspace:
    """The workspace's expected targets plus any dead `tests/*.rs` files."""

    pairs: tuple[str, ...]
    orphans: tuple[str, ...]


@dataclass(frozen=True, slots=True)
class ListedTest:
    """One `cargo test -p <package> --test <test>` line of the Makefile."""

    package: str
    test: str
    serialized: bool

    @property
    def pair(self) -> str:
        return f"{self.package}:{self.test}"


class Manifest:
    """One workspace member's `Cargo.toml`, parsed once."""

    def __init__(self, directory: Path) -> None:
        self.directory = directory
        self.path = directory / "Cargo.toml"
        self.data = _load_toml(self.path)
        package = self.data.get("package")
        if not isinstance(package, dict):
            raise ListError(f"{self.path} declares no [package] table")
        name = package.get("name")
        if not isinstance(name, str) or not name:
            raise ListError(f"{self.path} declares no literal [package] name")
        self.name = name
        autotests = package.get("autotests", True)
        if not isinstance(autotests, bool):
            raise ListError(f"{self.path} [package] autotests must be a boolean")
        self.autotests = autotests

    def declared_targets(self) -> tuple[tuple[str, Path], ...]:
        declarations = self.data.get("test", [])
        if not isinstance(declarations, list):
            raise ListError(f"{self.path} [[test]] must be an array of tables")
        targets: list[tuple[str, Path]] = []
        for declaration in declarations:
            if not isinstance(declaration, dict):
                raise ListError(f"{self.path} [[test]] must be an array of tables")
            name = declaration.get("name")
            raw_path = declaration.get("path")
            if raw_path is not None and not isinstance(raw_path, str):
                raise ListError(f"{self.path} [[test]] path must be a string")
            if raw_path is None:
                if not isinstance(name, str) or not name:
                    raise ListError(f"{self.path} [[test]] needs a name or a path")
                raw_path = f"tests/{name}.rs"
            if name is None:
                name = Path(raw_path).stem
            if not isinstance(name, str) or not name:
                raise ListError(f"{self.path} [[test]] name must be a string")
            targets.append((name, (self.directory / raw_path).resolve()))
        return tuple(targets)

    def inlined_modules(self) -> frozenset[Path]:
        """Files the declared `[[test]]` targets pull in with `#[path = "..."]`."""
        modules: set[Path] = set()
        for _, target_path in self.declared_targets():
            try:
                source = target_path.read_text(encoding="utf-8")
            except OSError as error:
                raise ListError(
                    f"cannot read declared test target {target_path}: {error}"
                ) from error
            modules.update(
                (target_path.parent / match.group("path")).resolve()
                for match in PATH_ATTRIBUTE_RE.finditer(source)
            )
        return frozenset(modules)

    def targets(self) -> tuple[tuple[str, ...], tuple[str, ...]]:
        declared = self.declared_targets()
        declared_paths = {path for _, path in declared}
        pairs = {f"{self.name}:{name}" for name, _ in declared}
        tests_dir = self.directory / "tests"
        discovered = _autodiscovered(tests_dir)
        if not self.autotests:
            # Cargo builds only the `[[test]]` declarations. A `tests/*.rs` file that is neither a
            # declared target nor a `#[path = "..."]` module of one is compiled by nothing, so the
            # guard reports it instead of letting a protected `critical_*.rs` go silently dead.
            inlined = self.inlined_modules()
            orphans = tuple(
                f"{self.name}:{source.relative_to(self.directory).as_posix()}"
                for source in discovered
                if source.resolve() not in declared_paths
                and source.resolve() not in inlined
            )
            return tuple(sorted(pairs)), orphans
        for source in discovered:
            target = source.resolve()
            if target in declared_paths:
                # Already counted under the name the manifest declares for this file.
                continue
            pairs.add(f"{self.name}:{_discovered_name(source, tests_dir)}")
        return tuple(sorted(pairs)), ()


def _load_toml(path: Path) -> dict:
    try:
        with path.open("rb") as handle:
            return tomllib.load(handle)
    except OSError as error:
        raise ListError(f"cannot read {path}: {error}") from error
    except tomllib.TOMLDecodeError as error:
        raise ListError(f"invalid TOML in {path}: {error}") from error


def workspace_members(root: Path) -> tuple[Manifest, ...]:
    """Every `[workspace] members` entry, with simple glob patterns expanded."""
    data = _load_toml(root / "Cargo.toml")
    workspace = data.get("workspace")
    if not isinstance(workspace, dict):
        raise ListError(f"{root / 'Cargo.toml'} declares no [workspace] table")
    entries = workspace.get("members")
    if not isinstance(entries, list):
        raise ListError(f"{root / 'Cargo.toml'} declares no [workspace] members")
    directories: list[Path] = []
    seen: set[Path] = set()
    for entry in entries:
        if not isinstance(entry, str):
            raise ListError(
                f"{root / 'Cargo.toml'} [workspace] members must be strings"
            )
        if any(character in entry for character in "*?["):
            # A glob only declares member *candidates*; directories without a manifest
            # (docs stubs, keep-files) are not packages.
            matched = sorted(
                path for path in root.glob(entry) if (path / "Cargo.toml").is_file()
            )
            if not matched:
                raise ListError(f"workspace member glob {entry!r} matches no directory")
        else:
            matched = [root / entry]
            if not (matched[0] / "Cargo.toml").is_file():
                raise ListError(f"workspace member {matched[0]} has no Cargo.toml")
        for directory in matched:
            resolved = directory.resolve()
            if resolved not in seen:
                seen.add(resolved)
                directories.append(resolved)
    return tuple(Manifest(directory) for directory in directories)


def inspect_workspace(root: Path) -> Workspace:
    """The full `<package>:<test>` set Cargo would build, plus dead `tests/*.rs` files."""
    pairs: set[str] = set()
    orphans: list[str] = []
    for member in workspace_members(root):
        member_pairs, member_orphans = member.targets()
        pairs.update(member_pairs)
        orphans.extend(member_orphans)
    return Workspace(tuple(sorted(pairs)), tuple(sorted(orphans)))


def _autodiscovered(tests_dir: Path) -> tuple[Path, ...]:
    if not tests_dir.is_dir():
        return ()
    return (
        *sorted(tests_dir.glob("*.rs")),
        *sorted(tests_dir.glob("*/main.rs")),
    )


def _discovered_name(source: Path, tests_dir: Path) -> str:
    # Cargo autodiscovers `tests/<name>/main.rs` as target `<name>`, but a top-level
    # `tests/main.rs` as target `main`.
    if source.name == "main.rs" and source.parent != tests_dir:
        return source.parent.name
    return source.stem


def parse_listed_tests(makefile: Path) -> tuple[ListedTest, ...]:
    """Parse every `cargo test ... -p <package> --test <name>` recipe line."""
    try:
        lines = makefile.read_text(encoding="utf-8").splitlines()
    except OSError as error:
        raise ListError(f"cannot read {makefile}: {error}") from error
    listed: list[ListedTest] = []
    for line in lines:
        if not line.startswith("\t"):
            continue
        command = line.strip().removeprefix("@").strip()
        if not command.startswith("cargo test"):
            continue
        try:
            tokens = shlex.split(command)
        except ValueError as error:
            raise ListError(f"cannot tokenize {command!r}: {error}") from error
        package: str | None = None
        test: str | None = None
        serialized = False
        for index, token in enumerate(tokens):
            if token in {"-p", "--package"} and index + 1 < len(tokens):
                package = tokens[index + 1]
            elif token == "--test" and index + 1 < len(tokens):
                test = tokens[index + 1]
            elif token == "--" and index + 1 < len(tokens):
                serialized = _serialized(tokens[index + 1 :])
        if package is not None and test is not None:
            listed.append(ListedTest(package, test, serialized))
    return tuple(listed)


def _serialized(arguments: list[str]) -> bool:
    return "--test-threads=1" in arguments


def missing_pairs(
    expected: tuple[str, ...], listed: tuple[ListedTest, ...]
) -> tuple[str, ...]:
    present = {entry.pair for entry in listed}
    return tuple(pair for pair in expected if pair not in present)


def duplicate_pairs(listed: tuple[ListedTest, ...]) -> tuple[str, ...]:
    counts: dict[str, int] = {}
    for entry in listed:
        counts[entry.pair] = counts.get(entry.pair, 0) + 1
    return tuple(sorted(pair for pair, count in counts.items() if count > 1))


def unserialized_pairs(
    world_booting: tuple[str, ...], listed: tuple[ListedTest, ...]
) -> tuple[str, ...]:
    by_pair: dict[str, list[ListedTest]] = {}
    for entry in listed:
        by_pair.setdefault(entry.pair, []).append(entry)
    offending: list[str] = []
    for pair in world_booting:
        entries = by_pair.get(pair, [])
        if not entries:
            offending.append(f"{pair}(not listed)")
        elif not any(entry.serialized for entry in entries):
            offending.append(pair)
    return tuple(offending)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=Path, default=REPO_ROOT, help="repository root")
    parser.add_argument(
        "--makefile",
        default=DEFAULT_MAKEFILE,
        help="Makefile (relative to --root unless absolute) holding the rs-test recipe",
    )
    parser.add_argument(
        "--world-booting",
        nargs="*",
        default=None,
        metavar="PACKAGE:TEST",
        help="required human-maintained world-booting classification to require serialized",
    )
    arguments = parser.parse_args(argv)

    root = arguments.root.resolve()
    makefile = Path(arguments.makefile)
    if not makefile.is_absolute():
        makefile = root / makefile
    if arguments.world_booting is None:
        parser.print_usage(sys.stderr)
        print(
            "rs-test-list: --world-booting is required (pass an empty classification "
            "when no suite boots a world)",
            file=sys.stderr,
        )
        return EXIT_USAGE
    world_booting = tuple(arguments.world_booting)

    try:
        workspace = inspect_workspace(root)
        listed = parse_listed_tests(makefile)
    except ListError as error:
        print(f"rs-test-list: cannot inspect: {error}", file=sys.stderr)
        return EXIT_CANNOT_INSPECT

    failures: list[str] = []
    missing = missing_pairs(workspace.pairs, listed)
    if missing:
        failures.append(
            "rs-test-list: integration test binaries missing from rs-test: "
            + " ".join(missing)
        )
    duplicates = duplicate_pairs(listed)
    if duplicates:
        failures.append(
            "rs-test-list: integration binary listed more than once in rs-test: "
            + " ".join(duplicates)
        )
    if workspace.orphans:
        failures.append(
            "rs-test-list: files under tests/ are neither declared [[test]] targets "
            "nor #[path] modules of one: " + " ".join(workspace.orphans)
        )
    unserialized = unserialized_pairs(world_booting, listed)
    if unserialized:
        failures.append(
            "rs-test-list: world-booting binaries must be listed with "
            "-- --test-threads=1: " + " ".join(unserialized)
        )

    if failures:
        print("\n".join(failures), file=sys.stderr)
        return EXIT_VIOLATION
    return EXIT_OK


if __name__ == "__main__":
    raise SystemExit(main())
