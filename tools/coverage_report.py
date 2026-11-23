#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Line coverage for instrumented Slipfork Rust tests.

Builds `cargo test <TESTS> --no-run --locked` with `-C instrument-coverage` into `target/cov`
(never `target/debug`), runs the resulting test executables, merges their profiles with rustup's
`llvm-profdata`, and prints, per file in `--files`:

- the line percentage (covered lines / executable lines);
- the exact uncovered line ranges (`llvm-cov export --format=lcov`).

Every slow child is spawned and its progress forwarded live with an elapsed-time column, so a
20-minute instrumented rebuild reads as work in flight rather than a hang. Output carries no ANSI
escapes unless stderr is a terminal and `NO_COLOR` is unset.

Non-test `bin` artefacts built for those tests (for example the `slipfork` daemon or
`slipfork-world-init` that an integration test spawns) are measured as well, so code reached only
through a spawned binary is counted instead of reading 0%. A file seen only in such a bin object
with no executed line is reported as `not exercised in this run (live suite gated)` — never as
0% dead code — and `--live-suites` names the gated suites in the summary. A test-owned file that
still reads `0.0%` prints the `env_clear()` caveat instead.

Each object is read on its own and the per-line execution counts are unioned: `llvm-cov export`
given many objects at once silently reports only a couple of them, which understates or drops whole
crates.

Python sources are not measured offline: `uvx --offline coverage` is not cached, so `.py` entries
are skipped with a stderr note; audit each one against its `TESTING_PLAN.md` story and E2E test.

The first instrumented build populates `target/cov` and is slow once: the whole dependency graph is
rebuilt with instrumentation. Warm runs reuse it and only rebuild what changed.

Uncovered lines are never an error here. The reviewer decides whether they are dead code (delete
it) or an untested user story (extend the E2E test); the exit status only fails on a tooling,
build or test failure.

Usage:
    uv run --no-project tools/coverage_report.py \
      --tests "-p slipfork --tests" \
      --files "crates/slipfork/src/lib.rs"
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shlex
import subprocess
import sys
import threading
import time
from pathlib import Path
from typing import TextIO

TARGET_DIR = Path("target/cov")
PROFRAW_DIR = TARGET_DIR / "profraw"
PASSED = re.compile(r"(\d+) passed")
COLOR = sys.stderr.isatty() and "NO_COLOR" not in os.environ


class CoverageError(RuntimeError):
    """The instrumented build, test run or report could not complete."""


def _paint(text: str, code: str) -> str:
    return f"\x1b[{code}m{text}\x1b[0m" if COLOR else text


def _clock(started: float) -> str:
    seconds = time.monotonic() - started
    if seconds < 60:
        return f"{seconds:5.1f}s"
    minutes, rest = divmod(int(seconds), 60)
    return f"{minutes}m{rest:02d}s"


def _capture(argv: list[str], env: dict[str, str]) -> str:
    """Run a quick ``argv`` to completion and return stdout, refusing a non-zero exit."""
    done = subprocess.run(argv, env=env, text=True, capture_output=True, check=False)
    if done.returncode != 0:
        tail = (done.stderr or done.stdout).strip().splitlines()[-12:]
        raise CoverageError(f"{argv[0]} exited {done.returncode}: " + " | ".join(tail))
    return done.stdout


def _pump(
    pipe: TextIO, echo: bool, collected: list[str] | None, started: float
) -> None:
    """Drain one child pipe, optionally echoing each line with its elapsed stamp."""
    for line in iter(pipe.readline, ""):
        if collected is not None:
            collected.append(line)
        if echo:
            print(f"{_clock(started)} │ {line.rstrip()}", file=sys.stderr, flush=True)


