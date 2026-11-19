"""Unit tests for Slipfork harness tools (`plan.py`, `guard.py`, `critical_guard.py`)."""

from __future__ import annotations

import importlib.util
import os
import subprocess
import sys
from pathlib import Path
from typing import TYPE_CHECKING

import pytest

if TYPE_CHECKING:
    from types import ModuleType

TOOLS_DIR = Path(__file__).resolve().parent.parent


def _load_tool(name: str) -> ModuleType:
    path = TOOLS_DIR / f"{name}.py"
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    mod = importlib.util.module_from_spec(spec)
    sys.modules[name] = mod
    spec.loader.exec_module(mod)
    return mod


plan_mod = _load_tool("plan")
guard_mod = _load_tool("guard")
crit_mod = _load_tool("critical_guard")
docs_honesty_mod = _load_tool("check_docs_honesty")


def _init_plan(tmp_path: Path):
    root = tmp_path / "plan"
    (root / "nodes").mkdir(parents=True)
    (root / "HUMAN.md").write_text("# HUMAN inbox\n", encoding="utf-8")
    (root / "ESCALATIONS.md").write_text(
        "# Escalations\n\n| time | node | to | by | reason |\n|---|---|---|---|---|\n",
        encoding="utf-8",
    )
    (root / "JOURNAL.md").write_text("# Plan Journal\n", encoding="utf-8")
    (root / "AUTOPILOT").write_text("off\n", encoding="utf-8")
    return plan_mod.PlanTree(root, clock=lambda: "2026-11-14T12:00")


def test_globs_overlap_precision() -> None:
    assert plan_mod.globs_overlap(
        "crates/slipfork-snow/**", "crates/slipfork-snow/src/lib.rs"
    )
    assert plan_mod.globs_overlap("docs/design/*.md", "docs/design/core.md")
    assert not plan_mod.globs_overlap(
        "crates/slipfork-snow/**", "crates/slipfork-dag/**"
    )
    assert not plan_mod.globs_overlap("docs/index.md", "docs/snow-modes.md")


def test_single_writer_path_lock_blocks_overlapping_active_nodes(
    tmp_path: Path,
) -> None:
    tree = _init_plan(tmp_path)
    body = "# Goal\n\nG\n\n# Acceptance\n\n- [ ] ok\n"
    tree.create(
        None,
        plan_mod.NewNode("M1", "milestone", None, [], [], 1, "ready", body, "M1"),
        "strategist",
    )
    t1 = tree.create(
        "M1",
        plan_mod.NewNode(
            "Task 1",
            "task",
            "implementer",
            [],
            ["crates/slipfork-snow/**"],
            1,
            "ready",
            body,
            None,
        ),
        "planner",
    )
    t2 = tree.create(
        "M1",
        plan_mod.NewNode(
            "Task 2",
            "task",
            "implementer",
            [],
            ["crates/slipfork-snow/src/lib.rs"],
            2,
            "ready",
            body,
            None,
        ),
        "planner",
    )
    t3 = tree.create(
        "M1",
        plan_mod.NewNode(
            "Task 3",
            "task",
            "implementer",
            [],
            ["crates/slipfork-dag/**"],
            2,
            "ready",
            body,
            None,
        ),
        "planner",
    )

    tree.set_status(t1, "active", "lead", None)
    with pytest.raises(plan_mod.PlanError, match="single-writer path lock"):
        tree.set_status(t2, "active", "lead", None)

    # Disjoint crate activates cleanly!
    tree.set_status(t3, "active", "lead", None)
    assert tree.validate() == []


def test_typed_human_inbox_lifecycle(tmp_path: Path) -> None:
    tree = _init_plan(tmp_path)
    hid = tree.human_add("Need DI decision", "M1", "architect", "direction-challenge")
    assert hid == "H-001"
    open_items = tree.human_open()
    assert len(open_items) == 1
    assert "[direction-challenge]" in open_items[0]
    assert tree.human_answered() == []

    # Simulate human writing an answer
    human_md = tree.root / "HUMAN.md"
    human_md.write_text(
        human_md.read_text(encoding="utf-8") + "Use local mode.\n", encoding="utf-8"
    )
    assert tree.human_answered() == ["H-001"]


