"""PostgreSQL access for the demo backend (psycopg 3, dict rows)."""

from __future__ import annotations

import contextlib
import os
from collections.abc import Iterator
from typing import Any

import psycopg
from psycopg.rows import dict_row


@contextlib.contextmanager
def connect() -> Iterator[psycopg.Connection]:
    """One committed connection per call, built from `DATABASE_URL`."""
    with psycopg.connect(os.environ["DATABASE_URL"], row_factory=dict_row) as conn:
        yield conn


def query(sql: str, params: tuple[Any, ...] = ()) -> list[dict[str, Any]]:
    with connect() as conn, conn.cursor() as cur:
        cur.execute(sql, params)
        return cur.fetchall() if cur.description else []


def execute(sql: str, params: tuple[Any, ...] = ()) -> list[dict[str, Any]]:
    """Run a statement and return any `RETURNING` rows."""
    return query(sql, params)
