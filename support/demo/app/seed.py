"""Pre-deploy hook: initializes PostgreSQL 18 schema, seeds S3 buckets, and notifies Mailpit."""

from __future__ import annotations

import json
import os
import smtplib
import time
from email.message import EmailMessage

from clients import pg_query, redis_cmd, s3_request

BADGE_SVG = """<svg xmlns="http://www.w3.org/2000/svg" width="210" height="28" role="img" aria-label="Slipfork CoW: public-read CDN">
  <rect width="92" height="28" rx="6" fill="#0f172a"/>
  <rect x="92" width="118" height="28" rx="6" fill="#06b6d4"/>
  <text x="46" y="18" fill="#e2e8f0" font-family="monospace" font-size="12" text-anchor="middle">s3:assets</text>
  <text x="151" y="18" fill="#082f49" font-family="monospace" font-weight="bold" font-size="12" text-anchor="middle">static-cdn 200</text>
</svg>
"""


def main() -> None:
    world = os.environ.get("SLIPFORK_WORLD", "demo")
    print(f"[seed] migrating PostgreSQL 18 schema for world `{world}`")
    pg_query(
        """
        CREATE TABLE IF NOT EXISTS releases (
            id SERIAL PRIMARY KEY,
            slug TEXT UNIQUE NOT NULL,
            title TEXT NOT NULL,
            source_world TEXT NOT NULL,
            created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        );
        """
    )
    existing = pg_query("SELECT COUNT(*) AS count FROM releases;")
    if int(existing[0]["count"] or "0") == 0:
        pg_query(
            f"""
            INSERT INTO releases (slug, title, source_world) VALUES
              ('btrfs-cow', 'Atomic Btrfs project + database snapshots', '{world}'),
              ('crun-pods', 'Rootless crun pods inside per-world namespaces', '{world}'),
              ('s3-policy', 'Native Btrfs S3 private uploads (403) + public CDN (200)', '{world}')
            ON CONFLICT (slug) DO NOTHING;
            """
        )

    redis_cmd("SETNX", "slipfork:visits", "0")
    redis_cmd("SET", "slipfork:last_boot_world", world)

    for attempt in range(15):
        code_pub, _ = s3_request(
            "PUT",
            "assets",
            "badge.svg",
            BADGE_SVG.encode("utf-8"),
            "image/svg+xml",
            signed=True,
        )
        receipt = json.dumps({"world": world, "visibility": "private-only"}).encode(
            "utf-8"
        )
        code_priv, _ = s3_request(
            "PUT", "uploads", "audit.json", receipt, "application/json", signed=True
        )
        if code_pub in (200, 201) and code_priv in (200, 201):
            print(
                f"[seed] S3 seeded: assets/badge.svg ({code_pub}), uploads/audit.json ({code_priv})"
            )
            break
        time.sleep(0.4)
    else:
        raise RuntimeError(
            f"failed to seed S3 objects: assets={code_pub}, uploads={code_priv}"
        )

    smtp_addr = os.environ.get("SMTP_ADDR")
    if smtp_addr:
        host, port_s = smtp_addr.split(":", 1)
        for _ in range(10):
            try:
                msg = EmailMessage()
                msg["Subject"] = f"[Slipfork] World `{world}` deployed"
                msg["From"] = "deploy@slipfork.localhost"
                msg["To"] = "dev@slipfork.localhost"
                msg.set_content(
                    f"World `{world}` is live at {os.environ.get('PUBLIC_URL', '')}\n"
                    f"CDN: {os.environ.get('CDN_URL', '')}/badge.svg\n"
                )
                with smtplib.SMTP(host, int(port_s), timeout=2.0) as smtp:
                    smtp.send_message(msg)
                print(f"[seed] delivered deploy notification to Mailpit ({smtp_addr})")
                break
            except OSError:
                time.sleep(0.2)


if __name__ == "__main__":
    main()
