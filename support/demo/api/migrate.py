"""Apply ``db/*.sql`` to the world's PostgreSQL database, idempotently.

Runs as the ``pre-deploy`` hook and again through ``slipfork run migrate``.
"""

from __future__ import annotations

import os
import pathlib
import time

import psycopg

DB_DIR = pathlib.Path(__file__).resolve().parent.parent / "db"


def connect(deadline_seconds: float = 60.0) -> psycopg.Connection:
    """Connect to DATABASE_URL, retrying while the world's postgres container warms up."""
    deadline = time.monotonic() + deadline_seconds
    while True:
        try:
            return psycopg.connect(os.environ["DATABASE_URL"], connect_timeout=5)
        except psycopg.OperationalError:
            if time.monotonic() >= deadline:
                raise
            time.sleep(0.5)


def main() -> None:
    conn = connect()
    with conn.cursor() as cur:
        for sql in sorted(DB_DIR.glob("*.sql")):
            cur.execute(sql.read_text(encoding="utf-8"))
    conn.commit()
    print("demo-api: migrations applied", flush=True)


if __name__ == "__main__":
    main()
