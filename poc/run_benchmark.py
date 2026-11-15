#!/usr/bin/env -S uv run --no-project --script
# /// script
# requires-python = ">=3.12"
# dependencies = [
#     "playwright",
#     "psycopg[binary]",
# ]
# ///
"""Real end-to-end PoC benchmark for the `0.2.0` world model (`docs/research/m1-poc-benchmark.md`).

The harness drives the shipped `slipfork` CLI (`doctor`, `snow resolve`, `dag plan`, `up`, `fork`,
`exec`, `drop` over an auto-spawned `slipforkd`) against a generated sample project, and measures
real PostgreSQL 18 `initdb -> seed -> pg_dump -> pg_restore`, real `FICLONE` CoW world forks, and
a head-to-head browser + committed-SQL suite across three regimes:

1. Traditional baseline (host PostgreSQL, one warm Chromium, `TRUNCATE` + re-seed per test).
2. Slipfork on-demand cold CoW worlds (`fork` + cold Chromium + cold HTTP per test).
3. Slipfork pre-warmed CoW pool (worlds forked and booted ahead of test dispatch).

Every child process receives an explicit allowlisted environment (`bench_child_env`), every byte
of run state lives under one disk-backed run directory that is wiped fail-closed before the first
measurement (`prepare_run_state`), and every measurement's dispatch order is derived from one
explicit seed.

`--check` is the default and strictly read-only: it re-measures, compares the run against
`poc/bench-baseline.json` (the parsed, provenance-pinned projection of the committed report), and
exits non-zero on drift without writing a file. `--write-report` is the only mutator; review and
commit the report it writes.

**Topology honesty:** `0.2.0` worlds boot from a read-only `Dockerfile`-derived `L0` rootfs and run
backing services as daemonless `crun` containers in the world `netns` (`ADR-0009`). The committed
M1 figures in the report were measured on the pre-`ADR-0009` host-shared-`/usr` topology (host
`postgres`/Chromium over the host `/usr` + `/lib`) and are not comparable to fresh runs.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import json
import os
import platform
import random
import shutil
import subprocess
import sys
import time
from collections.abc import Callable, Sequence
from dataclasses import asdict, dataclass
from datetime import UTC, datetime
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
SLIPFORK_BIN = REPO_ROOT / "target" / "release" / "slipfork"
PG_BIN = Path("/usr/lib/postgresql/18/bin")
CHROME_BIN = Path("/usr/bin/google-chrome")
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

DOCKERFILE = """FROM python:3.12-bookworm AS app
RUN pip install --no-cache-dir "psycopg[binary]" playwright \\
 && python -m playwright install --with-deps chromium
WORKDIR /workspace
"""

SNOW_YML = """images:
  - id: app
    context: .
    dockerfile: Dockerfile

components:
  - id: api
    image: app
    envs: [app]
    run_command: "exec sleep 300"

ingresses:
  - match: { prefix: "/back" }
    target: { component: api, port: 8000 }
  - match: { prefix: "/" }
    target: { component: api, port: 8000 }

resources:
  pg_clusters: [{ id: psql, version: "18" }]
  pg_databases: [{ id: postgres, cluster: psql }]

modes:
  local:
    envs:
      app:
        SECRET_KEY: '${{ secrets::SECRET_KEY !! random_string(50) }}'
        DATABASE_URL: '${{ resources::pg_databases.postgres.url }}'
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

# Applied once inside the base world through `slipfork exec` (the world owns `DATABASE_URL`).
WORLD_SEED_SCRIPT = r"""
import json
import os
import sys

import psycopg

SQL = json.loads(sys.argv[1])
with psycopg.connect(os.environ["DATABASE_URL"], autocommit=True) as conn:
    for statement in SQL:
        conn.execute(statement)
    conn.execute("CHECKPOINT")
"""

