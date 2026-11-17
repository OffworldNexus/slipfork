#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.12"
# dependencies = [
#     "playwright",
#     "psycopg[binary]",
# ]
# ///
"""Real end-to-end PoC benchmark for the `0.2.0` world model (`docs/research/m1-poc-benchmark.md`).

`slipfork doctor --json` gates the host, then `snow resolve` / `auth login` / `dag plan` / `up` /
`down` / `fork` / `exec` / `drop` drive a harness-owned `slipforkd` against a generated sample
project. The harness measures real PostgreSQL 18 `initdb -> seed -> pg_dump -> pg_restore`, the real
`BtrfsEngine` subvolume snapshot/clone path (`KernelBtrfsDriver`), and a head-to-head browser +
committed-SQL suite across three regimes:

1. Traditional baseline (host PostgreSQL, one warm Chromium, `TRUNCATE` + re-seed per test).
2. Slipfork on-demand cold CoW worlds (`fork` + cold host Chromium per test).
3. Slipfork pre-warmed CoW pool (worlds forked and booted ahead of test dispatch).

The seed is produced honestly: a real PostgreSQL 18 `pg_dump -Fc` archive is zstd-compressed and
streamed by a minimal loopback `ospp-v1` server (`ADR-0008` §1–§2), so `slipforkd` restores it
inside `postgres:18` and seals `<project>-base` with `freeze_world`. Every later `fork` clones that
sealed subvolume with zero SQL re-seed and zero direct state-directory manipulation.

`ADR-0009` §2.2 topology: the app rootfs is a lean `Dockerfile` image with **no** browser; host
Chromium reaches each world's single `.localhost` `WorldOrigin` through `slipforkd`'s HTTP ingress
and `dial_world_tcp`, exactly like a developer's browser would.

Every child process receives an explicit allowlisted environment (`bench_child_env`), every byte
of run state lives under one disk-backed run directory that is wiped fail-closed before the first
measurement (`prepare_run_state`), and every measurement's dispatch order is derived from one
explicit seed.

`slipfork doctor --json` on `<run_dir>/state` — the exact `--state-dir` every later CLI call
receives — must report `ok: true` before any measurement starts; that single verdict is the host
gate, and the harness never re-derives a host fact the shipped CLI already owns. When it is not
`ok` the harness prints the report's `fixes` lines verbatim and exits non-zero, producing no metric
at all — never zeros and never a degraded fallback (`ADR-0010` §4). Every snapshot/clone number is
taken through the shipped `slipfork` binary, i.e. the `BtrfsEngine`/`KernelBtrfsDriver` seam; the
harness never issues `BTRFS_IOC_*` itself.

**Host honesty:** this checkout's host runs XFS, so it only exercises the fail-closed branch above.
The measured report is regenerated on the Btrfs DO VM by `M3.19`.

`--check` is the default and strictly read-only: it re-measures, compares the run against
`poc/bench-baseline.json` (the parsed projection of the committed report), and exits non-zero on
drift without writing a file. `--write-report` is the only mutator; review and commit the report it
writes.

**Topology honesty:** `0.2.0` worlds boot from a read-only `Dockerfile`-derived `L0` rootfs and run
backing services as daemonless `crun` containers in the world `netns` (`ADR-0009`). Earlier M1
figures in the report were measured on the pre-`ADR-0009` host-shared-`/usr` topology (host
`postgres`/Chromium over the host `/usr` + `/lib`) and are not comparable to fresh runs.
"""

from __future__ import annotations

import argparse
import base64
import concurrent.futures
import hashlib
import json
import os
import platform
import random
import shutil
import socket
import struct
import subprocess
import sys
import threading
import time
from collections.abc import Callable, Sequence
from dataclasses import asdict, dataclass
from datetime import UTC, datetime
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
SLIPFORK_BIN = REPO_ROOT / "target" / "release" / "slipfork"
SLIPFORKD_BIN = REPO_ROOT / "target" / "release" / "slipforkd"
PG_BIN = Path("/usr/lib/postgresql/18/bin")
CHROME_BIN = Path("/usr/bin/google-chrome")
ZSTD_BIN = Path("/usr/bin/zstd")
REPORT_PATH = REPO_ROOT / "docs" / "research" / "m1-poc-benchmark.md"
BASELINE_PATH = REPO_ROOT / "poc" / "bench-baseline.json"

# Reuse the active script interpreter (it already carries `playwright` and `psycopg`) so timings
# measure PostgreSQL + CoW + Chromium, never `uv` package resolution.
PYTHON_EXE = sys.executable

DEFAULT_RUN_DIR = Path.home() / ".cache" / "slipfork" / "bench"
DEFAULT_SEED = 20261115

# The only host variables a benchmark child may inherit. Everything else (notably `PYTHONPATH`,
# `CARGO_HOME`, `RUSTUP_HOME`, and `XDG_*` data homes) is dropped, and a missing allowlisted
# variable is omitted rather than replaced by a host default (`HOME`/`TMPDIR` are pinned to the
# run directory because the run must never write outside it).
PASSTHROUGH_ENV = ("PATH", "LANG", "LC_ALL", "LC_CTYPE")
PINNED_ENV = ("HOME", "TMPDIR")
CHILD_ENV_PREFIX = "SLIPFORK_"

TOPMOST_MOUNT_FORBIDDEN = ("/tmp/", "/dev/shm/")

# `doctor` is a fast host probe; a wedged one must surface as a diagnostic, never as a hung
# harness. The measured CLI calls (`up`/`fork`/`exec`/...) get a far larger cap because their
# duration is the measurement itself.
DOCTOR_TIMEOUT_S = 60.0
CLI_TIMEOUT_S = 900.0


class BenchError(RuntimeError):
    """The benchmark cannot produce an honest measurement."""


SCHEMA_SQL = """
CREATE TABLE tenants (
    id SERIAL PRIMARY KEY,
    slug TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL
);
CREATE TABLE products (
    id SERIAL PRIMARY KEY,
    tenant_id INT NOT NULL REFERENCES tenants(id),
    sku TEXT NOT NULL UNIQUE,
    title TEXT NOT NULL,
    price_cents INT NOT NULL,
    stock INT NOT NULL
);
CREATE TABLE orders (
    id SERIAL PRIMARY KEY,
    product_id INT NOT NULL REFERENCES products(id),
    buyer_email TEXT NOT NULL,
    quantity INT NOT NULL,
    total_cents INT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE audit_events (
    id SERIAL PRIMARY KEY,
    kind TEXT NOT NULL,
    payload JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_products_tenant ON products(tenant_id);
CREATE INDEX idx_orders_product ON orders(product_id);
"""

SEED_CORE_SQL = """
INSERT INTO tenants (slug, name)
SELECT 'tenant-' || g, 'Tenant Organization #' || g
FROM generate_series(1, 200) AS g;
"""

SEED_CATALOG_SQL = """
INSERT INTO products (tenant_id, sku, title, price_cents, stock)
SELECT ((g - 1) % 200) + 1,
       'SKU-' || lpad(g::text, 6, '0'),
       'Precision Component #' || g,
       1500 + (g % 50) * 100,
       25
FROM generate_series(1, 10000) AS g;

INSERT INTO audit_events (kind, payload)
SELECT 'catalog.seeded', jsonb_build_object('batch', g, 'ok', true)
FROM generate_series(1, 5000) AS g;
"""

FRONT_HTML = """<!doctype html>
<html>
<body>
  <h1 id="title">__TITLE__</h1>
  <p>SKU: <span id="sku">__SKU__</span> | Stock: <span id="stock">__STOCK__</span></p>
  <button onclick="checkout()">Complete Checkout</button>
  <div id="result"></div>
  <script>
    async function checkout() {
      const r = await fetch('/back/checkout', {
        method: 'POST',
        headers: {'Content-Type': 'application/json'},
        body: JSON.stringify({sku: '__SKU__', qty: 2, email: 'buyer@example.com'})
      });
      const d = await r.json();
      document.getElementById('result').innerHTML =
        '<p id="order-confirmed">Order #' + d.order_id +
        ' Remaining: <span id="remaining-stock">' + d.remaining_stock + '</span></p>';
    }
  </script>
</body>
</html>
"""

# `ADR-0009` §2.2: the app rootfs is a lean `L0` image with **zero** in-world
# Playwright/Chromium. Headless browsers stay in the shared host browser pool (`CHROME_BIN`)
# and reach each world through its single `.localhost` origin.
DOCKERFILE = """FROM python:3.12-slim-bookworm AS app
RUN pip install --no-cache-dir "psycopg[binary]"
WORKDIR /workspace
EXPOSE 8000
"""

SNOW_YML = """images:
  - id: app
    context: .
    dockerfile: Dockerfile

components:
  - id: api
    image: app
    envs: [app]
    internal_ports: [8000]
    run_command: "exec python3 api/app.py"

ingresses:
  - match: { prefix: "/" }
    target: { component: api, port: 8000 }

resources:
  pg_clusters: [{ id: psql, version: "18" }]
  pg_databases: [{ id: shop, cluster: psql }]

modes:
  local:
    envs:
      app:
        SECRET_KEY: '${{ secrets::SECRET_KEY !! random_string(50) }}'
        DATABASE_URL: '${{ resources::pg_databases.shop.url }}'
        BASE_URL: '${{ resources::app.url }}'
"""

SLIPFORK_TOML = """project = "acme-shop"

[artifacts.front]
inputs = ["front/src/**"]
build = "build-front"
mount = "front/dist"

[fixtures.migrate]
inputs = ["api/migrations/*.sql"]
run = "psql -f api/migrations/0001_schema.sql"

[fixtures.seed_core]
parent = "migrate"
inputs = ["api/migrations/seed_core.sql"]
run = "psql -f api/migrations/seed_core.sql"

[fixtures.seed_catalog]
parent = "seed_core"
inputs = ["api/migrations/seed_catalog.sql"]
run = "psql -f api/migrations/seed_catalog.sql"
"""

