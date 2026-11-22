"""HTTP JSON API (`/api/status`, `POST /api/releases`, `/api/healthz`)."""

from __future__ import annotations

import argparse
import json
import os
import pathlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from clients import pg_query, redis_cmd, s3_request


def git_branch() -> str:
    head = pathlib.Path("/project/.git/HEAD")
    if head.is_file():
        raw = head.read_text().strip()
        if raw.startswith("ref: refs/heads/"):
            return raw.removeprefix("ref: refs/heads/")
        return raw[:12]
    return "unknown"


class ApiHandler(BaseHTTPRequestHandler):
    def _send_json(self, status: int, payload: dict) -> None:
        body = (json.dumps(payload, indent=2) + "\n").encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self) -> None:
        path = self.path.split("?", 1)[0]
        if path in ("/api/healthz", "/healthz"):
            self._send_json(
                200, {"ok": True, "world": os.environ.get("SLIPFORK_WORLD", "demo")}
            )
            return
        if path in ("/api", "/api/", "/api/status"):
            visits = redis_cmd("INCR", "slipfork:visits")
            worker_beat = redis_cmd("GET", "slipfork:worker:heartbeat")
            rows = pg_query(
                "SELECT id, slug, title, source_world, created_at::text AS created_at "
                "FROM releases ORDER BY id ASC;"
            )
            anon_uploads_status, _ = s3_request(
                "GET", "uploads", "audit.json", signed=False
            )
            anon_assets_status, _ = s3_request(
                "GET", "assets", "badge.svg", signed=False
            )
            signed_uploads_status, _ = s3_request(
                "GET", "uploads", "audit.json", signed=True
            )
            world = os.environ.get("SLIPFORK_WORLD", "demo")
            self._send_json(
                200,
                {
                    "world": world,
                    "branch": git_branch(),
                    "public_url": os.environ.get(
                        "PUBLIC_URL", f"http://{world}.localhost"
                    ),
                    "cdn_domain": os.environ.get(
                        "CDN_DOMAIN", f"static-cdn.{world}.localhost"
                    ),
                    "cdn_badge_url": f"{os.environ.get('CDN_URL', '')}/badge.svg",
                    "uploads_url": f"{os.environ.get('UPLOADS_URL', '')}/audit.json",
                    "mailpit_url": f"http://mail.{world}.localhost",
                    "secret_key_configured": bool(os.environ.get("SECRET_KEY")),
                    "redis": {
                        "visits": visits,
                        "worker_heartbeat": worker_beat,
                    },
                    "s3": {
                        "assets_anonymous_http": anon_assets_status,
                        "uploads_anonymous_http": anon_uploads_status,
                        "uploads_signed_http": signed_uploads_status,
                    },
                    "releases_count": len(rows),
                    "releases": rows,
                },
            )
            return
        self._send_json(404, {"error": f"unknown route `{path}`"})

    def do_POST(self) -> None:
        path = self.path.split("?", 1)[0]
        if path == "/api/releases":
            length = int(self.headers.get("Content-Length", "0"))
            data = json.loads(self.rfile.read(length) or b"{}")
            slug = "".join(
                c for c in str(data.get("slug", "")) if c.isalnum() or c == "-"
            )[:40]
            title = str(data.get("title", "")).replace("'", "''")[:120]
            if not slug or not title:
                self._send_json(400, {"error": "slug and title are required"})
                return
            world = os.environ.get("SLIPFORK_WORLD", "demo")
            pg_query(
                f"INSERT INTO releases (slug, title, source_world) "
                f"VALUES ('{slug}', '{title}', '{world}') "
                f"ON CONFLICT (slug) DO UPDATE SET title = EXCLUDED.title;"
            )
            self._send_json(201, {"created": slug, "world": world})
            return
        self._send_json(404, {"error": f"unknown route `{path}`"})

    def log_message(self, fmt: str, *args: object) -> None:
        print(
            f"[api] {self.command} {self.path} -> {args[1] if len(args) > 1 else ''}",
            flush=True,
        )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=int(os.environ.get("PORT", "8000")))
    args = parser.parse_args()
    print(
        f"[api] listening on 0.0.0.0:{args.port} (world={os.environ.get('SLIPFORK_WORLD')})",
        flush=True,
    )
    ThreadingHTTPServer(("0.0.0.0", args.port), ApiHandler).serve_forever()


if __name__ == "__main__":
    main()
