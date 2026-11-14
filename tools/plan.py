#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Deterministic tooling for the Slipfork Markdown plan tree.

Upgrades over earlier harnesses:
1. Mechanical single-writer path lock: `plan.py set <id> active` rejects activating a task whose
   `paths` globs can overlap another currently `active` task's `paths`.
2. Typed `HUMAN.md` items (`--kind missing-input | direction-challenge | compat-question | release-gate | general`)
   so missing inputs and IoC/DI spec blind spots are first-class citizens, never hidden by workarounds.

Run via:
  uv run --no-project tools/plan.py --help
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING, Any, Final, Protocol

if TYPE_CHECKING:
    from collections.abc import Callable

    _SubParsers = argparse._SubParsersAction[argparse.ArgumentParser]

STATUSES = (
    "proposed",
    "ready",
    "active",
    "review",
    "done",
    "blocked",
    "failed",
    "superseded",
)
CLOSED = ("done", "superseded")
KINDS = ("milestone", "epic", "task")
HUMAN_KINDS = (
    "missing-input",
    "direction-challenge",
    "compat-question",
    "release-gate",
    "general",
)
OWNERS = (
    "doc-writer",
    "designer",
    "implementer",
    "researcher",
    "planner",
    "test-curator",
    "architect",
    "strategist",
    "expert",
    "human",
)
FIELD_ORDER = (
    "id",
    "title",
    "kind",
    "status",
    "owner",
    "priority",
    "depends_on",
    "paths",
    "attempts",
    "max_attempts",
    "escalation",
    "created",
    "updated",
)
ID_RE = re.compile(r"^M(?:[A-Z]|\d+)(?:\.\d+)*$")
FRONTMATTER_RE = re.compile(r"\A---\n(.*?)\n---\n?(.*)\Z", re.DOTALL)


def now() -> str:
    """Return the current local time as a minute-resolution ISO timestamp."""
    return dt.datetime.now().astimezone().strftime("%Y-%m-%dT%H:%M")


class PlanError(Exception):
    """Raised for any invalid plan operation."""


def _segment_overlaps(seg_a: str, seg_b: str) -> bool:
    """Conservatively check whether two single-segment globs can match the same filename."""
    if seg_a == "*" or seg_b == "*" or seg_a == seg_b:
        return True
    re_a = re.compile(
        "^" + re.escape(seg_a).replace(r"\*", ".*").replace(r"\?", ".") + "$"
    )
    re_b = re.compile(
        "^" + re.escape(seg_b).replace(r"\*", ".*").replace(r"\?", ".") + "$"
    )
    if "*" not in seg_a and "?" not in seg_a:
        return bool(re_b.match(seg_a))
    if "*" not in seg_b and "?" not in seg_b:
        return bool(re_a.match(seg_b))
    # Both contain wildcards: compare literal prefixes and suffixes conservatively.
    pre_a = seg_a.split("*", 1)[0].split("?", 1)[0]
    pre_b = seg_b.split("*", 1)[0].split("?", 1)[0]
    if pre_a and pre_b and not (pre_a.startswith(pre_b) or pre_b.startswith(pre_a)):
        return False
    suf_a = seg_a.rsplit("*", 1)[-1].rsplit("?", 1)[-1]
    suf_b = seg_b.rsplit("*", 1)[-1].rsplit("?", 1)[-1]
    return not (
        suf_a and suf_b and not (suf_a.endswith(suf_b) or suf_b.endswith(suf_a))
    )


def globs_overlap(pat_a: str, pat_b: str) -> bool:
    """Return True if repo path globs ``pat_a`` and ``pat_b`` can match a common file."""
    parts_a = [p for p in pat_a.strip("/").split("/") if p]
    parts_b = [p for p in pat_b.strip("/").split("/") if p]
    memo: dict[tuple[int, int], bool] = {}

    def walk(i: int, j: int) -> bool:
        key = (i, j)
        if key in memo:
            return memo[key]
        if i == len(parts_a) and j == len(parts_b):
            return True
        res = False
        if i < len(parts_a) and parts_a[i] == "**":
            res = walk(i + 1, j) or (j < len(parts_b) and walk(i, j + 1))
        elif j < len(parts_b) and parts_b[j] == "**":
            res = walk(i, j + 1) or (i < len(parts_a) and walk(i + 1, j))
        elif (
            i < len(parts_a)
            and j < len(parts_b)
            and _segment_overlaps(parts_a[i], parts_b[j])
        ):
            res = walk(i + 1, j + 1)
        memo[key] = res
        return res

    return walk(0, 0)