# The `components.api` `L0` process: a real HTTP server on the world's `127.0.0.1:8000`.
# `slipforkd`'s `.localhost` ingress reaches it through `dial_world_tcp`, and the app never needs
# a browser, a test runner, or any package beyond `psycopg[binary]`.
APP_SCRIPT = r"""
import http.server
import json
import os
import threading

import psycopg

DATABASE_URL = os.environ["DATABASE_URL"]

with open(os.path.join("front", "src", "App.html"), encoding="utf-8") as handle:
    TEMPLATE = handle.read()

CONN = psycopg.connect(DATABASE_URL, user="postgres", autocommit=True)
LOCK = threading.Lock()


class AppHandler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        pass

    def _send(self, status, body, content_type="text/html; charset=utf-8"):
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        path = self.path.split("?", 1)[0]
        if path.startswith("/products/"):
            sku = path.rsplit("/", 1)[-1]
            with LOCK:
                row = CONN.execute(
                    "SELECT sku, title, price_cents, stock FROM products WHERE sku = %s",
                    (sku,),
                ).fetchone()
            if row is None:
                self._send(404, b"not found")
                return
            body = (
                TEMPLATE.replace("__SKU__", row[0])
                .replace("__TITLE__", row[1])
                .replace("__PRICE__", f"{row[2] / 100:.2f}")
                .replace("__STOCK__", str(row[3]))
            ).encode("utf-8")
            self._send(200, body)
            return
        if path == "/orders/count":
            with LOCK:
                total = CONN.execute("SELECT count(*) FROM orders").fetchone()[0]
            self._send(200, str(total).encode("utf-8"), "text/plain")
            return
        self._send(404, b"not found")

    def do_POST(self):
        if self.path.split("?", 1)[0] != "/back/checkout":
            self._send(404, b"not found")
            return
        length = int(self.headers.get("Content-Length", "0"))
        payload = json.loads(self.rfile.read(length).decode("utf-8"))
        sku, qty = payload["sku"], int(payload["qty"])
        with LOCK, CONN.transaction():
            product = CONN.execute(
                "SELECT id, price_cents, stock FROM products WHERE sku = %s FOR UPDATE",
                (sku,),
            ).fetchone()
            new_stock = product[2] - qty
            CONN.execute(
                "UPDATE products SET stock = %s WHERE id = %s",
                (new_stock, product[0]),
            )
            order_id = CONN.execute(
                "INSERT INTO orders (product_id, buyer_email, quantity, total_cents) "
                "VALUES (%s, %s, %s, %s) RETURNING id",
                (product[0], payload["email"], qty, product[1] * qty),
            ).fetchone()[0]
            CONN.execute(
                "INSERT INTO audit_events (kind, payload) VALUES ('order.committed', %s)",
                (json.dumps({"order_id": order_id, "sku": sku}),),
            )
        body = json.dumps(
            {"order_id": order_id, "remaining_stock": new_stock}
        ).encode("utf-8")
        self._send(201, body, "application/json")


http.server.ThreadingHTTPServer(("127.0.0.1", 8000), AppHandler).serve_forever()
"""

# Executed ON THE HOST against one world's single `.localhost` `WorldOrigin`
# (argv: origin, index, ready-file, go-file). Chromium never runs inside a world.
WORLD_TEST_SCRIPT = r"""
import os
import sys
import time
from urllib.parse import urlsplit

from playwright.sync_api import sync_playwright

ORIGIN = sys.argv[1]
TEST_INDEX = int(sys.argv[2])
READY_FILE = sys.argv[3] if len(sys.argv) > 3 else ""
GO_FILE = sys.argv[4] if len(sys.argv) > 4 else ""

host = urlsplit(ORIGIN).hostname or ""
assert host.endswith(".localhost"), f"world origin must be a `.localhost` route, got {ORIGIN}"

sku = f"SKU-{TEST_INDEX + 1:06d}"
with sync_playwright() as playwright:
    browser = playwright.chromium.launch(
        headless=True, executable_path=os.environ["SLIPFORK_BENCH_CHROME"]
    )
    page = browser.new_page()
    if READY_FILE and GO_FILE:
        with open(READY_FILE, "w", encoding="utf-8") as ready:
            ready.write("ready\n")
        while not os.path.exists(GO_FILE):
            time.sleep(0.005)
    page.goto(f"{ORIGIN}/products/{sku}")
    assert page.locator("#stock").inner_text() == "25"
    page.get_by_role("button", name="Complete Checkout").click()
    page.wait_for_selector("#order-confirmed")
    assert page.locator("#remaining-stock").inner_text() == "23"
    orders = page.evaluate("fetch('/orders/count').then(r => r.text())")
    assert orders.strip() == "1", f"expected 1 isolated order, got {orders}"
    browser.close()
"""

# Executed on the host by the traditional baseline (argv: conninfo, num_tests, project root).
BASELINE_TEST_SCRIPT = r"""
import http.server
import json
import os
import sys
import threading

import psycopg
from playwright.sync_api import sync_playwright

CONNINFO = sys.argv[1]
NUM_TESTS = int(sys.argv[2])
PROJECT = sys.argv[3]

with open(os.path.join(PROJECT, "front/src/App.html"), "r", encoding="utf-8") as handle:
    BUNDLE_TEMPLATE = handle.read()
with open(os.path.join(PROJECT, "api/migrations/seed_core.sql"), "r", encoding="utf-8") as handle:
    SEED_CORE = handle.read()
with open(
    os.path.join(PROJECT, "api/migrations/seed_catalog.sql"), "r", encoding="utf-8"
) as handle:
    SEED_CATALOG = handle.read()

conn = psycopg.connect(CONNINFO, autocommit=True)


class AppHandler(http.server.BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        pass

    def do_GET(self):
        sku = self.path.rsplit("/", 1)[-1]
        row = conn.execute(
            "SELECT id, sku, title, price_cents, stock FROM products WHERE sku = %s", (sku,)
        ).fetchone()
        body = (
            BUNDLE_TEMPLATE.replace("__SKU__", row[1])
            .replace("__TITLE__", row[2])
            .replace("__PRICE__", f"{row[3] / 100:.2f}")
            .replace("__STOCK__", str(row[4]))
        ).encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        payload = json.loads(self.rfile.read(int(self.headers["Content-Length"])).decode("utf-8"))
        sku, qty = payload["sku"], int(payload["qty"])
        with conn.transaction():
            product = conn.execute(
                "SELECT id, price_cents, stock FROM products WHERE sku = %s FOR UPDATE", (sku,)
            ).fetchone()
            new_stock = product[2] - qty
            conn.execute(
                "UPDATE products SET stock = %s WHERE id = %s", (new_stock, product[0])
            )
            order_id = conn.execute(
                "INSERT INTO orders (product_id, buyer_email, quantity, total_cents) "
                "VALUES (%s, %s, %s, %s) RETURNING id",
                (product[0], payload["email"], qty, product[1] * qty),
            ).fetchone()[0]
            conn.execute(
                "INSERT INTO audit_events (kind, payload) VALUES ('order.committed', %s)",
                (json.dumps({"order_id": order_id, "sku": sku}),),
            )
        body = json.dumps(
            {"order_id": order_id, "remaining_stock": new_stock}
        ).encode("utf-8")
        self.send_response(201)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


# The host baseline owns an ephemeral loopback port (a developer's own server may hold host
# `:8000`), while every slipfork world still binds the canonical `:8000` in its private netns.
httpd = http.server.HTTPServer(("127.0.0.1", 0), AppHandler)
threading.Thread(target=httpd.serve_forever, daemon=True).start()
BASE = f"http://127.0.0.1:{httpd.server_address[1]}"

with sync_playwright() as playwright:
    browser = playwright.chromium.launch(executable_path=os.environ["SLIPFORK_BENCH_CHROME"])
    for index in range(NUM_TESTS):
        page = browser.new_page()
        sku = f"SKU-{index + 1:06d}"
        page.goto(f"{BASE}/products/{sku}")
        assert page.locator("#stock").inner_text() == "25"
        page.get_by_role("button", name="Complete Checkout").click()
        page.wait_for_selector("#order-confirmed")
        assert page.locator("#remaining-stock").inner_text() == "23"
        page.close()
        # Traditional `transactional_db` + `serialized_rollback` teardown cost.
        conn.execute(
            "TRUNCATE orders, audit_events, products, tenants RESTART IDENTITY CASCADE"
        )
        conn.execute(SEED_CORE)
        conn.execute(SEED_CATALOG)
    browser.close()

conn.close()
httpd.shutdown()
"""


# --------------------------------------------------------------- loopback `ospp-v1` seed server

# `ADR-0008` §1–§2 is the normative protocol. This is a **server-side harness**, never a
# production path: the shipped `slipforkd` still performs a real `CHALLENGE -> HELLO -> WELCOME`
# handshake with real ECDSA P-256 verification, a real `resources.describe`, and a real
# `dump.begin -> binary frames -> dump.end` zstd stream that `DiskSeedDumpStore` re-verifies
# against the advertised digest before any `postgres:18` `pg_restore` runs.

_OSPP_SUBPROTOCOL = "ospp-v1"
_OSPP_CONNECT_PATH = "/_ospp/v1/connect"
_OSPP_PAIR_PATH = "/_ospp/v1/pair"
_OSPP_MAX_CONTROL_FRAME = 16_384
_OSPP_DUMP_CHUNK = 32 * 1024

# P-256 domain parameters (SEC 2 / FIPS 186-4).
_P256_P = 0xFFFFFFFF00000001000000000000000000000000FFFFFFFFFFFFFFFFFFFFFFFF
_P256_A = _P256_P - 3
_P256_B = 0x5AC635D8AA3A93E7B3EBBD55769886BC651D06B0CC53B0F63BCE3C3E27D2604B
_P256_N = 0xFFFFFFFF00000000FFFFFFFFFFFFFFFFBCE6FAADA7179E84F3B9CAC2FC632551
_P256_G = (
    0x6B17D1F2E12C4247F8BCE6E563A440F277037D812DEB33A0F4A13945D898C296,
    0x4FE342E2FE1A7F9B8EE7EB4A7C0F9E162BCE33576B315ECECBB6406837BF51F5,
)


def _point_add(left, right):
    if left is None:
        return right
    if right is None:
        return left
    x1, y1 = left
    x2, y2 = right
    if x1 == x2 and (y1 + y2) % _P256_P == 0:
        return None
    if left == right:
        slope = (3 * x1 * x1 + _P256_A) * pow(2 * y1, -1, _P256_P) % _P256_P
    else:
        slope = (y2 - y1) * pow(x2 - x1, -1, _P256_P) % _P256_P
    x3 = (slope * slope - x1 - x2) % _P256_P
    return x3, (slope * (x1 - x3) - y1) % _P256_P


def _point_mul(scalar, point):
    result = None
    addend = point
    while scalar:
        if scalar & 1:
            result = _point_add(result, addend)
        addend = _point_add(addend, addend)
        scalar >>= 1
    return result