def _git_repo(tmp_path: Path) -> Path:
    env = {**os.environ, "GIT_CONFIG_GLOBAL": "/dev/null"}
    subprocess.run(["git", "init", "-b", "develop"], cwd=tmp_path, check=True, env=env)
    subprocess.run(
        ["git", "config", "user.email", "test@example.com"],
        cwd=tmp_path,
        check=True,
        env=env,
    )
    subprocess.run(
        ["git", "config", "user.name", "Test"], cwd=tmp_path, check=True, env=env
    )
    return tmp_path


def test_guard_rejects_derived_marker_and_secret_env(tmp_path: Path) -> None:
    repo = _git_repo(tmp_path)
    (repo / "docs" / "design").mkdir(parents=True)
    (repo / "docs" / "design" / "spec.md").write_text(
        "Width is 420px [derived] from intuition.\n", encoding="utf-8"
    )
    (repo / ".env").write_text("SECRET=123\n", encoding="utf-8")

    violations, _ = guard_mod.inspect(repo)
    reasons = {v.path: v.reason for v in violations}
    assert ".env" in reasons
    assert "docs/design/spec.md" in reasons
    assert "[derived]" in reasons["docs/design/spec.md"]


def test_critical_guard_allows_untracked_addition_and_blocks_unauthorized_edit(
    tmp_path: Path,
) -> None:
    env = {**os.environ, "GIT_CONFIG_GLOBAL": "/dev/null"}
    repo = _git_repo(tmp_path)
    crit_file = repo / "crates" / "slipfork-snow" / "tests" / "critical_modes.rs"
    crit_file.parent.mkdir(parents=True)
    crit_file.write_text("#[test] fn a() {}\n", encoding="utf-8")
    node_dir = repo / "plan" / "nodes" / "M1" / "M1.1"
    node_dir.mkdir(parents=True)
    (node_dir / "NODE.md").write_text(
        "---\nid: M1.1\npaths: [crates/slipfork-dag/**]\n---\n# Goal\n\n# Acceptance\n- [ ] ok\n",
        encoding="utf-8",
    )
    subprocess.run(["git", "add", "."], cwd=repo, check=True, env=env)
    subprocess.run(
        ["git", "commit", "--no-gpg-sign", "-m", "init"], cwd=repo, check=True, env=env
    )
    base = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=repo,
        check=True,
        capture_output=True,
        text=True,
        env=env,
    ).stdout.strip()

    # 1. Brand-new untracked critical test is always allowed.
    new_crit = repo / "crates" / "slipfork-dag" / "tests" / "critical_dag.rs"
    new_crit.parent.mkdir(parents=True)
    new_crit.write_text("#[test] fn b() {}\n", encoding="utf-8")
    assert crit_mod.main(["--repo", str(repo), "--base", base, "--node", "M1.1"]) == 0

    # 2. Editing existing critical_modes.rs without covering path fails!
    crit_file.write_text("#[test] fn weakened() {}\n", encoding="utf-8")
    assert crit_mod.main(["--repo", str(repo), "--base", base, "--node", "M1.1"]) == 1

    # 3. Authorizing crates/slipfork-snow/tests/** in NODE.md passes!
    (node_dir / "NODE.md").write_text(
        "---\nid: M1.1\npaths: [crates/slipfork-snow/tests/**]\n---\n# Goal\n\n# Acceptance\n- [ ] ok\n",
        encoding="utf-8",
    )
    assert crit_mod.main(["--repo", str(repo), "--base", base, "--node", "M1.1"]) == 0


def test_docs_honesty_rejects_vaporware_subcommands_in_published_docs(
    tmp_path: Path,
) -> None:
    (tmp_path / "docs" / "design").mkdir(parents=True)
    (tmp_path / "docs" / "index.md").write_text(
        "```console\n$ slipfork doctor\n$ slipfork up\n```\n", encoding="utf-8"
    )
    (tmp_path / "docs" / "design" / "target-dx.md").write_text(
        "```console\n$ slipfork up\n$ slipfork test\n```\n", encoding="utf-8"
    )
    help_out = "Usage: slipfork <COMMAND>\n\nCommands:\n  doctor  Probe host\n  fork    Clone world\n"
    valid = docs_honesty_mod.extract_help_subcommands(help_out)
    published = docs_honesty_mod.published_doc_files(tmp_path)
    errs = docs_honesty_mod.find_vaporware_commands(published, valid, tmp_path)
    assert len(errs) == 1
    assert "slipfork up" in errs[0]
