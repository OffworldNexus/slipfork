"""Pre-deploy hook: idempotent schema, seed rows and bucket objects."""

from __future__ import annotations

import json
import time

from app import db, s3
from app.world import world_id

BADGE_SVG = """<svg xmlns="http://www.w3.org/2000/svg" width="240" height="28" role="img" aria-label="Slipfork public asset">
  <rect width="100" height="28" rx="6" fill="#0f172a"/>
  <rect x="100" width="140" height="28" rx="6" fill="#06b6d4"/>
  <text x="50" y="18" fill="#e2e8f0" font-family="monospace" font-size="12" text-anchor="middle">s3:assets</text>
  <text x="170" y="18" fill="#082f49" font-family="monospace" font-weight="bold" font-size="12" text-anchor="middle">public-read 200</text>
</svg>
"""


def migrate_sql(world: str) -> None:
    db.execute(
        """
        CREATE TABLE IF NOT EXISTS items (
            id SERIAL PRIMARY KEY,
            title TEXT NOT NULL,
            done BOOLEAN NOT NULL DEFAULT FALSE,
            created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
        """
    )
    db.execute(
        """
        CREATE TABLE IF NOT EXISTS events (
            id SERIAL PRIMARY KEY,
            kind TEXT NOT NULL,
            world TEXT NOT NULL,
            created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
        """
    )
    if int(db.query("SELECT count(*) AS n FROM items")[0]["n"]) == 0:
        db.execute(
            "INSERT INTO items (title, done) VALUES (%s, %s), (%s, %s)",
            (
                f"Fork this world - rows in `{world}` stay put",
                False,
                "Watch the worker counter tick every 15s",
                False,
            ),
        )
    print(f"[migrate] items table ready in world `{world}`", flush=True)


def seed_buckets() -> None:
    audit = json.dumps({"visibility": "private-only"}).encode()
    for attempt in range(20):
        try:
            s3.put("assets", "badge.svg", BADGE_SVG.encode(), "image/svg+xml")
            s3.put("uploads", "audit.json", audit, "application/json")
            print(
                "[migrate] seeded assets/badge.svg and uploads/audit.json", flush=True
            )
            return
        except Exception as exc:
            if attempt == 19:
                raise RuntimeError(f"S3 seeding failed: {exc}") from exc
            time.sleep(0.4)


def main() -> None:
    world = world_id()
    print(f"[migrate] migrating world `{world}`", flush=True)
    migrate_sql(world)
    seed_buckets()


if __name__ == "__main__":
    main()
