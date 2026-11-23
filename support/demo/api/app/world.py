"""World identity derived from `PUBLIC_URL`: the full ingress host and its display id."""

from __future__ import annotations

import os

DEFAULT_PUBLIC_URL = "http://demo.localhost"


def public_url() -> str:
    """The real public URL of this world, exactly as the ingress serves it."""
    return os.environ.get("PUBLIC_URL", DEFAULT_PUBLIC_URL).rstrip("/")


def public_host() -> str:
    """The full ingress host (`demo.localhost`, `feat-auth--demo.localhost`)."""
    return public_url().split("://", 1)[-1].split("/", 1)[0]


def world_id() -> str:
    """Display-only id (`demo`, `feat-auth--demo`); never reuse it as a hostname."""
    return public_host().removesuffix(".localhost") or "demo"


def fork_name() -> str | None:
    """The fork prefix of a forked world, or `None` in the parent world."""
    world = world_id()
    return world.split("--", 1)[0] if "--" in world else None