def _parse_scalar(raw: str) -> Any:
    val = raw.strip()
    if val in ("", "null", "~"):
        return None
    if val == "true":
        return True
    if val == "false":
        return False
    if re.fullmatch(r"-?\d+", val):
        return int(val)
    if val.startswith("[") and val.endswith("]"):
        inner = val[1:-1].strip()
        if not inner:
            return []
        return [_parse_scalar(part) for part in _split_inline_list(inner)]
    if len(val) >= 2 and val[0] == '"' and val[-1] == '"':
        return json.loads(val)
    if len(val) >= 2 and val[0] == "'" and val[-1] == "'":
        return val[1:-1].replace("''", "'")
    return val


def _split_inline_list(inner: str) -> list[str]:
    parts: list[str] = []
    cur: list[str] = []
    quote: str | None = None
    escape = False
    for ch in inner:
        if escape:
            cur.append(ch)
            escape = False
        elif ch == "\\" and quote == '"':
            cur.append(ch)
            escape = True
        elif quote is not None:
            cur.append(ch)
            if ch == quote:
                quote = None
        elif ch in ('"', "'"):
            quote = ch
            cur.append(ch)
        elif ch == ",":
            parts.append("".join(cur).strip())
            cur = []
        else:
            cur.append(ch)
    if cur or inner.endswith(","):
        parts.append("".join(cur).strip())
    return [p for p in parts if p]


def parse_frontmatter(text: str, source: Path) -> dict[str, Any]:
    meta: dict[str, Any] = {}
    current_list_key: str | None = None
    for lineno, raw_line in enumerate(text.splitlines(), start=1):
        stripped = raw_line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        if stripped.startswith("- ") or stripped == "-":
            if current_list_key is None:
                msg = f"{source}:{lineno}: unexpected list item without key"
                raise PlanError(msg)
            item_raw = stripped[2:].strip() if len(stripped) >= 2 else ""
            meta[current_list_key].append(_parse_scalar(item_raw))
            continue
        if ":" not in raw_line:
            msg = f"{source}:{lineno}: bad frontmatter line {raw_line!r}"
            raise PlanError(msg)
        key, _, rest = raw_line.partition(":")
        key = key.strip()
        rest_stripped = rest.strip()
        if rest_stripped == "":
            meta[key] = []
            current_list_key = key
        else:
            current_list_key = None
            meta[key] = _parse_scalar(rest_stripped)
    return meta


def _format_scalar(value: Any) -> str:
    if value is None:
        return "null"
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, int):
        return str(value)
    text = str(value)
    safe_unquoted = (
        bool(text)
        and text == text.strip()
        and text not in ("null", "~", "true", "false")
        and not re.fullmatch(r"-?\d+(?:\.\d+)?", text)
        and all(ch not in text for ch in ":{}[],&*#?|-<>=!%@\\\"'\n\r\t")
    )
    if safe_unquoted:
        return text
    return json.dumps(text, ensure_ascii=False)


def dump_frontmatter(meta: dict[str, Any]) -> str:
    lines: list[str] = []
    for key, value in meta.items():
        if isinstance(value, list):
            if not value:
                lines.append(f"{key}: []")
            else:
                inline = "[" + ", ".join(_format_scalar(item) for item in value) + "]"
                if len(key) + 2 + len(inline) <= 110:
                    lines.append(f"{key}: {inline}")
                else:
                    lines.append(f"{key}:")
                    lines.extend(f"  - {_format_scalar(item)}" for item in value)
        else:
            lines.append(f"{key}: {_format_scalar(value)}")
    return "\n".join(lines)


@dataclass
class Node:
    """One plan node folder holding NODE.md, LOG.md, and optional RESULT.md."""

    path: Path
    meta: dict[str, Any]
    body: str
    children: list[Node] = field(default_factory=list)
    parent: Node | None = None

    @property
    def id(self) -> str:
        return str(self.meta["id"])

    @property
    def status(self) -> str:
        return str(self.meta.get("status", "proposed"))

    @property
    def kind(self) -> str:
        return str(self.meta.get("kind", "task"))

    @property
    def deps(self) -> list[str]:
        return [str(d) for d in (self.meta.get("depends_on") or [])]

    @property
    def paths(self) -> list[str]:
        return [str(p) for p in (self.meta.get("paths") or [])]

    @property
    def is_container(self) -> bool:
        return self.kind in ("milestone", "epic")

    def save(self) -> None:
        ordered = {k: self.meta[k] for k in FIELD_ORDER if k in self.meta}
        ordered.update({k: v for k, v in self.meta.items() if k not in ordered})
        front = dump_frontmatter(ordered)
        (self.path / "NODE.md").write_text(
            f"---\n{front}\n---\n{self.body}", encoding="utf-8"
        )


@dataclass(frozen=True, slots=True)
class NewNode:
    title: str
    kind: str
    owner: str | None
    depends_on: list[str]
    paths: list[str]
    priority: int
    status: str
    body: str
    node_id: str | None


class Clock(Protocol):
    def __call__(self) -> str: ...


