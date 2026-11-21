---
id: M3.40.7
title: "status/ps dynamic tree, doctor rows, and daemon install user+ingress units"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.40.4]
paths:
  - "crates/slipfork/src/**"
  - "crates/slipfork/tests/**"
  - crates/slipfork/Cargo.toml
  - Cargo.toml
  - Cargo.lock
  - Makefile
  - "crates/slipfork-forkfile/**"
  - "crates/slipfork-ingress/**"
  - "crates/slipfork-world/src/boot/**"
  - "specs/02-cli-ux.md"
  - "specs/03-under-the-hood.md"
  - TESTING_PLAN.md
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-21T13:11"
updated: "2026-11-21T14:02"
---

# Goal
Render the `specs/02-cli-ux.md` status surface on top of the daemon control plane shipped by `M3.40.4`: bare `slipfork`/`status` and scoped `ps` as one dynamic-width colored tree, `doctor` with three crisp rows, and `daemon install [--ingress]` writing the user unit plus the system `:80` socket/AppArmor profile.

# Context
- `M3.40.4` ships `<runtime_dir>/daemon.sock` (0600, NDJSON `specs/03` §5), the `ps` method returning every world/fork with services, ports, routes, digests, and ages, and the ingress accept path.
- Renderer home: `crates/slipfork/src/render.rs` (`paint`) plus a new `crates/slipfork/src/status/` module. Compute visible widths after stripping ANSI (`max()`), never `{:<16}`/`{:<24}` shears.
- Tree contract (`specs/02` §1): `● demo  http://demo.localhost  ~/projects/demo`, service rows `api running :8000 -> /api 12m`, resources `pg postgres 18 :5432`, `uploads s3 (private)`, `assets s3 + cdn -> static_cdn.*`, nested `forks` with `● feat-auth http://feat-auth--demo.localhost`; `○ No worlds running` on empty state with exit 0.
- Color: `running` green, `failed`/`crashed` red, primary URLs bold cyan, 12-char digests and ages dim, `:80` omitted on port-80 URLs. Zero `\x1b[` under `NO_COLOR`, `TERM=dumb`, piped stdout, or `--json`.
- `doctor`: exactly three rows — Btrfs (`statfs` super-magic via `slipfork_world` probe on the project/cache path), Namespaces (`user.max_user_namespaces`/userns probe), Runtime (`crun` on injected PATH and whether the `:80` ingress socket is active); lead with `✓ Ready` / `✗ N issue(s)` and one `next:` per failing row.
- `daemon install`: embed the systemd user unit and (for `--ingress`) the system socket unit + `/etc/apparmor.d/slipfork` profile as constants in `crates/slipfork/src/daemon/install.rs`; `packaging/` is outside this epic. `daemon install` needs no sudo; `--ingress` requires sudo and must print the exact `sudo slipfork daemon install --ingress` command instead of failing opaquely.
- `--json` for `status`/`ps`/`doctor` emits the same fields with zero ANSI.

# Acceptance
- `cargo test -p slipfork`: a real `XDG_RUNTIME_DIR` under `/var/tmp` journey asserts the tree widths are dynamic (a 40-char fork name does not shear), the empty state prints `○ No worlds running` and exits 0, `NO_COLOR=1`/`TERM=dumb`/piped stdout emit zero `\x1b[`, and `--json` parses.
- `doctor` on this host prints exactly three rows and exits 1 with `✗ N issue(s)` when a probe fails, 0 with `✓ Ready` otherwise.
- `daemon install` writes the user unit into the injected path and `--ingress` writes the socket unit + AppArmor profile, both asserted byte-for-byte in a temp `HOME`/`/var/tmp`.
- `cargo clippy -p slipfork --all-targets -- -D warnings`; `uv run --no-project tools/critical_guard.py`; `uv run --no-project tools/guard.py`.
