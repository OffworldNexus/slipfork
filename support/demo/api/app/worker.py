"""Background worker: BRPOP on the Redis `jobs` list, event rows, 15s heartbeat."""

from __future__ import annotations

import json
import os
import signal
import time

import redis

from app import db
from app.world import world_id

HEARTBEAT_SECONDS = 15
SCHEMA_WAIT_SECONDS = 30.0
STOP = False


def _stop(_signum: int, _frame: object) -> None:
    global STOP
    STOP = True


def wait_for_schema(world: str) -> bool:
    """Bounded wait for the api `pre-deploy` migration; log at most once per boot."""
    deadline = time.monotonic() + SCHEMA_WAIT_SECONDS
    announced = False
    while not STOP and time.monotonic() < deadline:
        try:
            rows = db.query("SELECT to_regclass('public.events') IS NOT NULL AS ready")
            if rows[0]["ready"]:
                return True
        except Exception:
            pass
        if not announced:
            print(
                f"[worker] waiting for `events` in `{world}` (api pre-deploy migrates it)",
                flush=True,
            )
            announced = True
        time.sleep(0.25)
    if announced:
        print(
            f"[worker] `events` still missing after {SCHEMA_WAIT_SECONDS:.0f}s; jobs retry",
            flush=True,
        )
    return False


def handle(client: redis.Redis, world: str, raw: str) -> None:
    try:
        job = json.loads(raw)
    except json.JSONDecodeError:
        job = {"kind": "raw"}
    kind = str(job.get("kind", "job"))
    activity = client.incr("activity")
    db.execute("INSERT INTO events (kind, world) VALUES (%s, %s)", (kind, world))
    print(f"[worker] handled {kind} in `{world}` (activity={activity})", flush=True)


def main() -> None:
    signal.signal(signal.SIGTERM, _stop)
    signal.signal(signal.SIGINT, _stop)
    world = world_id()
    client = redis.Redis.from_url(os.environ["REDIS_URL"], decode_responses=True)
    print(f"[worker] listening on Redis `jobs` for `{world}`", flush=True)
    wait_for_schema(world)
    last_beat = 0.0
    while not STOP:
        if time.monotonic() - last_beat >= HEARTBEAT_SECONDS:
            client.rpush("jobs", json.dumps({"kind": "heartbeat", "world": world}))
            last_beat = time.monotonic()
        popped = client.brpop("jobs", timeout=1)
        if popped is None:
            continue
        try:
            handle(client, world, popped[1])
        except Exception as exc:
            print(f"[worker] job failed: {exc}", flush=True)
    print(f"[worker] stopped cleanly in `{world}`", flush=True)


if __name__ == "__main__":
    main()
