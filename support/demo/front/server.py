"""Frontend dashboard server (`port 5173`, `route /`) consuming `/api/status` & `static-cdn`."""

from __future__ import annotations

import argparse
import html
import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PAGE = """<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Slipfork — {world}</title>
  <style>
    :root {{ color-scheme: dark; }}
    body {{
      margin: 0; font-family: ui-sans-serif, system-ui, -apple-system, sans-serif;
      background: #090d16; color: #e2e8f0; line-height: 1.5;
    }}
    main {{ max-width: 920px; margin: 0 auto; padding: 2.5rem 1.5rem; }}
    header {{ display: flex; align-items: center; justify-content: space-between; flex-wrap: wrap; gap: 1rem; border-bottom: 1px solid #1e293b; padding-bottom: 1.25rem; }}
    .pill {{ display: inline-flex; align-items: center; gap: 0.5rem; background: #0f172a; border: 1px solid #1e293b; border-radius: 999px; padding: 0.35rem 0.9rem; font-family: ui-monospace, monospace; font-size: 0.85rem; color: #22d3ee; }}
    .dot {{ width: 9px; height: 9px; border-radius: 50%; background: #22c55e; display: inline-block; }}
    .grid {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr)); gap: 1rem; margin: 1.5rem 0; }}
    .card {{ background: #0f172a; border: 1px solid #1e293b; border-radius: 0.75rem; padding: 1rem 1.15rem; }}
    .label {{ font-size: 0.75rem; text-transform: uppercase; letter-spacing: 0.06em; color: #94a3b8; }}
    .val {{ font-size: 1.35rem; font-weight: 700; font-family: ui-monospace, monospace; margin-top: 0.25rem; color: #f8fafc; }}
    table {{ width: 100%; border-collapse: collapse; background: #0f172a; border: 1px solid #1e293b; border-radius: 0.75rem; overflow: hidden; }}
    th, td {{ text-align: left; padding: 0.75rem 1rem; border-bottom: 1px solid #1e293b; font-size: 0.9rem; }}
    th {{ color: #94a3b8; font-weight: 600; font-size: 0.78rem; text-transform: uppercase; }}
    code {{ font-family: ui-monospace, monospace; color: #38bdf8; }}
    a {{ color: #22d3ee; text-decoration: none; }}
    a:hover {{ text-decoration: underline; }}
    .footer {{ margin-top: 1.5rem; color: #64748b; font-size: 0.85rem; font-family: ui-monospace, monospace; }}
  </style>
</head>
<body>
  <main>
    <header>
      <div>
        <div class="pill"><span class="dot"></span> <span id="world">{world}</span> &middot; branch <strong id="branch">...</strong></div>
        <h1 style="margin: 0.6rem 0 0; font-size: 1.65rem;">Slipfork Full-Stack CoW World</h1>
      </div>
      <div>
        <img id="cdn-badge" src="{cdn_url}/badge.svg" alt="CDN badge" height="28">
      </div>
    </header>

    <section class="grid">
      <div class="card"><div class="label">PostgreSQL 18 Rows</div><div class="val" id="pg-count">-</div></div>
      <div class="card"><div class="label">Redis 7 Visits</div><div class="val" id="redis-visits">-</div></div>
      <div class="card"><div class="label">S3 Private / Public</div><div class="val" id="s3-policy">-</div></div>
      <div class="card"><div class="label">Mailpit Inbox</div><div class="val"><a id="mail-link" href="http://mail.{world}.localhost" target="_blank">mailpit &rarr;</a></div></div>
    </section>

    <h2 style="font-size: 1.05rem; color: #cbd5e1;">Live <code>releases</code> (isolated per Btrfs fork)</h2>
    <table>
      <thead><tr><th>ID</th><th>Slug</th><th>Title</th><th>Seeded In</th></tr></thead>
      <tbody id="rows"><tr><td colspan="4">Loading from <code>/api/status</code>...</td></tr></tbody>
    </table>

    <div class="footer">
      build-arg VITE_CDN_URL={vite_cdn_url} &middot; worker: <span id="worker">-</span>
    </div>
  </main>
  <script>
    fetch('/api/status').then(r => r.json()).then(d => {{
      document.getElementById('world').textContent = d.world;
      document.getElementById('branch').textContent = d.branch;
      document.getElementById('pg-count').textContent = d.releases_count;
      document.getElementById('redis-visits').textContent = d.redis.visits;
      document.getElementById('s3-policy').textContent =
        d.s3.uploads_anonymous_http + ' / ' + d.s3.assets_anonymous_http;
      document.getElementById('mail-link').href = d.mailpit_url;
      document.getElementById('cdn-badge').src = d.cdn_badge_url;
      document.getElementById('worker').textContent = d.redis.worker_heartbeat || 'starting';
      const tbody = document.getElementById('rows');
      tbody.innerHTML = d.releases.map(r =>
        `<tr><td>${{r.id}}</td><td><code>${{r.slug}}</code></td><td>${{r.title}}</td><td><code>${{r.source_world}}</code></td></tr>`
      ).join('');
    }});
  </script>
</body>
</html>
"""


class FrontHandler(BaseHTTPRequestHandler):
    def do_GET(self) -> None:
        world = os.environ.get("SLIPFORK_WORLD", "demo")
        cdn_url = os.environ.get("CDN_URL") or os.environ.get("VITE_CDN_URL", "")
        vite_cdn = os.environ.get("VITE_CDN_URL", "")
        rendered = PAGE.format(
            world=html.escape(world),
            cdn_url=html.escape(cdn_url),
            vite_cdn_url=html.escape(vite_cdn),
        ).encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(rendered)))
        self.end_headers()
        self.wfile.write(rendered)

    def log_message(self, fmt: str, *args: object) -> None:
        print(
            f"[front] {self.command} {self.path} -> {args[1] if len(args) > 1 else ''}",
            flush=True,
        )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=int(os.environ.get("PORT", "5173")))
    args = parser.parse_args()
    print(f"[front] serving dashboard on 0.0.0.0:{args.port}", flush=True)
    ThreadingHTTPServer(("0.0.0.0", args.port), FrontHandler).serve_forever()


if __name__ == "__main__":
    main()