class PlanTree:
    """Loads, validates, queries, and mutates the plan tree under plan/."""

    def __init__(self, root: Path, clock: Clock = now) -> None:
        self.root = root
        self.nodes_dir = root / "nodes"
        self.clock = clock
        self.by_id: dict[str, Node] = {}
        self.top: list[Node] = []
        self._load()

    def _load(self) -> None:
        if not self.nodes_dir.is_dir():
            msg = f"missing {self.nodes_dir}"
            raise PlanError(msg)
        for top in sorted(self.nodes_dir.iterdir(), key=lambda p: _id_key(p.name)):
            if top.is_dir() and ID_RE.match(top.name):
                self.top.append(self._load_dir(top, None))

    def _load_dir(self, path: Path, parent: Node | None) -> Node:
        node_file = path / "NODE.md"
        if not node_file.is_file():
            msg = f"missing NODE.md in {path}"
            raise PlanError(msg)
        match = FRONTMATTER_RE.match(node_file.read_text(encoding="utf-8"))
        if not match:
            msg = f"bad frontmatter in {node_file}"
            raise PlanError(msg)
        meta = parse_frontmatter(match.group(1), node_file)
        if "id" not in meta:
            msg = f"missing 'id' in {node_file}"
            raise PlanError(msg)
        node = Node(path=path, meta=meta, body=match.group(2), parent=parent)
        if node.id in self.by_id:
            msg = f"duplicate id {node.id}"
            raise PlanError(msg)
        self.by_id[node.id] = node
        for child in sorted(path.iterdir(), key=lambda p: _id_key(p.name)):
            if child.is_dir() and ID_RE.match(child.name):
                node.children.append(self._load_dir(child, node))
        return node

    def get(self, node_id: str) -> Node:
        try:
            return self.by_id[node_id]
        except KeyError as exc:
            msg = f"unknown node {node_id}"
            raise PlanError(msg) from exc

    def walk(self, under: str | None = None) -> list[Node]:
        roots = [self.get(under)] if under else self.top
        out: list[Node] = []
        stack = list(reversed(roots))
        while stack:
            n = stack.pop()
            out.append(n)
            stack.extend(reversed(n.children))
        return out

    def deps_done(self, node: Node) -> bool:
        return all(self.get(d).status in CLOSED for d in node.deps if d in self.by_id)

    def ancestors_open(self, node: Node) -> bool:
        p = node.parent
        while p:
            if p.status in CLOSED or p.status in ("blocked", "failed"):
                return False
            if not self.deps_done(p):
                return False
            p = p.parent
        return True

    def actionable(self, under: str | None = None) -> list[tuple[str, Node]]:
        acts: list[tuple[str, Node]] = []
        for n in self.walk(under):
            if not self.ancestors_open(n) or not self.deps_done(n):
                continue
            if n.status == "ready" and n.is_container and not n.children:
                acts.append(("decompose", n))
            elif n.status == "ready" and not n.is_container:
                acts.append(("execute", n))
            elif n.status == "review":
                acts.append(("review", n))
            elif (
                n.is_container
                and n.children
                and n.status not in CLOSED
                and all(c.status in CLOSED for c in n.children)
            ):
                acts.append(("gate", n))
        prio = {"review": 0, "gate": 1, "execute": 2, "decompose": 3}
        acts.sort(
            key=lambda a: (
                prio[a[0]],
                int(a[1].meta.get("priority", 3)),
                _id_key(a[1].id),
            )
        )
        return acts

    def stuck(self, stale_hours: float) -> list[tuple[str, Node]]:
        out: list[tuple[str, Node]] = []
        limit = dt.datetime.now().astimezone() - dt.timedelta(hours=stale_hours)
        for n in self.walk():
            att, mx = int(n.meta.get("attempts", 0)), int(n.meta.get("max_attempts", 3))
            if n.status == "failed":
                out.append(("failed", n))
            elif n.status == "blocked":
                out.append(("blocked", n))
            elif not n.is_container and att >= mx and n.status == "ready":
                out.append(("attempts-exhausted", n))
            elif n.status == "active":
                try:
                    upd = dt.datetime.fromisoformat(str(n.meta.get("updated")))
                    if upd.tzinfo is None:
                        upd = upd.astimezone()
                    if upd < limit:
                        out.append(("stale-active", n))
                except ValueError:
                    out.append(("bad-timestamp", n))
        return out

    def active_path_conflicts(self, candidate: Node) -> list[tuple[Node, str, str]]:
        """Check whether ``candidate``'s paths overlap any other currently active leaf task."""
        conflicts: list[tuple[Node, str, str]] = []
        if candidate.is_container:
            return conflicts
        for other in self.walk():
            if (
                other.id == candidate.id
                or other.is_container
                or other.status != "active"
            ):
                continue
            for pa in candidate.paths:
                for pb in other.paths:
                    if globs_overlap(pa, pb):
                        conflicts.append((other, pa, pb))
        return conflicts

    def validate(self) -> list[str]:
        errors: list[str] = []
        for n in self.walk():
            errors.extend(self._identity_errors(n))
            errors.extend(self._content_errors(n))
            if n.status == "active":
                for other, pa, pb in self.active_path_conflicts(n):
                    if _id_key(n.id) < _id_key(other.id):
                        errors.append(
                            f"active path collision: {n.id} ({pa}) overlaps {other.id} ({pb})"
                        )
        errors.extend(self._cycles())
        return errors

    def _identity_errors(self, n: Node) -> list[str]:
        errors: list[str] = []
        if not ID_RE.match(n.id):
            errors.append(f"{n.id}: invalid id")
        if n.path.name != n.id:
            errors.append(f"{n.id}: folder name {n.path.name} != id")
        if n.parent is not None and not n.id.startswith(n.parent.id + "."):
            errors.append(f"{n.id}: id not under parent {n.parent.id}")
        return errors

    def _content_errors(self, n: Node) -> list[str]:
        errors: list[str] = []
        if n.status not in STATUSES:
            errors.append(f"{n.id}: bad status {n.status}")
        if n.kind not in KINDS:
            errors.append(f"{n.id}: bad kind {n.kind}")
        if not n.is_container and n.children:
            errors.append(
                f"{n.id}: task has children; convert with `edit {n.id} --kind epic` first"
            )
        if not n.is_container and n.meta.get("owner") not in OWNERS:
            errors.append(f"{n.id}: task needs owner in {OWNERS}")
        errors.extend(
            f"{n.id}: unknown dependency {d}" for d in n.deps if d not in self.by_id
        )
        if n.status == "done" and any(c.status not in CLOSED for c in n.children):
            errors.append(f"{n.id}: done but has open children")
        if "# Acceptance" not in n.body and n.status not in ("proposed", "superseded"):
            errors.append(f"{n.id}: body lacks a '# Acceptance' section")
        return errors

    def _cycles(self) -> list[str]:
        state: dict[str, int] = {}
        errs: list[str] = []

        def visit(nid: str, trail: list[str]) -> None:
            if state.get(nid) == 1:
                errs.append("dependency cycle: " + " -> ".join([*trail, nid]))
                return
            if state.get(nid) == 2 or nid not in self.by_id:
                return
            state[nid] = 1
            for d in self.by_id[nid].deps:
                visit(d, [*trail, nid])
            state[nid] = 2

        for nid in self.by_id:
            visit(nid, [])
        return errs

    def journal(self, line: str) -> None:
        with (self.root / "JOURNAL.md").open("a", encoding="utf-8") as f:
            f.write(f"- {self.clock()} {line}\n")

    def log(self, node: Node, agent: str, message: str) -> None:
        with (node.path / "LOG.md").open("a", encoding="utf-8") as f:
            f.write(f"\n## {self.clock()} [{agent}]\n\n{message.strip()}\n")
        self.journal(f"[{node.id}] ({agent}) {message.strip().splitlines()[0][:160]}")

    def set_status(
        self,
        node: Node,
        status: str,
        agent: str,
        note: str | None,
        *,
        allow_overlap: bool = False,
    ) -> None:
        if status not in STATUSES:
            msg = f"bad status {status}"
            raise PlanError(msg)
        if status == "done" and any(c.status not in CLOSED for c in node.children):
            msg = f"{node.id} has open children; cannot mark done"
            raise PlanError(msg)
        if status == "active" and not allow_overlap:
            conflicts = self.active_path_conflicts(node)
            if conflicts:
                other, pa, pb = conflicts[0]
                msg = (
                    f"single-writer path lock: cannot activate {node.id} ({pa!r}) while "
                    f"{other.id} ({pb!r}) is active. Wait or serialize via --depends."
                )
                raise PlanError(msg)
        old = node.status
        node.meta["status"] = status
        node.meta["updated"] = self.clock()
        node.save()
        self.log(
            node, agent, f"status {old} -> {status}" + (f": {note}" if note else "")
        )

    def bump(self, node: Node, agent: str) -> int:
        node.meta["attempts"] = int(node.meta.get("attempts", 0)) + 1
        node.meta["updated"] = self.clock()
        node.save()
        self.log(
            node,
            agent,
            f"attempt {node.meta['attempts']}/{node.meta.get('max_attempts', 3)} started",
        )
        return int(node.meta["attempts"])

    def create(self, parent_id: str | None, spec: NewNode, agent: str) -> Node:
        if spec.kind not in KINDS:
            msg = f"bad kind {spec.kind}"
            raise PlanError(msg)
        if spec.kind == "task" and spec.owner not in OWNERS:
            msg = f"task needs --owner in {OWNERS}"
            raise PlanError(msg)
        parent = self.get(parent_id) if parent_id else None
        node_id = spec.node_id
        if node_id is None:
            if parent is None:
                msg = "top-level nodes need an explicit --id (e.g. M1)"
                raise PlanError(msg)
            used = [int(c.id.rsplit(".", 1)[1]) for c in parent.children]
            node_id = f"{parent.id}.{max(used, default=0) + 1}"
        if node_id in self.by_id or not ID_RE.match(node_id):
            msg = f"invalid or existing id {node_id}"
            raise PlanError(msg)
        base = parent.path if parent else self.nodes_dir
        path = base / node_id
        path.mkdir(parents=False)
        stamp = self.clock()
        meta: dict[str, Any] = {
            "id": node_id,
            "title": spec.title,
            "kind": spec.kind,
            "status": spec.status,
            "owner": spec.owner if spec.kind == "task" else None,
            "priority": spec.priority,
            "depends_on": list(spec.depends_on),
            "paths": list(spec.paths),
            "attempts": 0,
            "max_attempts": 3,
            "escalation": "none",
            "created": stamp,
            "updated": stamp,
        }
        if spec.kind != "task":
            meta.pop("owner")
        node = Node(
            path=path, meta=meta, body="\n" + spec.body.strip() + "\n", parent=parent
        )
        node.save()
        (path / "LOG.md").write_text(f"# Log {node_id}\n", encoding="utf-8")
        if parent:
            parent.children.append(node)
        else:
            self.top.append(node)
            self.top.sort(key=lambda item: _id_key(item.id))
        self.by_id[node_id] = node
        self.log(node, agent, f"created ({spec.kind}) {spec.title}")
        return node

    def human_add(
        self, text: str, node_id: str | None, agent: str, kind: str = "general"
    ) -> str:
        if kind not in HUMAN_KINDS:
            msg = f"bad human request kind {kind!r}; expected {HUMAN_KINDS}"
            raise PlanError(msg)
        f = self.root / "HUMAN.md"
        content = f.read_text(encoding="utf-8")
        nums = [int(m) for m in re.findall(r"^## H-(\d+)", content, re.MULTILINE)]
        hid = f"H-{max(nums, default=0) + 1:03d}"
        ref = f" (node {node_id})" if node_id else ""
        block = (
            f"\n## {hid} [open] [{kind}]{ref}\n\n"
            f"- asked: {self.clock()} by {agent}\n\n"
            f"{text.strip()}\n\n**Answer:**\n\n"
        )
        f.write_text(content.rstrip() + "\n" + block, encoding="utf-8")
        self.journal(f"HUMAN {hid} [{kind}] opened by {agent}{ref}")
        return hid

    def human_open(self) -> list[str]:
        content = (self.root / "HUMAN.md").read_text(encoding="utf-8")
        return re.findall(r"^## (H-\d+ \[open\][^\n]*)$", content, re.MULTILINE)

    def human_answered(self) -> list[str]:
        content = (self.root / "HUMAN.md").read_text(encoding="utf-8")
        pattern = r"^## (H-\d+) \[open\].*?\*\*Answer:\*\*(.*?)(?=^## H-|\Z)"
        return [
            m.group(1)
            for m in re.finditer(pattern, content, re.DOTALL | re.MULTILINE)
            if m.group(2).strip()
        ]

    def escalate(self, node_id: str, agent: str, reason: str, by: str) -> int:
        f = self.root / "ESCALATIONS.md"
        content = f.read_text(encoding="utf-8")
        count = len(re.findall(r"^\| \d{4}-", content, re.MULTILINE)) + 1
        line = (
            f"| {self.clock()} | {node_id} | {agent} | {by} | "
            f"{reason.replace('|', '/')} |\n"
        )
        f.write_text(content.rstrip() + "\n" + line, encoding="utf-8")
        if node_id in self.by_id:
            n = self.by_id[node_id]
            n.meta["escalation"] = agent
            n.save()
        self.journal(
            f"ESCALATION #{count} -> {agent} for {node_id} by {by}: {reason[:120]}"
        )
        return count

    def render_tree(
        self, under: str | None, depth: int | None, *, show_closed: bool
    ) -> str:
        marks = {
            "proposed": "·",
            "ready": "○",
            "active": "◐",
            "review": "◑",
            "done": "●",
            "blocked": "⊘",
            "failed": "✗",
            "superseded": "~",
        }
        lines: list[str] = []
        base_depth = self.get(under).id.count(".") if under else 0
        for n in self.walk(under):
            d = n.id.count(".") - base_depth
            if depth is not None and d > depth:
                continue
            if (
                not show_closed
                and n.status in CLOSED
                and n.children
                and all(c.status in CLOSED for c in n.children)
                and d > 0
            ):
                continue
            owner = f" @{n.meta['owner']}" if n.meta.get("owner") else ""
            deps = f" <-{','.join(n.deps)}" if n.deps else ""
            att = int(n.meta.get("attempts", 0))
            atts = f" a{att}" if att else ""
            lines.append(
                f"{'  ' * d}{marks.get(n.status, '?')} {n.id} [{n.kind}/{n.status}]{owner}{atts}{deps} "
                f"{n.meta.get('title', '')}"
            )
        return "\n".join(lines)

    def render_state(self) -> str:
        counts: dict[str, int] = {s: 0 for s in STATUSES}
        for n in self.walk():
            if not n.is_container:
                counts[n.status] += 1
        current = next((m for m in self.top if m.status not in CLOSED), None)
        milestone = (
            f"{current.id} {current.meta.get('title')} [{current.status}]"
            if current is not None
            else "ALL DONE"
        )
        acts = self.actionable()
        stuck = self.stuck(stale_hours=6)
        esc = (self.root / "ESCALATIONS.md").read_text(encoding="utf-8")
        n_esc = len(re.findall(r"^\| \d{4}-", esc, re.MULTILINE))
        journal = (
            (self.root / "JOURNAL.md").read_text(encoding="utf-8").strip().splitlines()
        )
        tail = [line for line in journal if line.startswith("- ")][-15:]
        budget_md = self.root / "BUDGET.md"
        budget = (
            next(
                (
                    ln[2:]
                    for ln in budget_md.read_text(encoding="utf-8").splitlines()
                    if ln.startswith("- expensive spent")
                ),
                "expensive-tier spend unknown (plugin has not written plan/BUDGET.md yet)",
            )
            if budget_md.exists()
            else "expensive-tier spend unknown (plugin has not written plan/BUDGET.md yet)"
        )
        autopilot = (
            (self.root / "AUTOPILOT").read_text(encoding="utf-8").strip()
            if (self.root / "AUTOPILOT").exists()
            else "off"
        )
        open_items = self.human_open()
        n_answered = len(self.human_answered())
        out = [
            "# STATE (generated by `uv run --no-project tools/plan.py state`, do not edit)",
            "",
            f"- generated: {self.clock()}",
            f"- autopilot: {autopilot}",
            f"- current milestone: {milestone}",
            "- tasks: "
            + (", ".join(f"{k}={v}" for k, v in counts.items() if v) or "none yet"),
            f"- open human requests: {len(open_items)} (answered, awaiting pickup: {n_answered})",
            f"- escalations to expensive model so far: {n_esc}; {budget}",
            "",
            "## Milestones",
            "",
        ]
        out += [f"- {m.id} [{m.status}] {m.meta.get('title')}" for m in self.top]
        out += ["", "## Open HUMAN.md items", ""]
        out += [f"- {item}" for item in open_items] or ["- (none)"]
        out += ["", "## Next actionable (top 12)", ""]
        out += [f"- {a}: {n.id} {n.meta.get('title')}" for a, n in acts[:12]] or [
            "- (none)"
        ]
        out += ["", "## Stuck / attention", ""]
        out += [f"- {why}: {n.id} {n.meta.get('title')}" for why, n in stuck] or [
            "- (none)"
        ]
        out += ["", "## Recent journal", "", *tail, ""]
        return "\n".join(out)