def _decompress_sec1(encoded: bytes):
    """Decode a 33-byte compressed SEC1 point, or `None` when it is not on the curve."""
    if len(encoded) != 33 or encoded[0] not in (2, 3):
        return None
    x = int.from_bytes(encoded[1:], "big")
    if x >= _P256_P:
        return None
    y_squared = (pow(x, 3, _P256_P) + _P256_A * x + _P256_B) % _P256_P
    y = pow(y_squared, (_P256_P + 1) // 4, _P256_P)
    if y * y % _P256_P != y_squared:
        return None
    if (y & 1) != (encoded[0] & 1):
        y = _P256_P - y
    return x, y


def _parse_key_id(key_id: str):
    """`p256:<66 lowercase hex>` -> compressed public point, or `None`."""
    if not key_id.startswith("p256:"):
        return None
    raw = key_id[len("p256:") :]
    if len(raw) != 66 or any(character not in "0123456789abcdef" for character in raw):
        return None
    return _decompress_sec1(bytes.fromhex(raw))


def p256_verify(key_id: str, transcript: bytes, signature_hex: str) -> bool:
    """Verify a raw `r || s` ECDSA-P256/SHA-256 signature over `transcript`."""
    public_point = _parse_key_id(key_id)
    if public_point is None or len(signature_hex) != 128:
        return False
    if any(character not in "0123456789abcdef" for character in signature_hex):
        return False
    raw = bytes.fromhex(signature_hex)
    r = int.from_bytes(raw[:32], "big")
    s = int.from_bytes(raw[32:], "big")
    if not (1 <= r < _P256_N and 1 <= s < _P256_N):
        return False
    digest = hashlib.sha256(transcript).digest()
    e = int.from_bytes(digest, "big")
    inverse = pow(s, -1, _P256_N)
    point = _point_add(
        _point_mul(e * inverse % _P256_N, _P256_G),
        _point_mul(r * inverse % _P256_N, public_point),
    )
    return point is not None and point[0] % _P256_N == r


def connect_transcript(nonce_s: bytes, nonce_c: bytes, server_name: str) -> bytes:
    return b"ospp-v1" + nonce_s + nonce_c + server_name.encode("ascii")


def pairing_transcript(
    nonce_s: bytes, nonce_c: bytes, handle: bytes, server_name: str
) -> bytes:
    return b"ospp-v1-pair" + nonce_s + nonce_c + handle + server_name.encode("ascii")


class _WebSocketClosed(RuntimeError):
    """The peer closed or violated the WebSocket framing."""


class _WebSocket:
    """The minimal RFC 6455 server side of one `ospp-v1` connection (text + binary frames)."""

    def __init__(self, stream: socket.socket) -> None:
        self._stream = stream

    def send_text(self, payload: dict) -> None:
        self._send_frame(0x1, json.dumps(payload).encode("utf-8"))

    def send_binary(self, payload: bytes) -> None:
        self._send_frame(0x2, payload)

    def _send_frame(self, opcode: int, payload: bytes) -> None:
        header = bytearray([0x80 | opcode])
        length = len(payload)
        if length < 126:
            header.append(length)
        elif length < 1 << 16:
            header.append(126)
            header += struct.pack("!H", length)
        else:
            header.append(127)
            header += struct.pack("!Q", length)
        self._stream.sendall(bytes(header) + payload)

    def _read_exact(self, count: int) -> bytes:
        chunks = bytearray()
        while len(chunks) < count:
            chunk = self._stream.recv(count - len(chunks))
            if not chunk:
                raise _WebSocketClosed("connection closed mid-frame")
            chunks += chunk
        return bytes(chunks)

    def recv(self) -> tuple[int, bytes]:
        """Return one `(opcode, payload)` data frame, answering pings transparently."""
        while True:
            first, second = self._read_exact(2)
            opcode = first & 0x0F
            if not second & 0x80:
                raise _WebSocketClosed("client data frames must be masked")
            length = second & 0x7F
            if length == 126:
                length = struct.unpack("!H", self._read_exact(2))[0]
            elif length == 127:
                length = struct.unpack("!Q", self._read_exact(8))[0]
            mask = self._read_exact(4)
            payload = bytearray(self._read_exact(length))
            for index in range(length):
                payload[index] ^= mask[index % 4]
            if opcode == 0x8:
                raise _WebSocketClosed("peer closed")
            if opcode == 0x9:
                self._send_frame(0xA, bytes(payload))
                continue
            if opcode == 0xA:
                continue
            return opcode, bytes(payload)

    def recv_json(self) -> dict:
        opcode, payload = self.recv()
        if opcode != 0x1:
            raise _WebSocketClosed("expected a text control frame")
        if len(payload) > _OSPP_MAX_CONTROL_FRAME:
            raise _WebSocketClosed("control frame exceeds 16 KiB")
        try:
            message = json.loads(payload.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise _WebSocketClosed(f"invalid control frame: {error}") from error
        if not isinstance(message, dict):
            raise _WebSocketClosed("control frame must be a JSON object")
        return message


@dataclass(frozen=True, slots=True)
class SeedDumpOffer:
    """One real `pg_dump --format=custom` archive, zstd-compressed for transport."""

    db_id: str
    compressed: Path
    sha256: str
    compressed_bytes: int


class SeedOsppServer:
    """A minimal loopback `ospp-v1` host that pairs a device key and streams one seed dump."""

    def __init__(self) -> None:
        self._listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self._listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._listener.bind(("127.0.0.1", 0))
        self._listener.listen(8)
        self._listener.settimeout(0.1)
        self._stop = threading.Event()
        self._lock = threading.Lock()
        self._accepted_keys: set[str] = set()
        self._redeemed_handles: set[bytes] = set()
        self._dump: SeedDumpOffer | None = None
        self._pairing_handle = os.urandom(32)
        self._account = "bench@localhost"
        self._thread = threading.Thread(target=self._serve, daemon=True)
        self._thread.start()

    @property
    def port(self) -> int:
        return self._listener.getsockname()[1]

    @property
    def handle_hex(self) -> str:
        return self._pairing_handle.hex()

    @property
    def pairing_url(self) -> str:
        """A fragment-only pairing URL (`http://` is loopback-only, `ADR-0008` §1)."""
        return f"http://127.0.0.1:{self.port}/pair#handle={self.handle_hex}"

    def serve_dump(self, dump: SeedDumpOffer) -> None:
        with self._lock:
            self._dump = dump

    def stop(self) -> None:
        self._stop.set()
        if self._thread.is_alive():
            self._thread.join(timeout=5)
        try:
            self._listener.close()
        except OSError:
            pass

    # -- server internals ---------------------------------------------------

    def _serve(self) -> None:
        while not self._stop.is_set():
            try:
                stream, _ = self._listener.accept()
            except TimeoutError:
                continue
            except OSError:
                return
            stream.settimeout(30)
            threading.Thread(target=self._handle, args=(stream,), daemon=True).start()

    def _handle(self, stream: socket.socket) -> None:
        try:
            path = self._handshake(stream)
            websocket = _WebSocket(stream)
            if path == _OSPP_PAIR_PATH:
                self._pair(websocket)
            elif path == _OSPP_CONNECT_PATH:
                self._connect(websocket)
        except (_WebSocketClosed, OSError, TimeoutError):
            pass
        finally:
            try:
                stream.close()
            except OSError:
                pass

    def _handshake(self, stream: socket.socket) -> str:
        request = bytearray()
        while b"\r\n\r\n" not in request:
            chunk = stream.recv(4096)
            if not chunk:
                raise _WebSocketClosed("connection closed during the upgrade")
            request += chunk
            if len(request) > _OSPP_MAX_CONTROL_FRAME:
                raise _WebSocketClosed("upgrade request exceeds 16 KiB")
        lines = bytes(request).split(b"\r\n")
        request_line = lines[0].decode("ascii", "replace").split(" ")
        path = request_line[1] if len(request_line) > 1 else "/"
        headers = {}
        for line in lines[1:]:
            if b":" in line:
                name, value = line.split(b":", 1)
                headers[name.strip().lower()] = value.strip()
        key = headers.get(b"sec-websocket-key")
        subprotocol = headers.get(b"sec-websocket-protocol", b"").decode("ascii")
        if key is None or _OSPP_SUBPROTOCOL not in subprotocol:
            raise _WebSocketClosed("missing `ospp-v1` subprotocol")
        accept = base64.b64encode(
            hashlib.sha1(key + b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11").digest()
        ).decode("ascii")
        response = (
            "HTTP/1.1 101 Switching Protocols\r\n"
            "Upgrade: websocket\r\n"
            "Connection: Upgrade\r\n"
            f"Sec-WebSocket-Accept: {accept}\r\n"
            f"Sec-WebSocket-Protocol: {_OSPP_SUBPROTOCOL}\r\n\r\n"
        )
        stream.sendall(response.encode("ascii"))
        return path

    def _pair(self, websocket: _WebSocket) -> None:
        nonce_s = os.urandom(32)
        websocket.send_text({"type": "challenge", "nonce_s": nonce_s.hex()})
        hello = websocket.recv_json()
        if hello.get("type") != "pair_hello" or hello.get("version") != 1:
            websocket.send_text({"type": "reject", "reason": {"kind": "bad_signature"}})
            return
        try:
            handle = bytes.fromhex(hello.get("handle", ""))
            nonce_c = bytes.fromhex(hello.get("nonce_c", ""))
        except ValueError:
            websocket.send_text(
                {"type": "reject", "reason": {"kind": "unknown_handle"}}
            )
            return
        with self._lock:
            already_redeemed = handle in self._redeemed_handles
        if len(handle) != 32 or handle != self._pairing_handle:
            websocket.send_text(
                {"type": "reject", "reason": {"kind": "unknown_handle"}}
            )
            return
        if len(nonce_c) != 32 or already_redeemed:
            websocket.send_text(
                {"type": "reject", "reason": {"kind": "already_redeemed"}}
            )
            return
        key_id = hello.get("key_id", "")
        if not p256_verify(
            key_id,
            pairing_transcript(nonce_s, nonce_c, handle, "127.0.0.1"),
            hello.get("sig", ""),
        ):
            websocket.send_text({"type": "reject", "reason": {"kind": "bad_signature"}})
            return
        with self._lock:
            self._accepted_keys.add(key_id)
            self._redeemed_handles.add(handle)
        websocket.send_text({"type": "pair_approved", "account": self._account})

    def _connect(self, websocket: _WebSocket) -> None:
        nonce_s = os.urandom(32)
        websocket.send_text({"type": "challenge", "nonce_s": nonce_s.hex()})
        hello = websocket.recv_json()
        key_id = hello.get("key_id", "")
        with self._lock:
            accepted = key_id in self._accepted_keys
        try:
            nonce_c = bytes.fromhex(hello.get("nonce_c", ""))
        except ValueError:
            nonce_c = b""
        valid = (
            hello.get("type") == "hello"
            and hello.get("version") == 1
            and accepted
            and len(nonce_c) == 32
            and p256_verify(
                key_id,
                connect_transcript(nonce_s, nonce_c, "127.0.0.1"),
                hello.get("sig", ""),
            )
        )
        if not valid:
            reason = "unknown_key" if not accepted else "bad_signature"
            websocket.send_text({"type": "reject", "reason": {"kind": reason}})
            return
        websocket.send_text({"type": "welcome", "version": 1})
        while True:
            request = websocket.recv_json()
            capability = request.get("type")
            request_id = request.get("request_id")
            if not isinstance(request_id, int):
                return
            if not self._dispatch(websocket, capability, request_id, request):
                return

    def _dispatch(
        self, websocket: _WebSocket, capability: object, request_id: int, request: dict
    ) -> bool:
        if capability == "env.bundle":
            websocket.send_text(
                {
                    "type": "env.bundle",
                    "request_id": request_id,
                    "vars": {},
                    "secrets": {},
                    "domains": {},
                }
            )
            return True
        if capability == "secrets.ensure":
            websocket.send_text(
                {
                    "type": "secrets.ensure",
                    "request_id": request_id,
                    "key": request.get("key", ""),
                    "value": request.get("proposed", ""),
                }
            )
            return True
        if capability == "resources.describe":
            websocket.send_text(
                {
                    "type": "resources.describe",
                    "request_id": request_id,
                    "s3_buckets": {},
                    "pg_databases": self._describe(),
                }
            )
            return True
        if capability == "dump.begin":
            return self._stream_dump(websocket, request_id, request.get("db_id"))
        websocket.send_text(
            {
                "type": "capability.error",
                "request_id": request_id,
                "capability": capability,
                "kind": "unsupported_capability",
                "message": "the benchmark seed server offers only seed dumps",
            }
        )
        return True

    def _describe(self) -> dict:
        with self._lock:
            dump = self._dump
        if dump is None:
            return {}
        return {
            dump.db_id: {
                "db_id": dump.db_id,
                "sha256_compressed": dump.sha256,
                "compressed_bytes": dump.compressed_bytes,
                "format": "pg_custom_zstd",
            }
        }

    def _stream_dump(
        self, websocket: _WebSocket, request_id: int, db_id: object
    ) -> bool:
        with self._lock:
            dump = self._dump
        if dump is None or db_id != dump.db_id:
            websocket.send_text(
                {
                    "type": "capability.error",
                    "request_id": request_id,
                    "capability": "dump.begin",
                    "kind": "unknown_resource",
                    "message": "no seed dump is offered for that database id",
                }
            )
            return True
        websocket.send_text(
            {
                "type": "dump.begin",
                "request_id": request_id,
                "db_id": dump.db_id,
                "sha256_compressed": dump.sha256,
                "compressed_bytes": dump.compressed_bytes,
                "format": "pg_custom_zstd",
            }
        )
        with dump.compressed.open("rb") as handle:
            while True:
                chunk = handle.read(_OSPP_DUMP_CHUNK)
                if not chunk:
                    break
                websocket.send_binary(chunk)
        websocket.send_text(
            {
                "type": "dump.end",
                "request_id": request_id,
                "sha256_compressed": dump.sha256,
                "compressed_bytes": dump.compressed_bytes,
            }
        )
        return True


@dataclass(frozen=True, slots=True)
class BenchmarkMetrics:
    """One measured record; both report tables and the JSON block render from this."""

    first_up_ms: float
    pgdata_logical_mib: float
    pgdata_files: int
    world_up_ms: float
    worktree_fork_ms: float
    worktree_exec_ms: float
    parallel_4_forks_ms: float
    pg_restore_ms: float
    dag_warm_plan_ms: float
    dag_front_only_plan_ms: float
    num_tests: int
    workers: int
    baseline_serial_s: float
    baseline_per_test_ms: float
    slipfork_cold_parallel_s: float
    slipfork_cold_per_test_ms: float
    slipfork_warm_pool_s: float
    slipfork_warm_per_test_ms: float
    cold_speedup_factor: float
    warm_speedup_factor: float
    seed: int

    def to_json(self) -> str:
        return json.dumps(asdict(self), indent=2, sort_keys=True)


# --------------------------------------------------------------------------- hermetic environment


def bench_child_env(
    run_dir: Path, extra: dict[str, str] | None = None
) -> dict[str, str]:
    """The exact environment every benchmark child receives.

    Host `PATH`/locale pass through when present; `HOME` and `TMPDIR` are pinned inside `run_dir`
    so no child can write into the developer's home or `/tmp`; `extra` may only carry explicit
    `SLIPFORK_*` variables the run itself sets.

    Pure mapping: the pinned `HOME`/`TMPDIR` directories are created by `prepare_run_state`, so the
    host gate can run against the shipped `slipfork doctor` before anything is written.

    `BROWSER` is pinned to `/bin/true`: `slipfork auth login -` hands the pairing URL to the
    daemon's injected `BrowserOpener` (a desktop convenience) before redeeming the handle over
    real `ospp-v1`. A headless benchmark must not depend on a desktop browser, and the handle is
    still redeemed exactly once over the loopback seed server.
    """
    env: dict[str, str] = {}
    for key in PASSTHROUGH_ENV:
        value = os.environ.get(key)
        if value:
            env[key] = value
    for key in PINNED_ENV:
        env.pop(key, None)
    env["HOME"] = str(run_dir / "home")
    env["TMPDIR"] = str(run_dir / "tmp")
    env["BROWSER"] = "/bin/true"
    for key, value in (extra or {}).items():
        if not key.startswith(CHILD_ENV_PREFIX):
            msg = f"bench child env may only add {CHILD_ENV_PREFIX}* variables, got '{key}'"
            raise BenchError(msg)
        env[key] = value
    return env


def _reject_tmpfs(run_dir: Path) -> None:
    resolved = str(run_dir.resolve()) + os.sep
    for forbidden in TOPMOST_MOUNT_FORBIDDEN:
        if resolved.startswith(forbidden):
            msg = (
                f"bench run directory '{run_dir}' lives on tmpfs ({forbidden}); heavy CoW and "
                f"PostgreSQL state must stay on disk — pass --run-dir"
            )
            raise BenchError(msg)


def _run_doctor(layout: Path, env: dict[str, str], project: Path) -> dict:
    """Run the shipped `slipfork doctor --json` against the state directory this run measures."""
    state_dir = layout / "state"
    try:
        result = subprocess.run(
            [str(SLIPFORK_BIN), "--state-dir", str(state_dir), "doctor", "--json"],
            cwd=project,
            env=env,
            check=False,
            capture_output=True,
            text=True,
            timeout=DOCTOR_TIMEOUT_S,
        )
    except subprocess.TimeoutExpired as error:
        msg = (
            f"`slipfork doctor --json` on '{state_dir}' did not answer within "
            f"{DOCTOR_TIMEOUT_S:.0f}s; the host probe is wedged"
        )
        raise BenchError(msg) from error
    except OSError as error:
        msg = (
            f"cannot run '{SLIPFORK_BIN} doctor --json' to probe '{state_dir}': {error} "
            f"(run `make bench`, which builds target/release/slipfork)"
        )
        raise BenchError(msg) from error
    try:
        report = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        detail = (result.stderr or result.stdout).strip()
        msg = (
            f"`slipfork doctor --json` on '{state_dir}' produced no report "
            f"({error}):\n{detail}"
        )
        raise BenchError(msg) from error
    if not isinstance(report, dict):
        msg = f"`slipfork doctor --json` on '{state_dir}' did not return a JSON object"
        raise BenchError(msg)
    return report


def _failed_doctor_checks(report: dict) -> str:
    """Name the `doctor` checks that are not `ok`, for the diagnostic line only.

    The gate's decision is `report["ok"]` alone; this helper decides nothing. It only renders the
    report's own fields so a failure says which host fact is missing.
    """
    failed: list[str] = []
    filesystem = report.get("filesystem")
    if filesystem != "btrfs":
        failed.append(f"filesystem {filesystem!r}")
    if report.get("btrfs_super_magic") is not True:
        failed.append("btrfs_super_magic")
    for check in ("subvolume_ioctl", "unshare_userns", "netns"):
        outcome = report.get(check)
        status = outcome.get("status") if isinstance(outcome, dict) else None
        if status != "ok":
            failed.append(f"{check} {status!r}")
    return ", ".join(failed) if failed else "no individual check named a failure"


def require_btrfs(layout: Path, env: dict[str, str], project: Path) -> None:
    """Fail closed unless `slipfork doctor` reports `ok` at the state directory this run measures.

    The probe is the shipped `slipfork doctor --json` (`slipfork_engine::btrfs` +
    `crates/slipfork/src/doctor.rs`) run against `<layout>/state` — exactly the `--state-dir` every
    later `slipfork` call receives — so the harness never re-derives a host fact the CLI already
    owns. `project` is only the subprocess working directory and must already exist; `doctor`
    resolves nothing against it, and the run's own sample project does not exist yet because the
    gate runs before the pre-run wipe and before any measurement.

    The gate is the report's own canonical verdict: proceed only when `ok` is `True`. `doctor`
    already folds the Btrfs superblock, the live subvolume ioctl, and real `unshare_userns`/`netns`
    probes into that one verdict — and every measured `up`/`fork`/`exec` boots a world through
    those namespaces, so a namespace failure must stop the run exactly like a non-Btrfs one. Any
    other verdict prints the report's `fixes` verbatim, one per line, and raises: no metric is
    produced and nothing is written (`ADR-0010` §4). The failing fields only shape the diagnostic
    text; the decision is `ok` alone.
    """
    state_dir = layout / "state"
    report = _run_doctor(layout, env, project)
    fixes = report.get("fixes")
    if not isinstance(fixes, list) or not all(isinstance(fix, str) for fix in fixes):
        msg = f"`slipfork doctor --json` on '{state_dir}' did not report a `fixes` list: {report}"
        raise BenchError(msg)
    if report.get("ok") is True:
        return
    msg = "\n".join(
        [
            (
                f"bench state directory '{state_dir}' failed `slipfork doctor` "
                f"({_failed_doctor_checks(report)})"
            ),
            *fixes,
        ]
    )
    raise BenchError(msg)


def prepare_run_state(
    run_dir: Path, *, remover: Callable[[Path], None] = shutil.rmtree
) -> None:
    """Wipe and re-create the run directory fail-closed, before the first measurement.

    The pinned `HOME`/`TMPDIR` scratch directories `bench_child_env` names are created here (and
    only here), so nothing exists under `run_dir` until the host gate has passed.
    """
    _reject_tmpfs(run_dir)
    if run_dir.exists() or run_dir.is_symlink():
        try:
            remover(run_dir)
        except OSError as error:
            msg = (
                f"cannot wipe stale bench run directory '{run_dir}' ({error}); refusing to time "
                f"a non-empty directory — remove it and retry"
            )
            raise BenchError(msg) from error
    if run_dir.exists():
        msg = f"bench run directory '{run_dir}' still exists after the pre-run wipe"
        raise BenchError(msg)
    run_dir.mkdir(parents=True)
    leftovers = sorted(path.name for path in run_dir.iterdir())
    if leftovers:
        msg = f"bench run directory '{run_dir}' is not empty before timing: {leftovers}"
        raise BenchError(msg)
    home = run_dir / "home"
    home.mkdir()
    os.chmod(home, 0o700)
    (run_dir / "tmp").mkdir()


# --------------------------------------------------------------------------- sample project


def run_cmd(
    argv: Sequence[str],
    *,
    cwd: Path | None = None,
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        list(argv), cwd=cwd, env=env, check=True, capture_output=True, text=True
    )


def _git(project: Path, env: dict[str, str], *args: str) -> str:
    return run_cmd(
        [
            "git",
            "-c",
            "user.name=Slipfork Benchmark",
            "-c",
            "user.email=bench@localhost",
            "-c",
            "commit.gpgsign=false",
            *args,
        ],
        cwd=project,
        env=env,
    ).stdout


def setup_sample_project(project: Path, run_dir: Path) -> None:
    """Generate the sample project as a real Git repository (the CLI refuses bare trees)."""
    (project / "front" / "src").mkdir(parents=True, exist_ok=True)
    (project / "api" / "migrations").mkdir(parents=True, exist_ok=True)
    (project / "snow.yml").write_text(SNOW_YML, encoding="utf-8")
    (project / "Dockerfile").write_text(DOCKERFILE, encoding="utf-8")
    (project / "slipfork.toml").write_text(SLIPFORK_TOML, encoding="utf-8")
    (project / "front" / "src" / "App.html").write_text(FRONT_HTML, encoding="utf-8")
    (project / "api" / "migrations" / "0001_schema.sql").write_text(
        SCHEMA_SQL, encoding="utf-8"
    )
    (project / "api" / "migrations" / "seed_core.sql").write_text(
        SEED_CORE_SQL, encoding="utf-8"
    )
    (project / "api" / "migrations" / "seed_catalog.sql").write_text(
        SEED_CATALOG_SQL, encoding="utf-8"
    )
    # Every in-world script is committed so fork worktrees contain it.
    (project / "api" / "app.py").write_text(APP_SCRIPT, encoding="utf-8")
    (project / "run_one_world_test.py").write_text(WORLD_TEST_SCRIPT, encoding="utf-8")
    (project / "run_baseline.py").write_text(BASELINE_TEST_SCRIPT, encoding="utf-8")
    env = bench_child_env(run_dir)
    # Seal Git discovery so sample-project commands can never resolve the outer repository.
    env["GIT_CEILING_DIRECTORIES"] = str(run_dir)
    _git(project, env, "init", "-q")
    _git(project, env, "add", ".")
    _git(project, env, "commit", "-q", "-m", "benchmark sample project")


# --------------------------------------------------------------------------- PostgreSQL fixtures


def _pg_start(pgdata: Path, sock: Path, log: Path, env: dict[str, str]) -> None:
    sock.mkdir(parents=True, exist_ok=True)
    run_cmd(
        [
            str(PG_BIN / "pg_ctl"),
            "-D",
            str(pgdata),
            "-l",
            str(log),
            "-o",
            (
                f"-F -c full_page_writes=off -c unix_socket_directories={sock} "
                "-c listen_addresses="
            ),
            "start",
        ],
        env=env,
    )


def _pg_stop(pgdata: Path, env: dict[str, str]) -> None:
    run_cmd([str(PG_BIN / "pg_ctl"), "-D", str(pgdata), "-m", "fast", "stop"], env=env)


def _psql(sock: Path, sql: str, env: dict[str, str]) -> None:
    run_cmd(
        [
            str(PG_BIN / "psql"),
            "-h",
            str(sock),
            "-d",
            "postgres",
            "-v",
            "ON_ERROR_STOP=1",
            "-c",
            sql,
        ],
        env=env,
    )


def _initdb(pgdata: Path, env: dict[str, str]) -> None:
    run_cmd(
        [
            str(PG_BIN / "initdb"),
            "-D",
            str(pgdata),
            "-A",
            "trust",
            "--no-sync",
            "--locale=C",
            "-E",
            "UTF8",
        ],
        env=env,
    )


def _pgdata_size(pgdata: Path) -> tuple[float, int]:
    files = [path for path in pgdata.rglob("*") if path.is_file()]
    total = sum(path.stat().st_size for path in files)
    return total / (1024.0 * 1024.0), len(files)


def measure_restore_pipeline(
    seed_src: Path, restore_target: Path, env: dict[str, str]
) -> tuple[float, Path]:
    """Measure a real `initdb -> seed -> pg_dump -> pg_restore` pipeline (`M1.7` #7).

    Returns the measured restore time and the real PostgreSQL 18 `pg_dump --format=custom`
    archive (`-Z zstd` data blocks) that `slipforkd` later restores into `postgres:18`.
    """
    _initdb(seed_src, env)
    sock = seed_src / "pgsock"
    _pg_start(seed_src, sock, seed_src / "pg-seed.log", env)
    dump = seed_src.parent / "seed-catalog.pgdump"
    try:
        for sql in (SCHEMA_SQL, SEED_CORE_SQL, SEED_CATALOG_SQL, "CHECKPOINT;"):
            _psql(sock, sql, env)
        run_cmd(
            [
                str(PG_BIN / "pg_dump"),
                "-h",
                str(sock),
                "-d",
                "postgres",
                "-Fc",
                "-Z",
                "zstd",
                "-f",
                str(dump),
            ],
            env=env,
        )
    finally:
        _pg_stop(seed_src, env)
    if not dump.is_file() or dump.stat().st_size == 0:
        msg = f"`pg_dump -Fc` produced no archive at '{dump}'"
        raise BenchError(msg)

    _initdb(restore_target, env)
    target_sock = restore_target / "pgsock"
    _pg_start(restore_target, target_sock, restore_target / "pg-restore.log", env)
    try:
        started = time.perf_counter()
        run_cmd(
            [
                str(PG_BIN / "pg_restore"),
                "-h",
                str(target_sock),
                "-d",
                "postgres",
                str(dump),
            ],
            env=env,
        )
        elapsed_ms = (time.perf_counter() - started) * 1000.0
        _psql(target_sock, "CHECKPOINT;", env)
    finally:
        _pg_stop(restore_target, env)
    return elapsed_ms, dump


def compress_seed_dump(dump: Path, env: dict[str, str]) -> SeedDumpOffer:
    """zstd-compress the real archive and pin the transport digest `ospp-v1` advertises."""
    compressed = dump.with_suffix(dump.suffix + ".zst")
    run_cmd(
        [str(ZSTD_BIN), "-q", "-3", "-f", str(dump), "-o", str(compressed)],
        env=env,
    )
    if not compressed.is_file() or compressed.stat().st_size == 0:
        msg = f"`zstd` produced no archive at '{compressed}'"
        raise BenchError(msg)
    return SeedDumpOffer(
        db_id="shop",
        compressed=compressed,
        sha256=_sha256(compressed),
        compressed_bytes=compressed.stat().st_size,
    )


def run_baseline_serial_suite(
    project: Path, run_dir: Path, conninfo: str, num_tests: int
) -> float:
    child_env = bench_child_env(run_dir, {"SLIPFORK_BENCH_CHROME": str(CHROME_BIN)})
    started = time.perf_counter()
    result = subprocess.run(
        [
            PYTHON_EXE,
            str(project / "run_baseline.py"),
            conninfo,
            str(num_tests),
            str(project),
        ],
        env=child_env,
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        detail = (result.stderr or result.stdout).strip()
        msg = f"traditional baseline suite failed:\n{detail}"
        raise BenchError(msg)
    return time.perf_counter() - started


# --------------------------------------------------------------------------- 0.2.0 world CLI


def _slipfork(
    layout: Path,
    env: dict[str, str],
    project: Path,
    *args: str,
    stdin: str | None = None,
) -> subprocess.CompletedProcess[str]:
    argv = [
        str(SLIPFORK_BIN),
        "--state-dir",
        str(layout / "state"),
        "--runtime-dir",
        str(layout / "runtime"),
        *args,
    ]
    try:
        result = subprocess.run(
            argv,
            cwd=project,
            env=env,
            check=False,
            capture_output=True,
            text=True,
            input=stdin,
            timeout=CLI_TIMEOUT_S,
        )
    except subprocess.TimeoutExpired as error:
        msg = (
            f"`slipfork {' '.join(args)}` did not finish within {CLI_TIMEOUT_S:.0f}s; "
            f"the world engine is wedged"
        )
        raise BenchError(msg) from error
    if result.returncode != 0:
        diagnostic = (result.stderr or result.stdout).strip()
        msg = f"`slipfork {' '.join(args)}` failed with {result.returncode}:\n{diagnostic}"
        raise BenchError(msg)
    return result


def _slipfork_json(
    layout: Path,
    env: dict[str, str],
    project: Path,
    *args: str,
    stdin: str | None = None,
) -> dict:
    output = _slipfork(layout, env, project, "--json", *args, stdin=stdin).stdout
    return json.loads(output)


def _origin_of(summary: dict) -> str:
    origin = summary.get("origin")
    if not isinstance(origin, str) or not origin.startswith("http"):
        msg = f"`slipfork --json` did not return a world origin: {summary}"
        raise BenchError(msg)
    return origin


def _fork(layout: Path, env: dict[str, str], project: Path, name: str) -> str:
    return _origin_of(_slipfork_json(layout, env, project, "fork", name))


def _drop(layout: Path, env: dict[str, str], project: Path, name: str) -> None:
    _slipfork(layout, env, project, "drop", name)


def _exec(
    layout: Path,
    env: dict[str, str],
    project: Path,
    world: str | None,
    cwd: Path,
    argv: Sequence[str],
) -> None:
    world_args = ["--world", world] if world is not None else []
    _slipfork(
        layout,
        env,
        project,
        "exec",
        *world_args,
        "--cwd",
        str(cwd),
        "--",
        *argv,
    )


def _worktree(project: Path, name: str) -> Path:
    return project / ".worktrees" / name


READY_FILE_NAME = ".slipfork-bench-ready"
GO_FILE_NAME = ".slipfork-bench-go"


class SlipforkDaemon:
    """The real `slipforkd` composition root, owned by this harness for deterministic teardown."""

    def __init__(self, process: subprocess.Popen, run_dir: Path) -> None:
        self._process = process
        self._run_dir = run_dir

    @classmethod
    def start(cls, layout: Path, env: dict[str, str]) -> SlipforkDaemon:
        (layout / "runtime").mkdir(parents=True, exist_ok=True)
        log_path = layout / "runtime" / "daemon.log"
        handle = open(log_path, "wb")  # noqa: SIM115 - owned until the daemon exits
        os.chmod(log_path, 0o600)
        process = subprocess.Popen(
            [
                str(SLIPFORKD_BIN),
                "--listen",
                "127.0.0.1:0",
                "--state-dir",
                str(layout / "state"),
                "--runtime-dir",
                str(layout / "runtime"),
            ],
            cwd=layout,
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=handle,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        socket_path = layout / "runtime" / "daemon.sock"
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            if process.poll() is not None:
                tail = log_path.read_text(encoding="utf-8", errors="replace")
                msg = f"`slipforkd` exited before serving {socket_path}:\n{tail}"
                raise BenchError(msg)
            if socket_path.exists():
                probe = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                try:
                    probe.connect(str(socket_path))
                except OSError:
                    pass
                else:
                    probe.close()
                    handle.close()
                    return cls(process, layout)
                finally:
                    probe.close()
            time.sleep(0.05)
        process.kill()
        msg = f"`slipforkd` did not serve {socket_path} within 20s"
        raise BenchError(msg)

    def stop(self) -> None:
        """Stop the daemon, then assert that no world process survives the run."""
        if self._process.poll() is None:
            try:
                os.killpg(self._process.pid, 15)
            except ProcessLookupError:
                pass
            try:
                self._process.wait(timeout=20)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(self._process.pid, 9)
                except ProcessLookupError:
                    pass
                self._process.wait(timeout=20)
        leftovers = _leftover_processes(self._run_dir)
        if leftovers:
            for pid in leftovers:
                try:
                    os.kill(pid, 9)
                except ProcessLookupError:
                    pass
            time.sleep(0.2)
            leftovers = _leftover_processes(self._run_dir)
        if leftovers:
            msg = f"benchmark leaked world processes after teardown: {leftovers}"
            raise BenchError(msg)


def _leftover_processes(run_dir: Path) -> list[int]:
    """Every live `slipforkd`/`slipfork-world-init` process scoped to this run directory."""
    found: list[int] = []
    for pattern in (
        f"target/release/slipforkd --listen 127.0.0.1:0 --state-dir {run_dir}/state",
        f"slipfork-world-init .*{run_dir}",
    ):
        result = subprocess.run(
            ["pgrep", "-f", pattern],
            capture_output=True,
            text=True,
            check=False,
        )
        for line in result.stdout.split():
            if line.isdigit() and int(line) != os.getpid():
                found.append(int(line))
    return sorted(set(found))


def _host_test_argv(
    project: Path, origin: str, index: int, ready: str = "", go: str = ""
) -> list[str]:
    """The exact argv of one host-side Playwright test child (decision-frame or not)."""
    return [
        PYTHON_EXE,
        str(project / "run_one_world_test.py"),
        origin,
        str(index),
        ready,
        go,
    ]


def _run_test_on_host(
    env: dict[str, str], project: Path, origin: str, index: int
) -> None:
    """Drive host Chromium against one world's `.localhost` origin."""
    result = subprocess.run(
        _host_test_argv(project, origin, index),
        cwd=project,
        env=env,
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        detail = (result.stderr or result.stdout).strip()
        msg = f"host browser test {index} against {origin} failed:\n{detail}"
        raise BenchError(msg)


def measure_cold_parallel_suite(
    env: dict[str, str],
    project: Path,
    layout: Path,
    order: Sequence[int],
    workers: int,
) -> float:
    """On-demand cold worlds: `fork` (boots `postgres:18` + the app) + cold host Chromium."""

    def run_one(index: int) -> None:
        name = f"cold-w-{index}"
        origin = _fork(layout, env, project, name)
        try:
            _run_test_on_host(env, project, origin, index)
        finally:
            _drop(layout, env, project, name)

    started = time.perf_counter()
    with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as pool:
        list(pool.map(run_one, order))
    return time.perf_counter() - started


def measure_warm_pool_suite(
    env: dict[str, str],
    project: Path,
    run_dir: Path,
    order: Sequence[int],
    workers: int,
) -> float:
    """Pre-warmed pool: fork a wave, launch its host browser, then time only the test."""
    total = 0.0
    for start in range(0, len(order), workers):
        indices = list(order[start : start + workers])
        worlds: list[tuple[int, str, Path, Path, subprocess.Popen[str]]] = []
        try:
            for index in indices:
                name = f"warm-w-{index}"
                origin = _fork(run_dir, env, project, name)
                handshake = run_dir / "handshake"
                handshake.mkdir(exist_ok=True)
                ready = handshake / f"{index}{READY_FILE_NAME}"
                go = handshake / f"{index}{GO_FILE_NAME}"
                process = subprocess.Popen(
                    _host_test_argv(project, origin, index, str(ready), str(go)),
                    cwd=project,
                    env=env,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    text=True,
                )
                worlds.append((index, name, ready, go, process))
            for index, _, ready, _, process in worlds:
                while not ready.exists():
                    if process.poll() is not None:
                        _, err = process.communicate()
                        msg = f"warm world {index} died before it was ready: {err}"
                        raise BenchError(msg)
                    time.sleep(0.005)
            started = time.perf_counter()
            for _, _, _, go, _ in worlds:
                go.write_text("go\n", encoding="utf-8")
            for index, _, _, _, process in worlds:
                _, err = process.communicate()
                if process.returncode != 0:
                    msg = f"warm test {index} failed: {err}"
                    raise BenchError(msg)
            total += time.perf_counter() - started
        finally:
            for _, name, _, _, process in worlds:
                if process.poll() is None:
                    process.kill()
                    process.communicate()
                _drop(run_dir, env, project, name)
    return total


# --------------------------------------------------------------------------- measurement


def preflight() -> None:
    missing = [
        str(path)
        for path in (
            SLIPFORK_BIN,
            SLIPFORKD_BIN,
            PG_BIN / "initdb",
            PG_BIN / "pg_dump",
            PG_BIN / "pg_restore",
            CHROME_BIN,
            ZSTD_BIN,
        )
        if not path.exists()
    ]
    if missing:
        msg = (
            "benchmark prerequisites are missing: "
            + ", ".join(missing)
            + " (run `make bench`, which builds target/release/slipfork + slipforkd; install "
            "PostgreSQL 18 server binaries under /usr/lib/postgresql/18/bin, Google Chrome, and "
            "`zstd`)"
        )
        raise BenchError(msg)
    try:
        run_cmd([str(PG_BIN / "initdb"), "--version"])
        run_cmd([str(PG_BIN / "pg_dump"), "--version"])
        run_cmd([str(SLIPFORK_BIN), "--version"])
    except (OSError, subprocess.CalledProcessError) as error:
        msg = f"benchmark prerequisite probe failed: {error}"
        raise BenchError(msg) from error


def measure_up_ms(
    layout: Path, env: dict[str, str], project: Path
) -> tuple[float, dict]:
    """Time one real `slipfork up` and return its parsed summary.

    The first `up` on a fresh state directory is the only CLI operation that seals the seeded
    world into its read-only base snapshot (`BtrfsEngine::create_snapshot` + `freeze_world`); the
    CLI exposes no isolated seal command, so the reported window is the whole `up`. A later `up`
    from the already-sealed base clones it writably and boots its services.
    """
    started = time.perf_counter()
    summary = _slipfork_json(layout, env, project, "up")
    return (time.perf_counter() - started) * 1000.0, summary


def measure_fork_ms(
    layout: Path, env: dict[str, str], project: Path, name: str
) -> float:
    """Time one `slipfork fork`: worktree plus a writable clone of the sealed base subvolume."""
    started = time.perf_counter()
    _fork(layout, env, project, name)
    return (time.perf_counter() - started) * 1000.0


def measure_dag_plan_ms(
    layout: Path,
    env: dict[str, str],
    project: Path,
    plan_args: Sequence[str],
    *,
    repeats: int = 3,
) -> tuple[float, dict]:
    """Best-of-`repeats` `slipfork dag plan` wall time plus the last parsed plan.

    `dag plan` is read-only and deterministic, so the fastest of three identical invocations is
    the honest Merkle-hash cost; a loaded developer host otherwise inflates a single sample.
    """
    best = None
    plan: dict = {}
    for _ in range(repeats):
        started = time.perf_counter()
        plan = _slipfork_json(layout, env, project, *plan_args)
        elapsed_ms = (time.perf_counter() - started) * 1000.0
        best = elapsed_ms if best is None else min(best, elapsed_ms)
    assert best is not None
    return best, plan


def measure_all(
    num_tests: int,
    workers: int,
    seed: int,
    run_dir: Path,
) -> BenchmarkMetrics:
    # The host gate is the shipped `slipfork doctor --json` on the state directory this run
    # measures. It runs before the pre-run wipe, before preflight, and before any measurement, so a
    # host `doctor` rejects prints the report's fixes and no metric at all (`ADR-0010` §4). The
    # gate's working directory is the repository root: `doctor` resolves nothing against it and the
    # run's sample project is created only after the gate passes.
    env = bench_child_env(run_dir, {"SLIPFORK_BENCH_CHROME": str(CHROME_BIN)})
    require_btrfs(run_dir, env, REPO_ROOT)
    prepare_run_state(run_dir)
    preflight()
    project = run_dir / "project"
    project.mkdir(parents=True)
    setup_sample_project(project, run_dir)

    server = SeedOsppServer()
    daemon: SlipforkDaemon | None = None
    try:
        daemon = SlipforkDaemon.start(run_dir, env)

        resolved = _slipfork_json(
            run_dir,
            env,
            project,
            "snow",
            "resolve",
            "--file",
            str(project / "snow.yml"),
            "--mode",
            "local",
        )
        if resolved.get("SECRET_KEY") != "[REDACTED]":
            msg = "`slipfork snow resolve` must redact SECRET_KEY"
            raise BenchError(msg)

        # A real `pg_dump -Fc` from real PostgreSQL 18, restored once into the sealed
        # `<project>-base` world over a loopback `ospp-v1` seed server (`ADR-0008` §2).
        restore_ms, dump = measure_restore_pipeline(
            run_dir / "seed-src", run_dir / "restore-target", env
        )
        pg_mib, pg_files = _pgdata_size(run_dir / "restore-target")
        server.serve_dump(compress_seed_dump(dump, env))

        # Pair the daemon's real device key through a real `pair_hello`, then let `up` restore the
        # streamed dump inside `postgres:18` and seal the base snapshot with `freeze_world`.
        _slipfork_json(
            run_dir, env, project, "auth", "login", "-", stdin=f"{server.pairing_url}\n"
        )

        # The first `up` on a fresh state directory seals the seeded world's read-only base
        # snapshot and boots; the CLI exposes no isolated seal command, so the measured window is
        # this whole composite `up`.
        first_up_ms, up_summary = measure_up_ms(run_dir, env, project)
        origin = _origin_of(up_summary)
        url_before_fragment, _, url_fragment = server.pairing_url.partition("#")
        if (
            "handle=" in url_before_fragment
            or server.handle_hex in url_before_fragment
            or url_fragment != f"handle={server.handle_hex}"
        ):
            msg = (
                "the seed server pairing URL must carry its handle only in the `#handle=` "
                f"fragment, got '{url_before_fragment}#{url_fragment}'"
            )
            raise BenchError(msg)
        if not any(
            row["id"] == "shop" and row["resolved"] == "remote"
            for row in up_summary.get("resources", [])
        ):
            msg = (
                "`slipfork up` did not resolve the seeded base dump from the OSPP offer: "
                f"{up_summary.get('resources')}"
            )
            raise BenchError(msg)

        # The canonical world boots from the seeded base: no post-boot SQL seed exists.
        _run_test_on_host(env, project, origin, 0)
        _slipfork(run_dir, env, project, "down")

        # A second `up` from the already-sealed base clones it writably and boots its
        # `postgres:18` + app services: the warm-SQL window.
        world_up_ms, _ = measure_up_ms(run_dir, env, project)
        _slipfork(run_dir, env, project, "down")

        fork_ms = measure_fork_ms(run_dir, env, project, "fork-probe")
        try:
            exec_started = time.perf_counter()
            _exec(
                run_dir,
                env,
                project,
                "fork-probe",
                _worktree(project, "fork-probe"),
                ["sh", "-c", "true"],
            )
            exec_ms = (time.perf_counter() - exec_started) * 1000.0
        finally:
            _drop(run_dir, env, project, "fork-probe")

        def mutate(index: int) -> None:
            name = f"parallel-{index}"
            _fork(run_dir, env, project, name)
            try:
                _exec(
                    run_dir,
                    env,
                    project,
                    name,
                    _worktree(project, name),
                    [
                        "python3",
                        "-c",
                        (
                            "import os, psycopg;"
                            "conn = psycopg.connect(os.environ['DATABASE_URL'], "
                            "user='postgres', autocommit=True);"
                            "assert conn.execute("
                            "'SELECT count(*) FROM products').fetchone()[0] == 10000;"
                            'conn.execute("INSERT INTO orders (product_id, buyer_email, '
                            "quantity, total_cents) VALUES (1, 'bench@localhost', 1, 1500)\");"
                            "assert conn.execute("
                            "'SELECT count(*) FROM orders').fetchone()[0] == 1"
                        ),
                    ],
                )
            finally:
                _drop(run_dir, env, project, name)

        parallel_started = time.perf_counter()
        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
            list(pool.map(mutate, range(4)))
        parallel_4_ms = (time.perf_counter() - parallel_started) * 1000.0

        plan_args = (
            "dag",
            "plan",
            "--file",
            str(project / "slipfork.toml"),
            "--repo",
            str(project),
        )
        plan = _slipfork_json(run_dir, env, project, *plan_args)

        dag_warm_ms, warm_plan = measure_dag_plan_ms(run_dir, env, project, plan_args)
        if [item["digest"] for item in warm_plan["artifacts"]] != [
            item["digest"] for item in plan["artifacts"]
        ]:
            msg = "`dag plan` must be deterministic on an unchanged tree"
            raise BenchError(msg)

        # Traditional baseline: host PostgreSQL 18 + one host Chromium.
        baseline_src = run_dir / "baseline"
        _initdb(baseline_src / "pgdata", env)
        baseline_sock = baseline_src / "pgsock"
        _pg_start(
            baseline_src / "pgdata",
            baseline_sock,
            baseline_src / "pg.log",
            env,
        )
        try:
            for sql in (SCHEMA_SQL, SEED_CORE_SQL, SEED_CATALOG_SQL, "CHECKPOINT;"):
                _psql(baseline_sock, sql, env)
            conninfo = f"postgresql:///postgres?host={baseline_sock}"
            baseline_s = run_baseline_serial_suite(
                project, run_dir, conninfo, num_tests
            )
        finally:
            _pg_stop(baseline_src / "pgdata", env)

        # CoW world suites: the seeded base world is the fork source. One explicit seed decides
        # the deterministic dispatch order (never a host- or timing-dependent order).
        order = list(range(num_tests))
        random.Random(seed).shuffle(order)
        cold_s = measure_cold_parallel_suite(env, project, run_dir, order, workers)
        warm_s = measure_warm_pool_suite(env, project, run_dir, order, workers)

        # Orthogonal invalidation is measured last so every fork above sees a clean tree.
        app_html = project / "front" / "src" / "App.html"
        app_html.write_text(
            app_html.read_text(encoding="utf-8") + "<!-- css tweak -->\n",
            encoding="utf-8",
        )
        dag_front_ms, front_plan = measure_dag_plan_ms(run_dir, env, project, plan_args)
        front = next(item for item in front_plan["artifacts"] if item["id"] == "front")
        if front["digest"] == next(
            item["digest"] for item in plan["artifacts"] if item["id"] == "front"
        ):
            msg = "editing `front/src` must change the `front` artifact digest"
            raise BenchError(msg)
        if [
            [fixture["digest"] for fixture in wave]
            for wave in front_plan["fixture_waves"]
        ] != [
            [fixture["digest"] for fixture in wave] for wave in plan["fixture_waves"]
        ]:
            msg = "editing `front/src` must never invalidate the DB fixtures"
            raise BenchError(msg)

        return BenchmarkMetrics(
            first_up_ms=first_up_ms,
            pgdata_logical_mib=pg_mib,
            pgdata_files=pg_files,
            world_up_ms=world_up_ms,
            worktree_fork_ms=fork_ms,
            worktree_exec_ms=exec_ms,
            parallel_4_forks_ms=parallel_4_ms,
            pg_restore_ms=restore_ms,
            dag_warm_plan_ms=dag_warm_ms,
            dag_front_only_plan_ms=dag_front_ms,
            num_tests=num_tests,
            workers=workers,
            baseline_serial_s=baseline_s,
            baseline_per_test_ms=(baseline_s / num_tests) * 1000.0,
            slipfork_cold_parallel_s=cold_s,
            slipfork_cold_per_test_ms=(cold_s / num_tests) * 1000.0,
            slipfork_warm_pool_s=warm_s,
            slipfork_warm_per_test_ms=(warm_s / num_tests) * 1000.0,
            cold_speedup_factor=baseline_s / cold_s,
            warm_speedup_factor=baseline_s / warm_s,
            seed=seed,
        )
    finally:
        active_error = sys.exc_info()[0] is not None
        problems = _teardown(run_dir, env, project, daemon, server)
        if problems and not active_error:
            msg = "benchmark teardown failed: " + "; ".join(problems)
            raise BenchError(msg)
        if problems:
            print(
                f"run_benchmark: teardown also failed: {'; '.join(problems)}",
                file=sys.stderr,
            )


def _teardown(
    layout: Path,
    env: dict[str, str],
    project: Path,
    daemon: SlipforkDaemon | None,
    server: SeedOsppServer,
) -> list[str]:
    """Release worlds, stop the daemon, stop the seed server — never skipping a later step.

    Every step is attempted even when an earlier one fails, and the caller decides whether a
    teardown failure is raised (only when no original error is already propagating). Returns the
    human-readable problems instead of raising, so the daemon/server stop can never be skipped.
    """
    problems: list[str] = []
    for args in (("down",), ("drop", "fork-probe")):
        try:
            _slipfork(layout, env, project, *args)
        except (BenchError, OSError) as error:
            problems.append(str(error))
    if daemon is not None:
        try:
            daemon.stop()
        except (BenchError, OSError) as error:
            problems.append(str(error))
    try:
        server.stop()
    except OSError as error:
        problems.append(f"cannot stop the seed server: {error}")
    return problems


# --------------------------------------------------------------------------- projections


def primitive_rows(metrics: BenchmarkMetrics) -> list[tuple[str, str, str]]:
    return [
        (
            "first `up` — seals the read-only base snapshot and boots (composite)",
            f"{metrics.first_up_ms:.1f} ms",
            (
                "Window: the whole first `up` on a fresh state directory — `L0` rootfs "
                "materialization, streamed seed restore, `freeze_world` seal, writable clone, "
                "service boot (the CLI exposes no isolated seal command)"
            ),
        ),
        (
            "Seeded PostgreSQL 18 cluster (`PGDATA`)",
            f"{metrics.pgdata_logical_mib:.1f} MiB",
            f"{metrics.pgdata_files} files: 200 tenants + 10,000 products + 5,000 audit events",
        ),
        (
            "`initdb -> seed -> pg_dump -> pg_restore` (PostgreSQL 18)",
            f"{metrics.pg_restore_ms:.1f} ms",
            "Fresh cluster restored from the real `pg_dump -Fc` archive, then checkpointed",
        ),
        (
            "`slipfork up` from the sealed base (writable clone + `crun` PostgreSQL service)",
            f"{metrics.world_up_ms:.1f} ms",
            (
                "Writable clone of the sealed base subvolume, then `postgres:18` + app readiness "
                "(the warm-SQL window)"
            ),
        ),
        (
            "`slipfork fork` (worktree + writable clone)",
            f"{metrics.worktree_fork_ms:.1f} ms",
            (
                "Worktree plus a writable clone of the sealed base subvolume "
                "(`BtrfsEngine::clone_world`)"
            ),
        ),
        (
            "`slipfork exec` (warm world, `sh -c true`)",
            f"{metrics.worktree_exec_ms:.1f} ms",
            "Daemon control round-trip + namespace entry + child exit",
        ),
        (
            "**4 parallel worlds** mutating `:5432` concurrently",
            f"{metrics.parallel_4_forks_ms:.1f} ms total",
            "Fork + execute + drop in parallel; every world binds `127.0.0.1:5432`",
        ),
        (
            "`slipfork dag plan` (unchanged tree)",
            f"{metrics.dag_warm_plan_ms:.2f} ms",
            "SHA-256 Merkle check across `front` + `migrate` + `seed_*` -> identical digests",
        ),
        (
            "`slipfork dag plan` after editing `front/src/App.html`",
            f"{metrics.dag_front_only_plan_ms:.2f} ms",
            "`artifact:front` digest changes; every DB fixture digest is unchanged (0 re-seed)",
        ),
    ]


def suite_rows(
    metrics: BenchmarkMetrics,
) -> list[tuple[str, str, str, str, str]]:
    return [
        (
            "Traditional baseline (host DB, one Chromium, `TRUNCATE` + re-seed, 1 worker)",
            f"{metrics.baseline_serial_s:.2f} s",
            f"{metrics.baseline_per_test_ms:.0f} ms",
            "1.00x",
            f"Replays the seed {metrics.num_tests} times",
        ),
        (
            f"Slipfork cold CoW worlds (`-j {metrics.workers}`)",
            f"{metrics.slipfork_cold_parallel_s:.2f} s",
            f"{metrics.slipfork_cold_per_test_ms:.0f} ms",
            f"{metrics.cold_speedup_factor:.2f}x",
            "0 ms (drop the CoW world)",
        ),
        (
            f"Slipfork pre-warmed CoW pool (`-j {metrics.workers}`)",
            f"{metrics.slipfork_warm_pool_s:.2f} s",
            f"{metrics.slipfork_warm_per_test_ms:.0f} ms",
            f"{metrics.warm_speedup_factor:.2f}x",
            "0 ms (drop the CoW world)",
        ),
    ]


PRIMITIVE_HEADERS = ("Operation", "Measured", "What happened")
SUITE_HEADERS = (
    "Execution mode",
    "Wall time",
    "Effective / test",
    "Speedup vs baseline",
    "SQL teardown",
)


def render_markdown_table(headers: Sequence[str], rows: Sequence[Sequence[str]]) -> str:
    lines = [
        "| " + " | ".join(headers) + " |",
        "|" + "|".join("---" for _ in headers) + "|",
    ]
    for row in rows:
        lines.append("| " + " | ".join(cell.replace("|", "\\|") for cell in row) + " |")
    return "\n".join(lines)


def render_html_table(headers: Sequence[str], rows: Sequence[Sequence[str]]) -> str:
    def cell(value: str) -> str:
        return value.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")

    lines = ["<table>", "  <thead>", "    <tr>"]
    lines.extend(f"      <th>{cell(header)}</th>" for header in headers)
    lines.extend(["    </tr>", "  </thead>", "  <tbody>"])
    for row in rows:
        lines.append("    <tr>")
        lines.extend(f"      <td>{cell(value)}</td>" for value in row)
        lines.append("    </tr>")
    lines.extend(["  </tbody>", "</table>"])
    return "\n".join(lines)


def render_report(
    metrics: BenchmarkMetrics, *, report_date: str, host_kernel: str, host_arch: str
) -> str:
    primitives = render_markdown_table(PRIMITIVE_HEADERS, primitive_rows(metrics))
    suites = render_markdown_table(SUITE_HEADERS, suite_rows(metrics))
    return f"""# PoC Benchmark Report (`docs/research/m1-poc-benchmark.md`)

- **Measured:** {report_date} (seed `{metrics.seed}`)
- **Host:** Linux `{host_kernel}` (`{host_arch}`), Btrfs (`slipfork doctor`: `filesystem: btrfs`,
  `btrfs_super_magic: ok`), PostgreSQL `18`
- **Driver:** `BtrfsEngine` over one Btrfs subvolume per world (`KernelBtrfsDriver`), driven by
  `slipfork` `up`/`fork`/`exec`/`drop` over the harness-owned `slipforkd`
- **Reproduce:** `make bench` (read-only: re-measures and fails on drift) or `make bench-update`
- **Topology:** `0.2.0` read-only `Dockerfile`-derived `L0` rootfs plus a daemonless `crun`
  `postgres:18` container inside each world's private `netns` (`ADR-0009`). The seed is a real
  PostgreSQL 18 `pg_dump -Fc`, zstd-compressed and streamed over loopback `ospp-v1`; `slipforkd`
  restores it once into `<project>-base`, seals that subvolume read-only, and every `fork` clones
  the sealed subvolume. Chromium runs in the shared **host** browser pool and reaches each world's
  single `.localhost` origin. Earlier M1 figures in this file were measured on the pre-`ADR-0009`
  **host-shared-`/usr`** topology (host `postgres` + host Chromium over `/usr` + `/lib`) and are
  not comparable.

---

## 1. Core primitive latencies (live measured)

{primitives}

---

## 2. Head-to-head browser + committed-SQL suite ({metrics.num_tests} heavy tests)

Every test drives **host** headless Chromium (`CHROME_BIN`) against the world's single `.localhost`
`WorldOrigin`, clicks **Complete Checkout** (`POST /back/checkout`), and commits a
`SELECT ... FOR UPDATE` + `UPDATE products` + `INSERT INTO orders` + `INSERT INTO audit_events`
PostgreSQL 18 transaction inside the world's `postgres:18` container:

{suites}

### What the numbers mean

- Btrfs subvolume cloning is nearly free (`{metrics.worktree_fork_ms:.1f} ms` for a
  `{metrics.pgdata_logical_mib:.1f} MiB` seeded PostgreSQL 18 world).
- The **first `up` — seals the read-only base snapshot and boots (composite)** row is not the bare
  `BTRFS_IOC_SNAP_CREATE_V2` cost: the shipped CLI exposes no isolated seal command, so the row
  times the whole first `up` (rootfs materialization, seed restore, `freeze_world` seal, clone,
  service boot).
- Cold worlds pay `L0` rootfs mount + `postgres:18` + app boot + host Chromium launch per test;
  the pre-warmed pool removes that startup cost and yields `{metrics.warm_speedup_factor:.2f}x`.
- Real suites add frontend builds and multi-second fixtures, so the warm-pool gain grows well
  beyond this micro-benchmark's `{metrics.warm_speedup_factor:.1f}x`.

---

## 3. Developer & AI-agent convenience gains

1. **Deletes fragile per-worker process/port plumbing:** every world binds fixed canonical ports
   (`:8000`, `:5432`) in its own `netns`, with zero `xdist` port-0 scraping or bundle re-baking.
2. **Zero `.env` drift:** `snow.yml` `modes.local` + `!! random_string(50)` generates local
   secrets once in the `0600` vault and fails closed on missing external keys.
3. **Zero `root:root` files:** worlds run as host `$UID:$GID` by construction (`ADR-0009` §3).
4. **Zero-confusion OpenCode worktrees:** `@slipfork/opencode` forks the live world on
   `worktree.create` in `{metrics.worktree_fork_ms:.0f} ms` and routes shell commands by `cwd`.
"""


def render_html_projection(metrics: BenchmarkMetrics) -> str:
    primitives = render_html_table(PRIMITIVE_HEADERS, primitive_rows(metrics))
    suites = render_html_table(SUITE_HEADERS, suite_rows(metrics))
    return f"{primitives}\n\n{suites}\n"


# --------------------------------------------------------------------------- read-only check


def _sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def load_baseline(baseline_path: Path) -> dict:
    if not baseline_path.is_file():
        msg = f"committed benchmark baseline '{baseline_path}' is missing"
        raise BenchError(msg)
    try:
        baseline = json.loads(baseline_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as error:
        msg = f"committed benchmark baseline '{baseline_path}' is not JSON: {error}"
        raise BenchError(msg) from error
    for key in ("source_report", "source_report_sha256", "metrics"):
        if key not in baseline:
            msg = f"committed benchmark baseline is missing '{key}'"
            raise BenchError(msg)
    if not isinstance(baseline["metrics"], dict):
        msg = "committed benchmark baseline 'metrics' must be an object"
        raise BenchError(msg)
    for name, spec in baseline["metrics"].items():
        if (
            not isinstance(spec, dict)
            or "value" not in spec
            or "max_factor" not in spec
        ):
            msg = (
                f"committed benchmark baseline metric '{name}' must carry 'value' and "
                f"'max_factor'"
            )
            raise BenchError(msg)
    return baseline


def _metric_bound(name: str, value: float, spec: dict) -> str | None:
    expected = float(spec["value"])
    max_value = expected * float(spec["max_factor"])
    if value > max_value:
        return (
            f"{name}: {value:g} exceeds the committed {expected:g} x "
            f"{spec['max_factor']:g} regression bound ({max_value:g})"
        )
    # A lower bound exists only when the baseline pins one (for example a seeded `PGDATA` size
    # that proves the seed really ran). Faster-than-pinned latency is never a regression.
    min_factor = spec.get("min_factor")
    if min_factor is None:
        return None
    min_value = expected / float(min_factor)
    if value < min_value:
        return (
            f"{name}: {value:g} is below the committed {expected:g} / "
            f"{float(min_factor):g} lower bound ({min_value:g}); the seed or "
            f"measurement may be broken"
        )
    return None


def compare_to_baseline(metrics: BenchmarkMetrics, baseline: dict) -> list[str]:
    """Compare only metrics with a like-for-like committed baseline; never invent numbers."""
    measured = asdict(metrics)
    drift: list[str] = []
    for name, spec in baseline["metrics"].items():
        if name not in measured:
            drift.append(f"baseline metric '{name}' is not measured by this harness")
            continue
        problem = _metric_bound(name, float(measured[name]), spec)
        if problem is not None:
            drift.append(problem)
    for ratio in ("cold_speedup_factor", "warm_speedup_factor"):
        if getattr(metrics, ratio) <= 0:
            drift.append(f"{ratio} must be positive, got {getattr(metrics, ratio):g}")
    return drift


def refresh_baseline(baseline: dict, metrics: BenchmarkMetrics) -> dict:
    """Project the measured record onto the pinned baseline.

    A metric the harness no longer measures is deleted, never kept as a stale pin: a regenerated
    baseline can therefore never leave `--check` comparing against a metric that does not exist.
    """
    measured = asdict(metrics)
    baseline["metrics"] = {
        name: {**spec, "value": measured[name]}
        for name, spec in baseline["metrics"].items()
        if name in measured
    }
    return baseline


def check_report(repo: Path, baseline: dict) -> list[str]:
    """Prove the baseline still describes the committed report; read-only by construction."""
    report = repo / REPORT_PATH.relative_to(REPO_ROOT)
    baseline_file = repo / BASELINE_PATH.relative_to(REPO_ROOT)
    problems: list[str] = []
    if not report.is_file():
        problems.append(f"committed report '{report}' is missing")
    else:
        actual = _sha256(report)
        if actual != baseline["source_report_sha256"]:
            problems.append(
                f"{report} changed (sha256 {actual}) since the benchmark baseline was pinned "
                f"({baseline['source_report_sha256']}); run `make bench-update` and review"
            )
    if not baseline_file.is_file():
        problems.append(f"committed baseline '{baseline_file}' is missing")
    return problems


def render_diff(metrics: BenchmarkMetrics, baseline: dict) -> str:
    measured = asdict(metrics)
    lines = [f"{'metric':32} {'committed':>12} {'measured':>12}  verdict"]
    for name, spec in baseline["metrics"].items():
        value = measured.get(name)
        if value is None:
            rendered, verdict = "-", "not measured"
        else:
            rendered = f"{float(value):.4g}"
            verdict = (
                "ok" if _metric_bound(name, float(value), spec) is None else "DRIFT"
            )
        lines.append(
            f"{name:32} {float(spec['value']):>12.4g} {rendered:>12}  {verdict}"
        )
    return "\n".join(lines)


# --------------------------------------------------------------------------- CLI


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="run_benchmark.py",
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    action = parser.add_mutually_exclusive_group()
    action.add_argument(
        "--check",
        dest="action",
        action="store_const",
        const="check",
        help="read-only (default): re-measure, compare against the committed baseline, exit 1 "
        "on drift, and never write a file",
    )
    action.add_argument(
        "--write-report",
        dest="action",
        action="store_const",
        const="write_report",
        help="the only mutator: write docs/research/m1-poc-benchmark.md and poc/bench-baseline.json",
    )
    parser.set_defaults(action="check")
    parser.add_argument(
        "--json", action="store_true", help="print the measured record as JSON"
    )
    parser.add_argument(
        "--html",
        action="store_true",
        help="print the HTML projection of the same measurement rows",
    )
    parser.add_argument(
        "--run-dir",
        type=Path,
        default=DEFAULT_RUN_DIR,
        help=f"disk-backed run state directory (default: {DEFAULT_RUN_DIR})",
    )
    parser.add_argument("--seed", type=int, default=DEFAULT_SEED)
    parser.add_argument("--num-tests", type=int, default=16)
    parser.add_argument("--workers", type=int, default=8)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    if args.num_tests <= 0:
        print("--num-tests must be positive", file=sys.stderr)
        return 2
    if args.workers <= 0:
        print("--workers must be positive", file=sys.stderr)
        return 2
    try:
        # One absolute run directory: `slipfork doctor` rejects a relative `--state-dir`, and every
        # later CLI call must receive exactly the state directory the gate probed.
        run_dir = args.run_dir.expanduser().resolve()
        metrics = measure_all(args.num_tests, args.workers, args.seed, run_dir)
        if args.html:
            print(render_html_projection(metrics))
        if args.json:
            print(metrics.to_json())
        if args.action == "write_report":
            report_date = datetime.now(UTC).date().isoformat()
            rendered = render_report(
                metrics,
                report_date=report_date,
                host_kernel=platform.release(),
                host_arch=platform.machine(),
            )
            REPORT_PATH.parent.mkdir(parents=True, exist_ok=True)
            REPORT_PATH.write_text(rendered, encoding="utf-8")
            baseline = refresh_baseline(
                json.loads(BASELINE_PATH.read_text(encoding="utf-8")), metrics
            )
            baseline["source_report_sha256"] = _sha256(REPORT_PATH)
            baseline["measured"] = report_date
            BASELINE_PATH.write_text(
                json.dumps(baseline, indent=2, sort_keys=True) + "\n", encoding="utf-8"
            )
            print(
                f"wrote {REPORT_PATH.relative_to(REPO_ROOT)} and {BASELINE_PATH.relative_to(REPO_ROOT)}"
            )
            return 0
        baseline = load_baseline(BASELINE_PATH)
        problems = check_report(REPO_ROOT, baseline)
        problems.extend(compare_to_baseline(metrics, baseline))
        if not args.json:
            print(render_diff(metrics, baseline))
        if problems:
            print("bench-check: DRIFT detected:", file=sys.stderr)
            for problem in problems:
                print(f"  X {problem}", file=sys.stderr)
            return 1
        if not args.json:
            print(
                "bench-check: OK (measured record within every committed bound; read-only)"
            )
        return 0
    except (BenchError, subprocess.CalledProcessError) as error:
        print(f"run_benchmark: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
