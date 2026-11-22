"""Background worker polling PostgreSQL 18, updating Redis 7, and notifying Mailpit."""

from __future__ import annotations

import datetime as dt
import os
import signal
import smtplib
import sys
import time
from email.message import EmailMessage

from clients import pg_query, redis_cmd

running = True


def _stop(_sig: int, _frame: object) -> None:
    global running
    running = False


signal.signal(signal.SIGTERM, _stop)
signal.signal(signal.SIGINT, _stop)


def notify_mailpit(world: str, count: str) -> None:
    smtp_addr = os.environ.get("SMTP_ADDR")
    if not smtp_addr or redis_cmd("GET", "slipfork:mail_sent"):
        return
    host, port_s = smtp_addr.split(":", 1)
    public_url = os.environ.get("PUBLIC_URL", "")
    cdn_url = os.environ.get("CDN_URL", "")
    msg = EmailMessage()
    msg["Subject"] = f"[Slipfork] World `{world}` is live ({count} releases)"
    msg["From"] = "worker@slipfork.localhost"
    msg["To"] = "dev@slipfork.localhost"
    msg.set_content(
        f"World `{world}` booted cleanly on Btrfs + crun.\n"
        f"URL: {public_url}\n"
        f"CDN: {cdn_url}/badge.svg\n"
    )
    with smtplib.SMTP(host, int(port_s), timeout=3.0) as smtp:
        smtp.send_message(msg)
    redis_cmd("SET", "slipfork:mail_sent", "1")
    print(f"[worker] delivered ready email to Mailpit ({smtp_addr})", flush=True)


def main() -> None:
    world = os.environ.get("SLIPFORK_WORLD", "demo")
    tick = 0
    print(f"[worker] started in world `{world}`", flush=True)
    while running:
        tick += 1
        try:
            rows = pg_query("SELECT COUNT(*) AS count FROM releases;")
            count = rows[0]["count"] if rows else "0"
            stamp = dt.datetime.now(dt.UTC).strftime("%H:%M:%SZ")
            redis_cmd(
                "SET",
                "slipfork:worker:heartbeat",
                f"tick={tick} rows={count} at={stamp}",
            )
            notify_mailpit(world, count or "0")
            if tick == 1 or tick % 6 == 0:
                print(
                    f"[worker] heartbeat tick={tick} world={world} releases={count}",
                    flush=True,
                )
        except Exception as exc:
            print(f"[worker] transient check error: {exc}", file=sys.stderr, flush=True)
        for _ in range(50):
            if not running:
                break
            time.sleep(0.2)
    print(f"[worker] shutting down cleanly in world `{world}`", flush=True)


if __name__ == "__main__":
    main()