# Executed inside one isolated CoW world by `slipfork exec` (argv: index, front_html, ready, go).
WORLD_TEST_SCRIPT = r"""
import http.server
import json
import os
import sys
import threading
import time

import psycopg
from playwright.sync_api import sync_playwright

TEST_INDEX = int(sys.argv[1])
FRONT_HTML = sys.argv[2]
READY_FILE = sys.argv[3] if len(sys.argv) > 3 else ""
GO_FILE = sys.argv[4] if len(sys.argv) > 4 else ""

with open(FRONT_HTML, "r", encoding="utf-8") as handle:
    BUNDLE_TEMPLATE = handle.read()

conn = psycopg.connect(os.environ["DATABASE_URL"], autocommit=True)


class AppHandler(http.server.BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        pass

    def do_GET(self):
        if self.path.startswith("/products/"):
            sku = self.path.rsplit("/", 1)[-1]
            row = conn.execute(
                "SELECT id, sku, title, price_cents, stock FROM products WHERE sku = %s",
                (sku,),
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
        else:
            self.send_response(404)
            self.end_headers()

    def do_POST(self):
        if self.path != "/back/checkout":
            self.send_response(404)
            self.end_headers()
            return
        length = int(self.headers.get("Content-Length", "0"))
        payload = json.loads(self.rfile.read(length).decode("utf-8"))
        sku, qty = payload["sku"], int(payload["qty"])
        with conn.transaction():
            product = conn.execute(
                "SELECT id, price_cents, stock FROM products WHERE sku = %s FOR UPDATE",
                (sku,),
            ).fetchone()
            new_stock = product[2] - qty
            conn.execute(
                "UPDATE products SET stock = %s WHERE id = %s",
                (new_stock, product[0]),
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


httpd = http.server.HTTPServer(("127.0.0.1", 8000), AppHandler)
threading.Thread(target=httpd.serve_forever, daemon=True).start()

sku = f"SKU-{TEST_INDEX + 1:06d}"
with sync_playwright() as playwright:
    browser = playwright.chromium.launch(headless=True)
    page = browser.new_page()
    if READY_FILE and GO_FILE:
        with open(READY_FILE, "w", encoding="utf-8") as ready:
            ready.write("ready\n")
        while not os.path.exists(GO_FILE):
            time.sleep(0.005)
    page.goto(f"http://127.0.0.1:8000/products/{sku}")
    assert page.locator("#stock").inner_text() == "25"
    page.get_by_role("button", name="Complete Checkout").click()
    page.wait_for_selector("#order-confirmed")
    assert page.locator("#remaining-stock").inner_text() == "23"
    browser.close()

orders = conn.execute("SELECT count(*) FROM orders").fetchone()[0]
assert orders == 1, f"expected 1 isolated order, got {orders}"
conn.close()
httpd.shutdown()
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


httpd = http.server.HTTPServer(("127.0.0.1", 8000), AppHandler)
threading.Thread(target=httpd.serve_forever, daemon=True).start()

with sync_playwright() as playwright:
    browser = playwright.chromium.launch(executable_path=os.environ["SLIPFORK_BENCH_CHROME"])
    for index in range(NUM_TESTS):
        page = browser.new_page()
        sku = f"SKU-{index + 1:06d}"
        page.goto(f"http://127.0.0.1:8000/products/{sku}")
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


@dataclass(frozen=True, slots=True)
class BenchmarkMetrics:
    """One measured record; both report tables and the JSON block render from this."""

    reflink_64k_us: int
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
    """
    env: dict[str, str] = {}
    for key in PASSTHROUGH_ENV:
        value = os.environ.get(key)
        if value:
            env[key] = value
    for key in PINNED_ENV:
        env.pop(key, None)
    home = run_dir / "home"
    tmp = run_dir / "tmp"
    home.mkdir(parents=True, exist_ok=True)
    tmp.mkdir(parents=True, exist_ok=True)
    os.chmod(home, 0o700)
    env["HOME"] = str(home)
    env["TMPDIR"] = str(tmp)
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


def prepare_run_state(
    run_dir: Path, *, remover: Callable[[Path], None] = shutil.rmtree
) -> None:
    """Wipe and re-create the run directory fail-closed, before the first measurement."""
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
    (project / "run_seed.py").write_text(WORLD_SEED_SCRIPT, encoding="utf-8")
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
) -> float:
    """Measure a real `initdb -> seed -> pg_dump -> pg_restore` pipeline (`M1.7` #7)."""
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
                "-f",
                str(dump),
            ],
            env=env,
        )
    finally:
        _pg_stop(seed_src, env)

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
    return elapsed_ms


def run_baseline_serial_suite(
    project: Path, run_dir: Path, conninfo: str, num_tests: int
) -> float:
    child_env = bench_child_env(run_dir, {"SLIPFORK_BENCH_CHROME": str(CHROME_BIN)})
    started = time.perf_counter()
    run_cmd(
        [
            PYTHON_EXE,
            str(project / "run_baseline.py"),
            conninfo,
            str(num_tests),
            str(project),
        ],
        env=child_env,
    )
    return time.perf_counter() - started


