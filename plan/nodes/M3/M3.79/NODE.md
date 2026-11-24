---
id: M3.79
title: "Install and start the per-user daemon automatically (package install is enough)"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths:
  - "crates/slipfork/src/cli/**"
  - "crates/slipfork/src/commands/**"
  - crates/slipfork/src/daemon/client.rs
  - crates/slipfork/src/daemon/handoff.rs
  - "crates/slipfork/src/doctor/**"
  - crates/slipfork/src/install.rs
  - crates/slipfork/src/main.rs
  - crates/slipfork/src/world/mod.rs
  - crates/slipfork/tests/critical_cli_surface.rs
  - crates/slipfork/tests/ansi_matrix.rs
  - crates/slipfork/tests/status_surface.rs
  - crates/slipfork/tests/critical_host_verbs.rs
  - crates/slipfork/tests/critical_daemon.rs
  - "packaging/**"
  - "docs/**"
  - "specs/**"
  - "scripts/**"
  - ".opencode/skills/**"
  - ".opencode/agents/**"
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-24T00:04"
updated: "2026-11-24T00:21"
---

# Problem

A package install ships `/usr/lib/systemd/user/slipfork.service` but nothing enables it: `packaging/deb/postinst` says "The package never enables anything", and `slipfork daemon install` only *writes* units then prints `next: systemctl --user daemon-reload && systemctl --user enable --now slipfork`. So every `slipfork up` finds no socket, auto-spawns an **ephemeral** daemon, and prints `• daemon not installed; running ephemeral — run `slipfork daemon install`` — even when the unit is already installed. Owner report: "I've installed slipfork with the package, the daemon should DEFINITELY be installed automatically".

# Contract

`slipfork up` (and every other daemon-touching verb) must **provision and start the per-user daemon itself** when a systemd user manager is available, then connect. No warning on the normal path. Ephemeral remains only as a real fallback (no systemd user manager, no DBus, containers), with an honest one-line reason.

# Pre-baked findings

- `crates/slipfork/src/daemon/client.rs::connect` (`~line 60`): socket absent -> `spawn_daemon(host)` + the `hint` string -> poll 3 s. This is the only place the warning and the ephemeral spawn live.
- `crates/slipfork/src/compose.rs::host` (`~55-140`): composes `HostEnv`; `runtime_dir`/`config_dir`/`state_dir` are resolved here. Add a resolved capability, e.g. `systemd_user: bool` = `/run/systemd/system` exists AND (`DBUS_SESSION_BUS_ADDRESS` is set OR `<runtime_dir>/systemd/private` exists). Keep the probe in this composition root only.
- `crates/slipfork/src/install.rs`: `USER_UNIT`, `UnitWriter::{from_host,plan,write_all}`, and `install()`. `HostEnv::install_host()` already exists (`crates/slipfork/src/world/mod.rs:~95`). Add e.g. `pub fn start_user(host: &InstallHost) -> Result<(), String>`: write the user unit (`write_all(false)`), then run `systemctl --user daemon-reload` and `systemctl --user enable --now slipfork.service`, returning a one-line failure reason. `--ingress` should likewise run `systemctl daemon-reload` + `systemctl enable --now slipfork-ingress.socket` after writing (root-only path), and `install()` should print `✓ enabled and started` instead of "systemd has not reloaded" + a next line on success.
- `crates/slipfork/src/doctor/probe.rs::daemon` (`~233`): reworded guidance: `up` starts it automatically; `daemon install` is the permanent provisioning path.
- `specs/02-cli-ux.md:230` currently specifies the old "auto-start warns and names `slipfork daemon install`" behaviour; rewrite it. Docs: `docs/install.md` (package section + the daemon paragraph ~115/265), `docs/getting-started.md`, `docs/troubleshooting.md` wherever they say the two-step.
- Tests: `crates/slipfork/tests/critical_daemon.rs`, `critical_host_verbs.rs`, `critical_cli_surface.rs` cover the ephemeral path and the `daemon install` copy; keep them green (CI has no user manager, so they must still take the fallback). Add a unit test for the pure capability predicate and for `start_user`'s argv/unit plan only if it can be done without mocks (no `MockSystemctl`).

# UX

- Interactive: one dim line before provisioning (`• starting the slipfork daemon (systemd --user)`), nothing on success; on failure one `! …` line + `next:` with the exact `systemctl --user status slipfork` probe.
- `--json`/non-TTY: no extra chatter; the daemon still starts.
- `daemon install` stays (explicit, idempotent) and now leaves the daemon **running**; `--ingress` leaves the socket **enabled**.
- Never print `• daemon not installed` when the service actually started.

# Verification

- Live on `165.232.86.174` with a freshly built glibc-2.34 `.deb` (`plan/` has the recipe): as user `slipfork` in `~/demo` (Btrfs), stop/disable the unit first, then `slipfork up` must create/enable/start `slipfork.service`, print no ephemeral warning, and return `✓ Ready`. `systemctl --user is-enabled slipfork` = `enabled`, `is-active` = `active`. Repeat after `systemctl --user stop slipfork`: `slipfork up` restarts it. Do not wipe `/home/slipfork/demo`.
- CI must stay green: fallback path unchanged where no user manager exists.

# Acceptance

- `make check`, `make docs-check`, `guard.py`, `critical_guard.py` green; targeted `critical_daemon`/`critical_host_verbs` green.
- Commit `[M3.79] ...`; `M3.79 -> done`; report the live VM evidence.
