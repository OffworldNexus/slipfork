"""Demo API: a real PostgreSQL-backed JSON endpoint running inside a Slipfork world.

``pre-deploy`` applies ``db/*.sql``; this process records one row scoped to the world it
booted in, then serves:

  GET /api     -> hello + host + world + debug + secret_len (never the secret itself)
  GET /api/db  -> the seeded table's rows
"""

from __future__ import annotations

import json
import os
import socket
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import psycopg

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 8000
WORKTREE_MARKER = f"{os.sep}.worktrees{os.sep}"


def world_label() -> str:
    """The world this process booted in, derived from its worktree cwd."""
    cwd = os.getcwd()
    if WORKTREE_MARKER in cwd:
        return cwd.split(WORKTREE_MARKER, 1)[1].split(os.sep, 1)[0]
    return os.path.basename(cwd) or "canonical"


def main() -> None:
    world = world_label()
    with psycopg.connect(os.environ["DATABASE_URL"], connect_timeout=10) as conn:
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
            path = self.path.split("?", 1)[0].rstrip("/") or "/api"
            if path == "/api":
                body = {
                    "hello": "world",
                    "host": socket.gethostname(),
                    "world": world,
                    "debug": os.environ.get("DEBUG") == "1",
                    "secret_len": len(os.environ.get("DEMO_SECRET", "")),
                }
            elif path == "/api/db":
                with psycopg.connect(os.environ["DATABASE_URL"]) as query:
                    with query.cursor() as cur:
                        cur.execute("SELECT id, name, scope FROM items ORDER BY id")
                        body = {"rows": [list(row) for row in cur.fetchall()]}
            else:
                self.send_error(404, "only /api and /api/db exist")
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