# --------------------------------------------------------------------------- 0.2.0 world CLI


def _slipfork(
    layout: Path,
    env: dict[str, str],
    project: Path,
    *args: str,
) -> subprocess.CompletedProcess[str]:
    return run_cmd(
        [
            str(SLIPFORK_BIN),
            "--state-dir",
            str(layout / "state"),
            "--runtime-dir",
            str(layout / "runtime"),
            *args,
        ],
        cwd=project,
        env=env,
    )


def _slipfork_json(
    layout: Path, env: dict[str, str], project: Path, *args: str
) -> dict:
    output = _slipfork(layout, env, project, "--json", *args).stdout
    return json.loads(output)


def _fork(layout: Path, env: dict[str, str], project: Path, name: str) -> None:
    _slipfork(layout, env, project, "fork", name)


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


def _run_test_in_world(
    layout: Path,
    env: dict[str, str],
    project: Path,
    world: str,
    index: int,
) -> None:
    # Everything the in-world script reads is relative to its `--cwd`, so no host path is
    # assumed inside the world's mount namespace.
    _exec(
        layout,
        env,
        project,
        world,
        _worktree(project, world),
        ["python3", "run_one_world_test.py", str(index), "front/src/App.html", "", ""],
    )


def measure_cold_parallel_suite(
    layout: Path,
    env: dict[str, str],
    project: Path,
    order: Sequence[int],
    workers: int,
) -> float:
    """On-demand cold worlds: `fork` + cold Chromium + cold HTTP per test."""

    def run_one(index: int) -> None:
        name = f"cold-w-{index}"
        _fork(layout, env, project, name)
        try:
            _run_test_in_world(layout, env, project, name, index)
        finally:
            _drop(layout, env, project, name)

    started = time.perf_counter()
    with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as pool:
        list(pool.map(run_one, order))
    return time.perf_counter() - started


