"""Demo API: a real PostgreSQL-backed HTTP endpoint running inside a Slipfork world.

On startup it applies ``db/0001_schema.sql`` and ``db/0002_seed.sql`` against
``DATABASE_URL`` (idempotent), then records one row scoped to the world it booted in,
derived from the worktree the component runs in. It serves:

  GET /    -> hello + host + world + debug + secret_len (never the secret itself)
  GET /db  -> the seeded table's rows

Only the standard library plus ``psycopg`` (installed by the Dockerfile) are used.
"""

from __future__ import annotations

import json
import os
import pathlib
import socket
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import psycopg

APP_DIR = pathlib.Path(__file__).resolve().parent
# `app/server.py` runs from the bind-mounted checkout, so its SQL is the sibling `db/` directory.
DB_DIR = APP_DIR.parent / "db"
PORT = 8000
WORKTREE_MARKER = f"{os.sep}.worktrees{os.sep}"


def world_label() -> str:
    """The world this process booted in, derived from its worktree cwd."""
    cwd = os.getcwd()
    if WORKTREE_MARKER in cwd:
        return cwd.split(WORKTREE_MARKER, 1)[1].split(os.sep, 1)[0]
    return os.path.basename(cwd) or "canonical"


def connect(deadline_seconds: float = 60.0) -> psycopg.Connection:
    """Connect to DATABASE_URL, retrying while the world's postgres container warms up."""
    url = os.environ["DATABASE_URL"]
    deadline = time.monotonic() + deadline_seconds
    while True:
        try:
            return psycopg.connect(url, connect_timeout=5)
        except psycopg.OperationalError:
            if time.monotonic() >= deadline:
                raise
            time.sleep(0.5)


def apply_sql(conn: psycopg.Connection, name: str) -> None:
    with conn.cursor() as cur:
        cur.execute((DB_DIR / name).read_text(encoding="utf-8"))
    conn.commit()


def main() -> None:
    world = world_label()
    conn = connect()
    apply_sql(conn, "0001_schema.sql")
    apply_sql(conn, "0002_seed.sql")
    with conn.cursor() as cur:
        cur.execute(
            "INSERT INTO items (name, scope) VALUES ('boot', %s) "
            "ON CONFLICT (scope, name) DO NOTHING",
            (world,),
        )
    conn.commit()
    print(f"demo-api: world={world} db=ready", flush=True)

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self) -> None:  # noqa: N802 - stdlib handler contract
            path = self.path.split("?", 1)[0]
            if path == "/":
                body = {
                    "hello": "world",
                    "host": socket.gethostname(),
                    "world": world,
                    "debug": os.environ.get("DEBUG") == "True",
                    "secret_len": len(os.environ.get("DEMO_SECRET", "")),
                }
            elif path == "/db":
                with psycopg.connect(os.environ["DATABASE_URL"]) as query:
                    with query.cursor() as cur:
                        cur.execute("SELECT id, name, scope FROM items ORDER BY id")
                        body = {"rows": [list(row) for row in cur.fetchall()]}
            else:
                self.send_error(404, "only / and /db exist")
                return
            payload = json.dumps(body, sort_keys=True).encode() + b"\n"
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def log_message(self, fmt: str, *args: object) -> None:
            print(f"demo-api: {fmt % args}", flush=True)

    server = ThreadingHTTPServer(("0.0.0.0", PORT), Handler)
    print(f"demo-api: listening on 0.0.0.0:{PORT}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
