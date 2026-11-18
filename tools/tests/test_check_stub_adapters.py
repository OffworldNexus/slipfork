"""Unit tests for `tools/check_stub_adapters.py`, the composition-root stub-adapter guard.

Covers the clean-tree pass, the fatal `todo!()`/`unimplemented!()`/stub-wiring/stub-import cases,
the `#[cfg(test)]` and `tests/` exclusions, the warning-only `TODO`/`FIXME` case, the error-variant
non-false-positive, and the real repository itself (the guard must stay green on it).
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

TOOLS_DIR = Path(__file__).resolve().parent.parent
TOOL = TOOLS_DIR / "check_stub_adapters.py"
REPO = TOOLS_DIR.parent


def _run(root: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(TOOL), "--check", "all", "--root", str(root)],
        capture_output=True,
        text=True,
        check=False,
    )


def _write(root: Path, relative: str, source: str) -> Path:
    path = root / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(source, encoding="utf-8")
    return path


def _clean_tree(root: Path) -> Path:
    _write(
        root, "crates/x/src/lib.rs", "pub fn add(a: u8, b: u8) -> u8 {\n    a + b\n}\n"
    )
    _write(root, "crates/x/src/daemon.rs", "pub fn build() -> u8 {\n    0\n}\n")
    _write(root, "crates/x/src/main.rs", 'fn main() {\n    println!("x");\n}\n')
    return root


def _output(result: subprocess.CompletedProcess[str]) -> str:
    return result.stdout + result.stderr


def _finding(root: Path, relative: str, line: int) -> str:
    return f"{(root / relative).resolve()}:{line}:"


def test_clean_mini_tree_exits_zero(tmp_path: Path) -> None:
    result = _run(_clean_tree(tmp_path))
    assert result.returncode == 0, _output(result)
    assert (
        "check_stub_adapters: OK (3 production file(s), 2 composition root(s))"
        in result.stdout
    )


def test_unavailable_adapter_wired_in_daemon_root_fails_with_exact_location(
    tmp_path: Path,
) -> None:
    _clean_tree(tmp_path)
    _write(
        tmp_path,
        "crates/x/src/daemon.rs",
        "use std::sync::Arc;\n"
        "\n"
        "pub fn build() -> Arc<dyn TunnelRegistrar> {\n"
        "    Arc::new(UnavailableTunnelRegistrar::new())\n"
        "}\n",
    )
    result = _run(tmp_path)
    assert result.returncode == 1, _output(result)
    assert (
        f"{_finding(tmp_path, 'crates/x/src/daemon.rs', 4)} FAIL composition root wires "
        "unavailable adapter `UnavailableTunnelRegistrar`"
    ) in _output(result)


def test_stub_struct_literal_in_daemon_root_fails(tmp_path: Path) -> None:
    _clean_tree(tmp_path)
    _write(
        tmp_path,
        "crates/x/src/daemon.rs",
        "pub fn build() -> FakeHostProbe {\n"
        '    let probe = FakeHostProbe { name: "x" };\n'
        "    probe\n"
        "}\n",
    )
    result = _run(tmp_path)
    assert result.returncode == 1, _output(result)
    assert f"{_finding(tmp_path, 'crates/x/src/daemon.rs', 2)} FAIL" in _output(result)


def test_stub_import_in_binary_root_fails_on_the_import_line(tmp_path: Path) -> None:
    _clean_tree(tmp_path)
    _write(
        tmp_path,
        "crates/x/src/main.rs",
        "use slipfork_edge::{\n"
        "    EdgeError, UnavailableTunnelRegistrar, WorldRegistry,\n"
        "};\n"
        "\n"
        'fn main() {\n    println!("x");\n}\n',
    )
    result = _run(tmp_path)
    assert result.returncode == 1, _output(result)
    assert (
        f"{_finding(tmp_path, 'crates/x/src/main.rs', 2)} FAIL composition root imports stub "
        "adapter `UnavailableTunnelRegistrar`"
    ) in _output(result)


def test_todo_macro_fails_in_lib_but_passes_in_cfg_test_and_tests_tree(
    tmp_path: Path,
) -> None:
    _clean_tree(tmp_path)
    _write(
        tmp_path,
        "crates/x/src/lib.rs",
        "pub fn add(a: u8, b: u8) -> u8 {\n    todo!()\n}\n",
    )
    result = _run(tmp_path)
    assert result.returncode == 1, _output(result)
    assert f"{_finding(tmp_path, 'crates/x/src/lib.rs', 2)} FAIL" in _output(result)

    _write(
        tmp_path,
        "crates/x/src/lib.rs",
        "pub fn add(a: u8, b: u8) -> u8 {\n"
        "    a + b\n"
        "}\n"
        "\n"
        "#[cfg(test)]\n"
        "mod tests {\n"
        "    #[test]\n"
        "    fn pending() {\n"
        "        todo!()\n"
        "    }\n"
        "}\n",
    )
    _write(
        tmp_path,
        "crates/x/tests/integration.rs",
        "#[test]\nfn pending() {\n    unimplemented!()\n}\n",
    )
    result = _run(tmp_path)
    assert result.returncode == 0, _output(result)


def test_error_variant_mentions_are_not_stub_adapters(tmp_path: Path) -> None:
    _clean_tree(tmp_path)
    _write(
        tmp_path,
        "crates/x/src/daemon.rs",
        "pub fn classify(err: EdgeError) -> DagError {\n"
        "    match err {\n"
        "        EdgeError::TunnelUnavailable { .. } => DagError::RemoteResourceUnavailable { id: 1 },\n"
        "        EdgeError::RegistryUnavailable { operation } => DagError::TpmdUnavailable(operation),\n"
        "    }\n"
        "}\n",
    )
    result = _run(tmp_path)
    assert result.returncode == 0, _output(result)
    assert "FAIL" not in _output(result)


def test_todo_comment_in_composition_root_warns_without_failing(tmp_path: Path) -> None:
    _clean_tree(tmp_path)
    _write(
        tmp_path,
        "crates/x/src/daemon.rs",
        "// TODO: wire the real registrar once the daemon ships it.\npub fn build() -> u8 {\n    0\n}\n",
    )
    result = _run(tmp_path)
    assert result.returncode == 0, _output(result)
    assert (
        f"{_finding(tmp_path, 'crates/x/src/daemon.rs', 1)} WARN TODO" in result.stderr
    )
    assert "FAIL" not in _output(result)


def test_missing_crates_tree_is_reported_not_silently_clean(tmp_path: Path) -> None:
    result = _run(tmp_path)
    assert result.returncode == 1
    assert "cannot inspect tree" in result.stderr


def test_real_repository_is_clean() -> None:
    result = subprocess.run(
        [sys.executable, str(TOOL), "--check", "all"],
        cwd=REPO,
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 0, _output(result)
    assert "check_stub_adapters: OK (" in result.stdout