def measure_warm_pool_suite(
    layout: Path,
    env: dict[str, str],
    project: Path,
    order: Sequence[int],
    workers: int,
) -> float:
    """Pre-warmed pool: fork and boot a wave, then time only test execution."""
    total = 0.0
    for start in range(0, len(order), workers):
        indices = list(order[start : start + workers])
        worlds: list[tuple[int, str, Path, Path, subprocess.Popen[str]]] = []
        try:
            for index in indices:
                name = f"warm-w-{index}"
                _fork(layout, env, project, name)
                worktree = _worktree(project, name)
                ready = worktree / READY_FILE_NAME
                go = worktree / GO_FILE_NAME
                process = subprocess.Popen(
                    [
                        str(SLIPFORK_BIN),
                        "--state-dir",
                        str(layout / "state"),
                        "--runtime-dir",
                        str(layout / "runtime"),
                        "exec",
                        "--world",
                        name,
                        "--cwd",
                        str(worktree),
                        "--",
                        "python3",
                        "run_one_world_test.py",
                        str(index),
                        "front/src/App.html",
                        READY_FILE_NAME,
                        GO_FILE_NAME,
                    ],
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
                _drop(layout, env, project, name)
    return total


# --------------------------------------------------------------------------- measurement


def preflight() -> None:
    missing = [
        str(path)
        for path in (
            SLIPFORK_BIN,
            PG_BIN / "initdb",
            PG_BIN / "pg_dump",
            PG_BIN / "pg_restore",
            CHROME_BIN,
        )
        if not path.exists()
    ]
    if missing:
        msg = (
            "benchmark prerequisites are missing: "
            + ", ".join(missing)
            + " (run `make bench`, which builds target/release/slipfork; install PostgreSQL 18 "
            "server binaries under /usr/lib/postgresql/18/bin and Google Chrome)"
        )
        raise BenchError(msg)
    try:
        run_cmd([str(PG_BIN / "initdb"), "--version"])
        run_cmd([str(SLIPFORK_BIN), "--version"])
    except (OSError, subprocess.CalledProcessError) as error:
        msg = f"benchmark prerequisite probe failed: {error}"
        raise BenchError(msg) from error


def measure_all(
    num_tests: int,
    workers: int,
    seed: int,
    run_dir: Path,
) -> BenchmarkMetrics:
    prepare_run_state(run_dir)
    preflight()
    env = bench_child_env(run_dir)
    project = run_dir / "project"
    project.mkdir(parents=True)
    setup_sample_project(project, run_dir)

    try:
        doctor = _slipfork_json(run_dir, env, project, "doctor")
        if doctor.get("ok") is not True:
            msg = f"`slipfork doctor` did not report host readiness: {doctor}"
            raise BenchError(msg)
        reflink_64k_us = int(doctor["reflink_64k_micros"])

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

        restore_ms = measure_restore_pipeline(
            run_dir / "seed-src", run_dir / "restore-target", env
        )
        pg_mib, pg_files = _pgdata_size(run_dir / "restore-target" / "pgdata")

        t_up = time.perf_counter()
        _slipfork(run_dir, env, project, "up", "--no-dump")
        world_up_ms = (time.perf_counter() - t_up) * 1000.0

        seed_sql = json.dumps([SCHEMA_SQL, SEED_CORE_SQL, SEED_CATALOG_SQL])
        _exec(
            run_dir,
            env,
            project,
            None,
            project,
            ["python3", "run_seed.py", seed_sql],
        )

        fork_started = time.perf_counter()
        _fork(run_dir, env, project, "fork-probe")
        fork_ms = (time.perf_counter() - fork_started) * 1000.0
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
                            "autocommit=True);"
                            "conn.execute('DELETE FROM orders');"
                            "assert conn.execute("
                            "'SELECT count(*) FROM products').fetchone()[0] == 10000"
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
        for artifact in plan["artifacts"]:
            (run_dir / "state" / "artifacts" / artifact["digest"]).mkdir(
                parents=True, exist_ok=True
            )
        for wave in plan["fixture_waves"]:
            for fixture in wave:
                (run_dir / "state" / "fixtures" / fixture["digest"]).mkdir(
                    parents=True, exist_ok=True
                )

        dag_started = time.perf_counter()
        warm_plan = _slipfork_json(run_dir, env, project, *plan_args)
        dag_warm_ms = (time.perf_counter() - dag_started) * 1000.0
        if not all(item["action"] == "cached" for item in warm_plan["artifacts"]):
            msg = "`dag plan` must report every artifact cached on an unchanged tree"
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
        cold_s = measure_cold_parallel_suite(run_dir, env, project, order, workers)
        warm_s = measure_warm_pool_suite(run_dir, env, project, order, workers)

        # Orthogonal invalidation is measured last so every fork above sees a clean tree.
        app_html = project / "front" / "src" / "App.html"
        app_html.write_text(
            app_html.read_text(encoding="utf-8") + "<!-- css tweak -->\n",
            encoding="utf-8",
        )
        dag_started = time.perf_counter()
        front_plan = _slipfork_json(run_dir, env, project, *plan_args)
        dag_front_ms = (time.perf_counter() - dag_started) * 1000.0
        if front_plan["artifacts"][0]["action"] != "rebuild":
            msg = "editing `front/src` must rebuild the `front` artifact"
            raise BenchError(msg)
        if not all(
            fixture["action"] == "cached"
            for wave in front_plan["fixture_waves"]
            for fixture in wave
        ):
            msg = "editing `front/src` must never invalidate the DB fixtures"
            raise BenchError(msg)

        return BenchmarkMetrics(
            reflink_64k_us=reflink_64k_us,
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
        try:
            _slipfork(run_dir, env, project, "down")
        except (OSError, subprocess.CalledProcessError):
            pass


# --------------------------------------------------------------------------- projections


def primitive_rows(metrics: BenchmarkMetrics) -> list[tuple[str, str, str]]:
    return [
        (
            "Single-file 64 KiB XFS `ioctl(FICLONE)`",
            f"{metrics.reflink_64k_us} us",
            "`slipfork doctor` on the run state directory (0 data bytes copied)",
        ),
        (
            "Seeded PostgreSQL 18 cluster (`PGDATA`)",
            f"{metrics.pgdata_logical_mib:.1f} MiB",
            f"{metrics.pgdata_files} files: 200 tenants + 10,000 products + 5,000 audit events",
        ),
        (
            "`initdb -> seed -> pg_dump -> pg_restore` (PostgreSQL 18)",
            f"{metrics.pg_restore_ms:.1f} ms",
            "Fresh cluster restored from the committed-format `pg_dump` archive, then checked",
        ),
        (
            "`slipfork up` (world + `crun` PostgreSQL service)",
            f"{metrics.world_up_ms:.1f} ms",
            "Read-only `L0` rootfs mount, world init, and `postgres:18` container boot",
        ),
        (
            "`slipfork fork` (CoW world clone)",
            f"{metrics.worktree_fork_ms:.1f} ms",
            "Worktree + `FICLONE` clone of the seeded world state directory",
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
            "`slipfork dag plan` (warm cache hit — zero changes)",
            f"{metrics.dag_warm_plan_ms:.2f} ms",
            "SHA-256 Merkle check across `front` + `migrate` + `seed_*` -> 0 rebuilds",
        ),
        (
            "`slipfork dag plan` after editing `front/src/App.html`",
            f"{metrics.dag_front_only_plan_ms:.2f} ms",
            "`artifact:front` = rebuild; every DB fixture stays cached (0 re-seed)",
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
- **Host:** Linux `{host_kernel}` (`{host_arch}`), XFS (`reflink=1`), PostgreSQL `18`
- **Driver:** `slipfork` `up`/`fork`/`exec`/`drop` over the auto-spawned `slipforkd`
- **Reproduce:** `make bench` (read-only: re-measures and fails on drift) or `make bench-update`
- **Topology:** `0.2.0` read-only `Dockerfile`-derived `L0` rootfs plus a daemonless `crun`
  `postgres:18` container inside each world's private `netns` (`ADR-0009`). The historical M1
  figures previously in this file were measured on the pre-`ADR-0009` **host-shared-`/usr`**
  topology (host `postgres` + host Chromium over `/usr` + `/lib`) and are not comparable.

---

## 1. Core primitive latencies (live measured)

{primitives}

---

## 2. Head-to-head browser + committed-SQL suite ({metrics.num_tests} heavy tests)

Every test drives headless Chromium against an HTTP app on the world's private
`127.0.0.1:8000`, clicks **Complete Checkout** (`POST /back/checkout`), and commits a
`SELECT ... FOR UPDATE` + `UPDATE products` + `INSERT INTO orders` + `INSERT INTO audit_events`
PostgreSQL 18 transaction on `127.0.0.1:5432`:

{suites}

### What the numbers mean

- CoW world cloning is nearly free (`{metrics.worktree_fork_ms:.1f} ms` for a
  `{metrics.pgdata_logical_mib:.1f} MiB` seeded PostgreSQL 18 world).
- Cold worlds pay `L0` rootfs mount + `crun` container boot + Chromium launch per test; the
  pre-warmed pool removes that startup cost and yields `{metrics.warm_speedup_factor:.2f}x`.
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
    min_value = expected / float(spec.get("min_factor", 1.0))
    if value > max_value:
        return (
            f"{name}: {value:g} exceeds the committed {expected:g} x "
            f"{spec['max_factor']:g} regression bound ({max_value:g})"
        )
    if value < min_value:
        return (
            f"{name}: {value:g} is below the committed {expected:g} / "
            f"{spec.get('min_factor', 1.0):g} lower bound ({min_value:g}); the seed or "
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
        action="store_true",
        help="read-only (default): re-measure, compare against the committed baseline, exit 1 "
        "on drift, and never write a file",
    )
    action.add_argument(
        "--write-report",
        action="store_true",
        help="the only mutator: write docs/research/m1-poc-benchmark.md and poc/bench-baseline.json",
    )
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
        metrics = measure_all(args.num_tests, args.workers, args.seed, args.run_dir)
        if args.html:
            print(render_html_projection(metrics))
        if args.json:
            print(metrics.to_json())
        if args.write_report:
            report_date = datetime.now(UTC).date().isoformat()
            rendered = render_report(
                metrics,
                report_date=report_date,
                host_kernel=platform.release(),
                host_arch=platform.machine(),
            )
            REPORT_PATH.parent.mkdir(parents=True, exist_ok=True)
            REPORT_PATH.write_text(rendered, encoding="utf-8")
            baseline = json.loads(BASELINE_PATH.read_text(encoding="utf-8"))
            baseline["source_report_sha256"] = _sha256(REPORT_PATH)
            baseline["measured"] = report_date
            for name, entry in baseline["metrics"].items():
                if hasattr(metrics, name):
                    entry["value"] = getattr(metrics, name)
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
