"""Real SMTP delivery to the Mailpit service (`SMTP_ADDR`)."""

from __future__ import annotations

import os
import smtplib
from email.message import EmailMessage

FROM = "demo@slipfork.localhost"
TO = "you@slipfork.localhost"


class MailDisabled(RuntimeError):
    """Raised when the `mail` feature is off and `SMTP_ADDR` is unset."""


def send_test(world: str, public_url: str) -> list[str]:
    addr = os.environ.get("SMTP_ADDR")
    if not addr:
        raise MailDisabled(
            "mail feature is off: set SMTP_ADDR (run with the `mail` feature)"
        )
    host, _, port = addr.rpartition(":")
    msg = EmailMessage()
    msg["From"] = FROM
    msg["To"] = TO
    msg["Subject"] = f"Slipfork demo - {world} is live"
    msg.set_content(
        f"World `{world}` served this message through real SMTP.\n"
        f"Open {public_url} to see it.\n"
    )
    msg.add_alternative(
        f"""<html><body style="font-family:system-ui,sans-serif">
<h2>World <code>{world}</code> is live</h2>
<p>This message travelled over SMTP into Mailpit, not a mock inbox.</p>
<p><a href="{public_url}">{public_url}</a></p>
</body></html>""",
        subtype="html",
    )
    with smtplib.SMTP(host, int(port), timeout=5.0) as smtp:
        smtp.send_message(msg)
    return [TO]
