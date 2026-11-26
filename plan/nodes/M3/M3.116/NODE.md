---
id: M3.116
title: "Package hooks: ingress setup decoupled from the user-daemon branch and never silent"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - "packaging/deb/**"
  - "packaging/rpm/**"
  - "packaging/arch/**"
  - "packaging/alpine/**"
  - docs/install.md
  - "scripts/check-install.sh"
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-24T16:21"
updated: "2026-11-24T16:30"
---

# Problem

`packaging/deb/postinst` nests the **:80 ingress** setup inside a branch that requires *all* of `/run/systemd/system`, non-root `SUDO_USER`, `systemctl`, `loginctl`, `runuser`, and `id`. If any is missing (GUI/`pkexec` install without `SUDO_USER`, `dpkg -i`, a missing tool), the ingress is skipped with no output; and `systemctl enable --now slipfork-ingress.socket` is `|| true`, so a real failure (port 80 in use, masked unit, missing template) is invisible. Owner: "the ingress should be installed and started by the Debian package, why is it not?"

# Change

- Move the ingress setup out of the user-daemon branch: it only needs the installing user's name/uid (`SUDO_USER`/`SUDO_UID`). Render `/etc/systemd/system/slipfork-ingress@.service` with `User=`/`Environment=XDG_RUNTIME_DIR=`, `systemctl daemon-reload`, then `enable --now slipfork-ingress.socket`.
- Never silence a real failure: check `systemctl is-active` after enabling; on failure print one `!` line + `next: journalctl -u slipfork-ingress.socket` (and `ss -ltn 'sport = :80'` when the port is taken). Do not fail the package transaction.
- If no `SUDO_USER`/`SUDO_UID`, print exactly one hint (install from your user session with sudo, or write the `User=` line and enable the socket) instead of silence.
- Mirror in rpm `%post`, pacman `post_install`, Alpine OpenRC; keep rootless/container installs silent and green.
- `scripts/check-install.sh` still passes as root with no user.

# Acceptance

- Real systemd host/VM: a 0.1.1+ `.deb` installed with `sudo` leaves `slipfork-ingress.socket` **active** and `/etc/systemd/system/slipfork-ingress@.service` with `User=<invoker>`; with port 80 occupied the hook prints one actionable line and exits 0; with no `SUDO_USER` it prints the single hint and stays silent otherwise.
- CI container legs stay green; `make docs-check`.
- Commit `[M3.117] ...`, done, no push.