def _id_key(name: str) -> tuple[tuple[int, int], ...]:
    if not ID_RE.match(name):
        return ((9, 10**9),)
    parts = name[1:].split(".")
    head = parts[0]
    head_key = (0, int(head)) if head.isdigit() else (1, ord(head[0]))
    rest_keys = tuple((0, int(part)) for part in parts[1:])
    return (head_key, *rest_keys)


def _read_body(args: argparse.Namespace) -> str:
    if args.body_file:
        return Path(args.body_file).read_text(encoding="utf-8")
    if args.body:
        return str(args.body)
    return (
        "# Goal\n\nTODO\n\n# Context\n\n# Acceptance\n\n"
        "- [ ] TODO (verifiable command + expected result)\n"
    )


def _build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="plan.py",
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    p.add_argument(
        "--root", default=str(Path(__file__).resolve().parent.parent / "plan")
    )
    p.add_argument("--agent", default="unknown", help="agent name recorded in logs")
    sub = p.add_subparsers(dest="cmd", required=True)
    _add_report_parsers(sub)
    _add_node_parsers(sub)
    _add_plan_parsers(sub)
    return p


def _add_report_parsers(sub: _SubParsers) -> None:
    s = sub.add_parser("tree", help="print the plan tree")
    s.add_argument("--under")
    s.add_argument("--depth", type=int)
    s.add_argument("--all", action="store_true")

    s = sub.add_parser("next", help="list actionable nodes")
    s.add_argument("--under")
    s.add_argument("--limit", type=int, default=10)

    s = sub.add_parser("show", help="print NODE.md (and optional LOG/RESULT)")
    s.add_argument("id")
    s.add_argument("--log", action="store_true")
    s.add_argument("--result", action="store_true")


