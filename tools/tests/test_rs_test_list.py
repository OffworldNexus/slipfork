"""Unit tests for `tools/rs_test_list.py`, the `make rs-test` list guard."""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from types import ModuleType

TOOLS_DIR = Path(__file__).resolve().parent.parent


def _load_tool(name: str) -> ModuleType:
    path = TOOLS_DIR / f"{name}.py"
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


rs_test_list = _load_tool("rs_test_list")


def _workspace(root: Path, members: list[str]) -> None:
    joined = "".join(f'  "{member}",\n' for member in members)
    (root / "Cargo.toml").write_text(
        f"[workspace]\nmembers = [\n{joined}]\n", encoding="utf-8"
    )


def _member(
    root: Path,
    relative: str,
    package: str,
    *,
    autotests: bool = True,
    test_files: tuple[str, ...] = (),
    declarations: str = "",
    body: dict[str, str] | None = None,
) -> Path:
    directory = root / relative
    (directory / "tests").mkdir(parents=True)
    autotests_line = "" if autotests else "autotests = false\n"
    (directory / "Cargo.toml").write_text(
        f'[package]\nname = "{package}"\nversion = "0.0.0"\n'
        f"{autotests_line}\n{declarations}",
        encoding="utf-8",
    )
    for name, source in (body or {}).items():
        path = directory / "tests" / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(source, encoding="utf-8")
    for name in test_files:
        path = directory / "tests" / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("// integration test\n", encoding="utf-8")
    return directory


def _makefile(root: Path, lines: list[str]) -> Path:
    recipe = "".join(f"\t{line}\n" for line in lines)
    path = root / "Makefile"
    path.write_text(f"rs-test:\n{recipe}", encoding="utf-8")
    return path


def _check(root: Path, makefile: Path, *world_booting: str) -> int:
    return rs_test_list.main(
        [
            "--root",
            str(root),
            "--makefile",
            str(makefile),
            "--world-booting",
            *world_booting,
        ]
    )


def test_workspace_member_globs_are_expanded(tmp_path: Path) -> None:
    _workspace(tmp_path, ["crates/*"])
    _member(tmp_path, "crates/alpha", "alpha", test_files=("critical_alpha.rs",))
    _member(tmp_path, "crates/beta", "beta", test_files=("critical_beta.rs",))
    (tmp_path / "crates" / "README").mkdir()

    assert rs_test_list.inspect_workspace(tmp_path).pairs == (
        "alpha:critical_alpha",
        "beta:critical_beta",
    )


def test_nested_main_rs_is_an_autodiscovered_target(tmp_path: Path) -> None:
    _workspace(tmp_path, ["crates/alpha"])
    _member(
        tmp_path,
        "crates/alpha",
        "alpha",
        test_files=("default_suite.rs", "main.rs", "nested/main.rs", "support/mod.rs"),
    )

    assert rs_test_list.inspect_workspace(tmp_path).pairs == (
        "alpha:default_suite",
        "alpha:main",
        "alpha:nested",
    )


def test_autotests_false_honours_declared_test_path(tmp_path: Path, capsys) -> None:
    _workspace(tmp_path, ["crates/vcs"])
    declarations = '\n[[test]]\nname = "critical_vcs"\npath = "tests/critical_vcs.rs"\n'
    _member(
        tmp_path,
        "crates/vcs",
        "slipfork-vcs",
        autotests=False,
        declarations=declarations,
        body={
            "critical_vcs.rs": (
                "// The matrix is a module of this declared target.\n"
                '#[path = "critical_conformance_matrix.rs"]\n'
                "mod matrix;\n"
            ),
            "critical_conformance_matrix.rs": "// included, never a target\n",
            "stray.rs": "// not declared, so Cargo never builds it\n",
        },
    )

    workspace = rs_test_list.inspect_workspace(tmp_path)
    assert workspace.pairs == ("slipfork-vcs:critical_vcs",)
    assert workspace.orphans == ("slipfork-vcs:tests/stray.rs",)

    makefile = _makefile(tmp_path, ["cargo test -p slipfork-vcs --test critical_vcs"])
    assert _check(tmp_path, makefile) == 1
    assert (
        "rs-test-list: files under tests/ are neither declared [[test]] targets "
        "nor #[path] modules of one: slipfork-vcs:tests/stray.rs"
        in capsys.readouterr().err
    )


def test_autotests_false_without_orphans_passes_orphan_check(tmp_path: Path) -> None:
    _workspace(tmp_path, ["crates/vcs"])
    declarations = '\n[[test]]\nname = "critical_vcs"\npath = "tests/critical_vcs.rs"\n'
    _member(
        tmp_path,
        "crates/vcs",
        "slipfork-vcs",
        autotests=False,
        declarations=declarations,
        body={
            "critical_vcs.rs": '#[path = "critical_conformance_matrix.rs"]\nmod matrix;\n',
            "critical_conformance_matrix.rs": "// included by the declared target\n",
        },
    )
    makefile = _makefile(tmp_path, ["cargo test -p slipfork-vcs --test critical_vcs"])

    assert _check(tmp_path, makefile) == 0