def _spawn(
    argv: list[str], env: dict[str, str], *, echo_stdout: bool
) -> tuple[int, str, float]:
    """Run one slow child, streaming both pipes live; return (exit, stdout, seconds)."""
    started = time.monotonic()
    process = subprocess.Popen(
        argv,
        env=env,
        text=True,
        bufsize=1,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    stdout: list[str] = []
    readers = [
        threading.Thread(
            target=_pump,
            args=(process.stdout, echo_stdout, stdout, started),
            daemon=True,
        ),
        threading.Thread(
            target=_pump, args=(process.stderr, True, None, started), daemon=True
        ),
    ]
    for reader in readers:
        reader.start()
    code = process.wait()
    for reader in readers:
        reader.join()
    return code, "".join(stdout), time.monotonic() - started


def _phase(message: str) -> None:
    """Announce a wait on stderr before it starts (charter CLI rule)."""
    print(f"{_paint('coverage:', '1;36')} {message}", file=sys.stderr, flush=True)


def _plural(count: int, noun: str) -> str:
    return f"{count} {noun}" if count == 1 else f"{count} {noun}s"


def _host(env: dict[str, str]) -> str:
    for line in _capture(["rustc", "-vV"], env).splitlines():
        if line.startswith("host: "):
            return line[len("host: ") :].strip()
    raise CoverageError("rustc -vV reports no host triple")


def _toolchain(env: dict[str, str]) -> tuple[Path, Path]:
    """Return rustup's `llvm-profdata` and `llvm-cov` for the active toolchain."""
    sysroot = Path(_capture(["rustc", "--print", "sysroot"], env).strip())
    bin_dir = sysroot / "lib" / "rustlib" / _host(env) / "bin"
    profdata = bin_dir / "llvm-profdata"
    cov = bin_dir / "llvm-cov"
    for tool in (profdata, cov):
        if not tool.is_file():
            raise CoverageError(
                f"{tool} is missing; run `rustup component add llvm-tools`"
            )
    return profdata, cov


def _build_targets(
    tests: list[str], env: dict[str, str]
) -> tuple[list[Path], list[Path]]:
    """Build the requested targets; return (test executables, bin objects)."""
    _phase(
        "building instrumented test targets into target/cov "
        "(first run rebuilds the dependency graph)…"
    )
    argv = ["cargo", "test", *tests, "--no-run", "--locked", "--message-format=json"]
    code, output, seconds = _spawn(argv, env, echo_stdout=False)
    if code != 0:
        raise CoverageError(
            f"cargo test --no-run exited {code} after {_clock_done(seconds)}; "
            "next: fix the build error printed above"
        )
    _phase(f"instrumented build finished in {_clock_done(seconds)}")
    executables: list[Path] = []
    binaries: list[Path] = []
    for line in output.splitlines():
        if not line.startswith("{"):
            continue
        message = json.loads(line)
        if message.get("reason") != "compiler-artifact":
            continue
        executable = message.get("executable")
        if not executable:
            continue
        profile = message.get("profile") or {}
        if profile.get("test"):
            executables.append(Path(executable))
        elif "bin" in ((message.get("target") or {}).get("kind") or []):
            binaries.append(Path(executable))
    if not executables:
        raise CoverageError("cargo test --no-run built no test executable")
    target = TARGET_DIR.resolve()
    for artifact in [*binaries, *executables]:
        if target not in artifact.resolve().parents:
            raise CoverageError(
                f"{artifact} is outside {TARGET_DIR}: set CARGO_TARGET_DIR"
            )
    return list(dict.fromkeys(executables)), list(dict.fromkeys(binaries))


def _clock_done(seconds: float) -> str:
    if seconds < 60:
        return f"{seconds:.1f}s"
    minutes, rest = divmod(int(seconds), 60)
    return f"{minutes}m{rest:02d}s"


def _run_tests(executables: list[Path], env: dict[str, str]) -> int:
    """Run every test binary under the profraw sink and count passing tests."""
    passed = 0
    total = len(executables)
    for index, executable in enumerate(executables, start=1):
        _phase(f"[{index}/{total}] running {executable.name}")
        code, output, seconds = _spawn([str(executable)], env, echo_stdout=True)
        if code != 0:
            raise CoverageError(
                f"{executable.name} exited {code} after {_clock_done(seconds)}; "
                "next: fix the failing test printed above"
            )
        passed += sum(int(count) for count in PASSED.findall(output))
    return passed


def _merge(profdata_tool: Path, output: Path, env: dict[str, str]) -> None:
    profraws = sorted(PROFRAW_DIR.glob("*.profraw"))
    if not profraws:
        raise CoverageError(f"no .profraw under {PROFRAW_DIR}: did the tests run?")
    _phase(f"merging {_plural(len(profraws), 'profile')}…")
    argv = [str(profdata_tool), "merge", "-sparse", "-o", str(output)]
    code, _, seconds = _spawn(
        [*argv, *(str(profraw) for profraw in profraws)], env, echo_stdout=False
    )
    if code != 0:
        raise CoverageError(
            f"llvm-profdata merge exited {code} after {_clock_done(seconds)}"
        )


def _lcov(cov_tool: Path, profdata: Path, obj: Path, env: dict[str, str]) -> str:
    """Per-line execution counts for one instrumented object, as lcov text."""
    argv = [
        str(cov_tool),
        "export",
        "-format=lcov",
        f"-instr-profile={profdata}",
        str(obj),
    ]
    return _capture(argv, env)


def _same_file(reported: str, wanted: str) -> bool:
    normalized = reported.replace("\\", "/").removeprefix("./")
    return normalized == wanted or normalized.endswith("/" + wanted)


def _profile(
    cov_tool: Path,
    profdata: Path,
    objects: list[tuple[bool, Path]],
    files: list[str],
    env: dict[str, str],
) -> tuple[dict[str, dict[int, int]], set[str]]:
    """Union per-line counts across every object, remembering which files bins own.

    ``objects`` is ``(is_test, path)``; the bin-owner set lets the report tell a file that only a
    spawned binary would exercise (live-gated) from code no object ever reaches.
    """
    hits: dict[str, dict[int, int]] = {wanted: {} for wanted in files}
    in_bin: set[str] = set()
    total = len(objects)
    for index, (is_test, obj) in enumerate(objects, start=1):
        _phase(f"[{index}/{total}] reading {obj.name}")
        seen: set[str] = set()
        current: str | None = None
        for row in _lcov(cov_tool, profdata, obj, env).splitlines():
            if row.startswith("SF:"):
                current = next(
                    (wanted for wanted in files if _same_file(row[3:], wanted)), None
                )
                if current is not None:
                    seen.add(current)
            elif row.startswith("DA:") and current is not None:
                number, count = row[len("DA:") :].split(",")[:2]
                line = int(number)
                counted = hits[current]
                counted[line] = max(counted.get(line, 0), int(count))
        if not is_test:
            in_bin.update(seen)
    return hits, in_bin


def _compress(lines: list[int]) -> str:
    """Compress line numbers into `12-14, 40` spans."""
    spans: list[tuple[int, int]] = []
    for line in sorted(set(lines)):
        if spans and line == spans[-1][1] + 1:
            spans[-1] = (spans[-1][0], line)
        else:
            spans.append((line, line))
    if not spans:
        return "none"
    return ", ".join(
        f"{start}-{end}" if start != end else str(start) for start, end in spans
    )


def _parse_args(argv: list[str] | None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--tests",
        required=True,
        help="cargo test arguments, e.g. '-p slipfork --tests'",
    )
    parser.add_argument(
        "--files",
        required=True,
        help="sources to report, e.g. 'crates/slipfork/src/lib.rs'",
    )
    parser.add_argument(
        "--live-suites",
        default="",
        help="space-separated `make live-test` suite names to name in the summary",
    )
    return parser.parse_args(argv)


def _rust_files(requested: list[str]) -> list[str]:
    """Drop `.py` entries with a loud skip note; keep everything llvm-cov reads."""
    files: list[str] = []
    for wanted in requested:
        if wanted.endswith(".py"):
            print(
                "python: line coverage unavailable offline (uvx --offline coverage "
                "is not cached) — audit "
                f"{wanted} against its TESTING_PLAN.md story + E2E test",
                file=sys.stderr,
            )
        else:
            files.append(wanted)
    return files


def main(argv: list[str] | None = None) -> int:
    args = _parse_args(argv)
    tests = shlex.split(args.tests)
    requested = shlex.split(args.files)
    live_suites = shlex.split(args.live_suites)
    if not tests or not requested:
        raise CoverageError("--tests and --files must both name something")
    files = _rust_files(requested)
    if not files:
        return 0

    env = dict(os.environ)
    env.update(
        {
            "CARGO_TARGET_DIR": str(TARGET_DIR),
            "RUSTFLAGS": "-C instrument-coverage",
            "CARGO_INCREMENTAL": "0",
            "LLVM_PROFILE_FILE": str(PROFRAW_DIR / "%p-%m.profraw"),
        }
    )

    for stale in PROFRAW_DIR.glob("*.profraw"):
        stale.unlink()
    PROFRAW_DIR.mkdir(parents=True, exist_ok=True)

    profdata_tool, cov_tool = _toolchain(env)
    executables, binaries = _build_targets(tests, env)
    _phase(f"running {_plural(len(executables), 'test executable')}…")
    passed = _run_tests(executables, env)
    if passed == 0:
        raise CoverageError("no tests ran: coverage would be meaningless")
    profdata = TARGET_DIR / "coverage.profdata"
    _merge(profdata_tool, profdata, env)

    objects = [
        *((False, binary) for binary in binaries),
        *((True, executable) for executable in executables),
    ]
    hits, in_bin = _profile(cov_tool, profdata, objects, files, env)
    measured = [wanted for wanted in files if hits[wanted]]
    if not measured:
        raise CoverageError(
            "no requested file appears in any instrumented object; "
            "next: check that --files names compiled sources"
        )
    unmeasured = [wanted for wanted in files if not hits[wanted]]
    if unmeasured:
        print(
            "coverage: unmeasured (no instrumented object contains them): "
            + ", ".join(unmeasured),
            file=sys.stderr,
        )

    extra = f" + {_plural(len(binaries), 'bin object')}" if binaries else ""
    noun = "test" if passed == 1 else "tests"
    print(
        f"coverage: {_plural(len(executables), 'test executable')}{extra}, "
        f"{passed} {noun} passed"
    )
    gated = 0
    for wanted in measured:
        counts = hits[wanted]
        total = len(counts)
        covered = sum(1 for count in counts.values() if count)
        if covered == 0 and wanted in in_bin:
            gated += 1
            print(f"  {wanted}  not exercised in this run (live suite gated)")
            continue
        share = 100.0 * covered / total
        print(f"  {wanted}  lines {share:.1f}%  ({covered}/{total})")
        print(
            "  uncovered lines: "
            + _compress([line for line, count in counts.items() if count == 0])
        )
        if share == 0.0:
            print(
                f"coverage: {wanted} reports 0.0% lines — the story test may spawn "
                "the binary with env_clear() so no .profraw is inherited; confirm "
                "its bin object is included before calling anything dead",
                file=sys.stderr,
            )
    if gated:
        print(
            f"coverage: {_plural(gated, 'file')} measured only through a spawned "
            "binary that this run never exercised"
        )
    if live_suites:
        print(
            "coverage: live-gated suites, proven only by `make live-test` on Btrfs "
            "(this run implies no `make coverage-live`): " + ", ".join(live_suites)
        )
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except CoverageError as error:
        print(f"coverage: {error}", file=sys.stderr)
        sys.exit(1)
