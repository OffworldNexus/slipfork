---
id: M3.103
title: make install must not leave the packaged daemon serving old code
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths: [Makefile, docs/install.md]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-24T11:34"
updated: "2026-11-24T11:40"
---

# Problem

`make install` puts a branch CLI in `~/.local/bin`, but an active packaged `slipfork.service` (`ExecStart=/usr/bin/slipfork __daemon`) keeps answering the socket. The user sees 0.1.0 daemon behaviour from a 0.1.1 CLI and internal errors.

# Change

- After installing, if `systemctl --user is-active slipfork` is active and the unit is not the dev binary, **stop it** and print one line: `✓ stopped the packaged slipfork.service — the dev CLI will start the dev daemon` (and `make uninstall` starts it again if the unit exists).
- If systemd is absent, print nothing extra.
- Document the dev-prefix flow in `docs/install.md`: `make install` switches the user daemon to the branch binary; `make uninstall` restores the package service.
- Never touch `/usr/bin`, never rewrite the packaged unit.

# Acceptance

- On the VM or a local systemd user session: with the package service active, `make install` stops it; `slipfork up` spawns the dev daemon (different exe); `make uninstall` restarts the package service.
- `make check`/docs-check green; commit `[M3.103] ...`, done, no push.