def test_autotests_true_still_autodiscovers_a_path_module(tmp_path: Path) -> None:
    _workspace(tmp_path, ["crates/alpha"])
    declarations = (
        '\n[[test]]\nname = "critical_alpha"\npath = "tests/critical_alpha.rs"\n'
    )
    _member(
        tmp_path,
        "crates/alpha",
        "alpha",
        declarations=declarations,
        body={
            "critical_alpha.rs": '#[path = "shared_matrix.rs"]\nmod matrix;\n',
            "shared_matrix.rs": "// Cargo also builds this as its own target\n",
        },
    )

    assert rs_test_list.inspect_workspace(tmp_path).pairs == (
        "alpha:critical_alpha",
        "alpha:shared_matrix",
    )


def test_member_outside_crates_is_not_lost(tmp_path: Path, capsys) -> None:
    _workspace(tmp_path, ["crates/*", "apps/widget"])
    _member(tmp_path, "crates/alpha", "alpha", test_files=("critical_alpha.rs",))
    _member(tmp_path, "apps/widget", "widget", test_files=("critical_widget.rs",))
    makefile = _makefile(tmp_path, ["cargo test -p alpha --test critical_alpha"])

    assert rs_test_list.inspect_workspace(tmp_path).pairs == (
        "alpha:critical_alpha",
        "widget:critical_widget",
    )
    assert _check(tmp_path, makefile) == 1
    assert (
        "rs-test-list: integration test binaries missing from rs-test: "
        "widget:critical_widget" in capsys.readouterr().err
    )


def test_missing_pair_fails(tmp_path: Path, capsys) -> None:
    _workspace(tmp_path, ["crates/*"])
    _member(tmp_path, "crates/alpha", "alpha", test_files=("critical_alpha.rs",))
    makefile = _makefile(tmp_path, ["cargo test -p alpha --lib"])

    assert _check(tmp_path, makefile) == 1
    assert (
        "rs-test-list: integration test binaries missing from rs-test: "
        "alpha:critical_alpha" in capsys.readouterr().err
    )


def test_duplicate_pair_fails(tmp_path: Path, capsys) -> None:
    _workspace(tmp_path, ["crates/alpha"])
    _member(tmp_path, "crates/alpha", "alpha", test_files=("critical_alpha.rs",))
    makefile = _makefile(
        tmp_path,
        [
            "cargo test -p alpha --test critical_alpha",
            "cargo test -p alpha --test critical_alpha",
        ],
    )

    assert _check(tmp_path, makefile) == 1
    assert (
        "rs-test-list: integration binary listed more than once in rs-test: "
        "alpha:critical_alpha" in capsys.readouterr().err
    )


def test_world_booting_pair_must_be_serialized(tmp_path: Path, capsys) -> None:
    _workspace(tmp_path, ["crates/alpha"])
    _member(tmp_path, "crates/alpha", "alpha", test_files=("critical_world.rs",))
    makefile = _makefile(
        tmp_path,
        ["cargo test -p alpha --test critical_world", "cargo test -p alpha --lib"],
    )

    assert _check(tmp_path, makefile, "alpha:critical_world") == 1
    assert (
        "rs-test-list: world-booting binaries must be listed with "
        "-- --test-threads=1: alpha:critical_world" in capsys.readouterr().err
    )

    makefile = _makefile(
        tmp_path,
        ["cargo test -p alpha --test critical_world -- --test-threads=1"],
    )
    assert _check(tmp_path, makefile, "alpha:critical_world") == 0


def test_duplicate_name_in_two_crates_stays_distinct(tmp_path: Path) -> None:
    _workspace(tmp_path, ["crates/*"])
    _member(tmp_path, "crates/alpha", "alpha", test_files=("critical_world.rs",))
    _member(tmp_path, "crates/beta", "beta", test_files=("critical_world.rs",))

    assert rs_test_list.inspect_workspace(tmp_path).pairs == (
        "alpha:critical_world",
        "beta:critical_world",
    )


def test_missing_classification_is_a_usage_error(tmp_path: Path, capsys) -> None:
    _workspace(tmp_path, ["crates/alpha"])
    _member(tmp_path, "crates/alpha", "alpha", test_files=("critical_alpha.rs",))
    makefile = _makefile(tmp_path, ["cargo test -p alpha --test critical_alpha"])

    assert (
        rs_test_list.main(["--root", str(tmp_path), "--makefile", str(makefile)]) == 64
    )
    assert "--world-booting is required" in capsys.readouterr().err


def test_empty_classification_is_accepted(tmp_path: Path) -> None:
    _workspace(tmp_path, ["crates/alpha"])
    _member(tmp_path, "crates/alpha", "alpha", test_files=("critical_alpha.rs",))
    makefile = _makefile(tmp_path, ["cargo test -p alpha --test critical_alpha"])

    assert _check(tmp_path, makefile) == 0