def _add_node_parsers(sub: _SubParsers) -> None:
    s = sub.add_parser("new", help="create a node under PARENT (or top-level)")
    s.add_argument("parent", nargs="?")
    s.add_argument("--id", dest="node_id")
    s.add_argument("--title", required=True)
    s.add_argument("--kind", default="task", choices=KINDS)
    s.add_argument("--owner", choices=OWNERS)
    s.add_argument("--depends", default="")
    s.add_argument("--paths", default="")
    s.add_argument("--priority", type=int, default=3)
    s.add_argument("--status", default="ready", choices=STATUSES)
    s.add_argument("--body")
    s.add_argument("--body-file")

    s = sub.add_parser("set", help="change a node's status")
    s.add_argument("id")
    s.add_argument("status", choices=STATUSES)
    s.add_argument("--note")
    s.add_argument("--allow-overlap", action="store_true")

    s = sub.add_parser("bump", help="increment attempts")
    s.add_argument("id")

    s = sub.add_parser("log", help="append to LOG.md and JOURNAL.md")
    s.add_argument("id")
    s.add_argument("message")

    s = sub.add_parser("edit", help="edit non-status metadata")
    s.add_argument("id")
    s.add_argument("--title")
    s.add_argument("--owner", choices=OWNERS)
    s.add_argument("--priority", type=int)
    s.add_argument("--depends")
    s.add_argument("--paths")
    s.add_argument("--max-attempts", type=int)
    s.add_argument("--kind", choices=KINDS)
    s.add_argument("--reset-attempts", action="store_true")


