"""Serve the demo front page on the service's public port."""

from __future__ import annotations

import functools
import sys
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 5173


def main() -> None:
    handler = functools.partial(
        SimpleHTTPRequestHandler, directory=str(Path(__file__).parent)
    )
    server = ThreadingHTTPServer(("0.0.0.0", PORT), handler)
    print(f"demo-front: listening on 0.0.0.0:{PORT}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