def _add_plan_parsers(sub: _SubParsers) -> None:
    sub.add_parser("validate", help="check plan consistency")
    s = sub.add_parser("stuck", help="list failed/blocked/exhausted/stale nodes")
    s.add_argument("--stale-hours", type=float, default=6)
    sub.add_parser("state", help="regenerate plan/STATE.md")

    s = sub.add_parser("human", help="human request inbox")
    hs = s.add_subparsers(dest="hcmd", required=True)
    h = hs.add_parser("add")
    h.add_argument("text")
    h.add_argument("--node")
    h.add_argument("--kind", default="general", choices=HUMAN_KINDS)
    hs.add_parser("open")
    hs.add_parser("answered")
    h = hs.add_parser("close")
    h.add_argument("hid")

    s = sub.add_parser("escalate", help="record a call to the expensive model")
    s.add_argument("id")
    s.add_argument("--to", required=True, choices=["strategist", "architect", "expert"])
    s.add_argument("--reason", required=True)

    s = sub.add_parser("autopilot", help="get or set plan/AUTOPILOT")
    s.add_argument("value", nargs="?", choices=["on", "off"])
    s.add_argument("--reason", default="")


def _cmd_tree(tree: PlanTree, args: argparse.Namespace) -> int:
    print(tree.render_tree(args.under, args.depth, show_closed=args.all))
    return 0


def _cmd_next(tree: PlanTree, args: argparse.Namespace) -> int:
    acts = tree.actionable(args.under)[: args.limit]
    for a, n in acts:
        owner = f" @{n.meta.get('owner')}" if n.meta.get("owner") else ""
        print(
            f"{a:9} {n.id}{owner} a{n.meta.get('attempts', 0)}/{n.meta.get('max_attempts', 3)} "
            f"{n.meta.get('title')}  ({n.path.relative_to(tree.root.parent)})"
        )
    if not acts:
        print("(nothing actionable)")
    return 0


def _cmd_show(tree: PlanTree, args: argparse.Namespace) -> int:
    n = tree.get(args.id)
    print(f"# path: {n.path.relative_to(tree.root.parent)}")
    print((n.path / "NODE.md").read_text(encoding="utf-8"))
    for flag, name in ((args.log, "LOG.md"), (args.result, "RESULT.md")):
        f = n.path / name
        if flag and f.exists():
            print(f"\n# ----- {name} -----\n" + f.read_text(encoding="utf-8"))
    return 0


def _cmd_new(tree: PlanTree, args: argparse.Namespace) -> int:
    n = tree.create(
        args.parent,
        NewNode(
            title=args.title,
            kind=args.kind,
            owner=args.owner,
            depends_on=[d.strip() for d in args.depends.split(",") if d.strip()],
            paths=[d.strip() for d in args.paths.split(",") if d.strip()],
            priority=args.priority,
            status=args.status,
            body=_read_body(args),
            node_id=args.node_id,
        ),
        args.agent,
    )
    print(f"created {n.id} at {n.path.relative_to(tree.root.parent)}")
    return 0


def _cmd_set(tree: PlanTree, args: argparse.Namespace) -> int:
    tree.set_status(
        tree.get(args.id),
        args.status,
        args.agent,
        args.note,
        allow_overlap=args.allow_overlap,
    )
    print(f"{args.id} -> {args.status}")
    return 0


def _cmd_bump(tree: PlanTree, args: argparse.Namespace) -> int:
    node = tree.get(args.id)
    attempt = tree.bump(node, args.agent)
    max_attempts = int(node.meta.get("max_attempts", 3))
    exhausted = "  ESCALATE: attempts exhausted" if attempt > max_attempts else ""
    print(f"{args.id} attempt {attempt}/{max_attempts}{exhausted}")
    return 0


def _cmd_log(tree: PlanTree, args: argparse.Namespace) -> int:
    tree.log(tree.get(args.id), args.agent, args.message)
    return 0


def _cmd_edit(tree: PlanTree, args: argparse.Namespace) -> int:
    n = tree.get(args.id)
    for key, val in (
        ("title", args.title),
        ("owner", args.owner),
        ("priority", args.priority),
        ("max_attempts", args.max_attempts),
        ("kind", args.kind),
    ):
        if val is not None:
            n.meta[key] = val
    if args.depends is not None:
        n.meta["depends_on"] = [d.strip() for d in args.depends.split(",") if d.strip()]
    if args.paths is not None:
        n.meta["paths"] = [d.strip() for d in args.paths.split(",") if d.strip()]
    if args.reset_attempts:
        n.meta["attempts"] = 0
    if n.kind != "task":
        n.meta.pop("owner", None)
    n.meta["updated"] = tree.clock()
    n.save()
    tree.log(n, args.agent, "edited fields")
    return 0


def _cmd_validate(tree: PlanTree, _args: argparse.Namespace) -> int:
    errs = tree.validate()
    print("\n".join(errs) if errs else "plan OK")
    return 1 if errs else 0


def _cmd_stuck(tree: PlanTree, args: argparse.Namespace) -> int:
    items = tree.stuck(args.stale_hours)
    for why, n in items:
        print(f"{why:18} {n.id} {n.meta.get('title')}")
    if not items:
        print("(nothing stuck)")
    return 0


def _cmd_state(tree: PlanTree, _args: argparse.Namespace) -> int:
    text = tree.render_state()
    (tree.root / "STATE.md").write_text(text, encoding="utf-8")
    print(text)
    return 0


def _close_human(tree: PlanTree, hid: str, agent: str) -> None:
    f = tree.root / "HUMAN.md"
    txt = f.read_text(encoding="utf-8")
    new = re.sub(
        rf"^## {re.escape(hid)} \[open\]",
        f"## {hid} [closed]",
        txt,
        flags=re.MULTILINE,
    )
    if new == txt:
        msg = f"{hid} not open"
        raise PlanError(msg)
    f.write_text(new, encoding="utf-8")
    tree.journal(f"HUMAN {hid} closed by {agent}")


def _cmd_human(tree: PlanTree, args: argparse.Namespace) -> int:
    if args.hcmd == "add":
        print(tree.human_add(args.text, args.node, args.agent, args.kind))
    elif args.hcmd == "open":
        print("\n".join(tree.human_open()) or "(none)")
    elif args.hcmd == "answered":
        print("\n".join(tree.human_answered()) or "(none)")
    else:
        _close_human(tree, args.hid, args.agent)
    return 0


def _cmd_escalate(tree: PlanTree, args: argparse.Namespace) -> int:
    c = tree.escalate(args.id, args.to, args.reason, args.agent)
    print(f"escalation #{c} recorded")
    return 0


def _cmd_autopilot(tree: PlanTree, args: argparse.Namespace) -> int:
    f = tree.root / "AUTOPILOT"
    if args.value:
        f.write_text(args.value + "\n", encoding="utf-8")
        tree.journal(f"AUTOPILOT {args.value} by {args.agent} {args.reason}".strip())
    print(f.read_text(encoding="utf-8").strip() if f.exists() else "off")
    return 0


_HANDLERS: Final[dict[str, Callable[[PlanTree, argparse.Namespace], int]]] = {
    "tree": _cmd_tree,
    "next": _cmd_next,
    "show": _cmd_show,
    "new": _cmd_new,
    "set": _cmd_set,
    "bump": _cmd_bump,
    "log": _cmd_log,
    "edit": _cmd_edit,
    "validate": _cmd_validate,
    "stuck": _cmd_stuck,
    "state": _cmd_state,
    "human": _cmd_human,
    "escalate": _cmd_escalate,
    "autopilot": _cmd_autopilot,
}


def main(argv: list[str] | None = None) -> int:
    args = _build_parser().parse_args(argv)
    root = Path(args.root)
    try:
        tree = PlanTree(root)
        return _HANDLERS[args.cmd](tree, args)
    except PlanError as exc:
        print(f"plan.py error: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
