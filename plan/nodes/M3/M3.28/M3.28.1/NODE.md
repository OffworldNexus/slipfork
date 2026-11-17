---
id: M3.28.1
title: "DX of record for the 0.1.0 UX pass: bare status, restart, ps, open, fork naming, and the :80 ingress"
kind: task
status: active
owner: architect
priority: 1
depends_on: []
paths:
  - "docs/design/dx-0.1.0-ux.md"
  - "docs/design/target-dx.md"
  - ARCHITECTURE.md
  - "docs/decisions/ADR-0011-*.md"
attempts: 1
max_attempts: 3
escalation: architect
created: "2026-11-17T16:32"
updated: "2026-11-17T16:34"
---

# Goal

Author `docs/design/dx-0.1.0-ux.md`: the dream-tool transcript of record for the `0.1.0` UX pass the owner requested after hands-on testing on `165.232.86.174`, and lock the contract decisions every leaf below implements. Update `ARCHITECTURE.md` (Composition Root + DI tree) and record the ingress/install decision as `ADR-0011`.

# Context

This is the DX blueprint, not code (skill `dx-first` §1.1). Live evidence is in the milestone's Context list and in `plan/nodes/M3/M3.19/RESULT.md` §12.4/§12.9.

Facts the design must respect (verify each before relying on it, do not re-derive from scratch):

- `WorldOrigin::local(&route, ingress_port)` already renders `http://<host>` with **no port** at 80 (`crates/slipfork-world/src/lib.rs:298-312`).
- `LocalHostRoute::parse` strips any numeric `Host:` port (`normalize_host`, `crates/slipfork-world/src/lib.rs:582-594`), so a forwarded non-80 port routes to the same world.
- `UnitSpec` already renders `ListenStream=127.0.0.1:80`, `[::1]:80`, then `127.0.0.1:<forward-port>` (`crates/slipfork/src/main.rs:276-290`, `crates/slipfork-edge/src/unit.rs:121-179`).
- `DaemonStatus` already carries `pid`, `http_ingress_port`, `forward_port`, and every world with one origin (`crates/slipfork-edge/src/daemon.rs:43-102`).
- A browser opener already exists: `trait BrowserOpener` (`crates/slipfork-ospp/src/pairing.rs:301`); do not design a second one.
- The account lookup adapter family is `crates/slipfork/src/user.rs` (`id -un`, `env_clear`); `review-legality` rejects a second ambient identity path.
- `docs/research/tunnel-weaver-and-systemd-facts.md` §5: on the verified host, no unprivileged component (including `systemd --user`) can bind port 80; a **system-level** `.socket` unit is the only sanctioned path, and `slipforkd` then runs with an empty `CapabilityBoundingSet=`.

# Design / Invariants

Write the exact transcripts and verbs for: bare `slipfork`, `status`, `up`, `restart`, `ps`, `open`, `open --remote`, `fork` (no argument), `shell`, `lineage`, `daemon install`, `doctor`, `list`; plus the `:80` ingress story (what systemd binds, what happens without it) and the laptop/remote recipe. The document must contain these verbatim tokens (leaves grep for them):

1. `slipfork status` — bare `slipfork` renders the status dashboard; `slipfork --help` keeps the 25-command list (`tools/check_docs_honesty.py` parses it). Lock: `Commands` becomes optional in `clap`; no-subcommand = status, not help.
2. `slipfork restart` — ONE dev loop: `up` stays idempotent and reports `changed`/`unchanged`; `restart` is the verb that re-reads edits. `up --watch` does not exist in `0.1.0`: no watcher, no polling, no new workspace dependency.
3. `slipfork ps` — the world's supervised tree only (components + `crun` backing services), answered by `slipforkd` from `WorldEngine` supervision records. The CLI never reads `/proc`, `/sys`, or a log directory.
4. `slipfork open --remote` — the exact SSH local-forward + `Host:` recipe, proven against `LocalHostRoute::parse`'s port-insensitivity (a unit test must round-trip the forwarded `Host:`). Local `open` reuses `BrowserOpener`; headless fails closed naming `--remote`.
5. `user.name=slipfork` — `fork` with no argument names the fork from the requesting checkout's current branch (resolved in `slipforkd`, one sanitisation rule to a lowercase DNS label, fail closed with `slipfork fork <name>` on detached HEAD or inside `.worktrees/<fork>`); snapshot commits carry `-c user.name=slipfork -c user.email=slipfork@localhost`, never the developer's git config.
6. `slipfork daemon install` — the one verb that writes `/etc/systemd/system/slipforkd.{socket,service}` + `/etc/apparmor.d/slipfork` (rendered by the existing product renderers, `daemon unit --bin-dir` repeatable), runs `systemctl daemon-reload` and `systemctl enable --now` for both units as `--user <account>` (default: the invoking account), and is idempotent. Non-root fails closed naming `sudo slipfork daemon install --user <account>`. When the ingress is not `:80`, `status`/`up`/`list` print exactly one actionable line — never a silent mystery port.
7. `<state_dir>/run` — ONE canonical runtime directory. Delete the `$XDG_RUNTIME_DIR/slipfork` default from `Layout::resolve` (it is the reason an installed system service and a login-shell CLI disagree and the CLI silently auto-spawns a `:0` daemon); `--runtime-dir` remains the only override. This is planner-locked: if it is genuinely wrong, file a `direction-challenge` instead of designing a second lookup path.
8. `lineage` — **`dag` is deleted from `0.1.0` entirely** (owner ruling): no `slipfork dag plan`, no clap variant, no renderer, no `critical_cli` coverage, no rows in `docs/reference/cli.md` / `docs/testing-dag.md`. **No rename to `slipfork plan`, no alias, no `#[deprecated]`.** The value moves to where the user already is: (a) `slipfork up` prints ONE compact lineage line with real cache/timing facts — `lineage  rootfs <digest12> cached · postgres:18 hit · seed <digest12> rebuilt 2.1s · base <key12> sealed 10ms` — naming exactly the daemon response fields (`M3.28.3` wire shape, `M3.28.4` engine values from boot records + injected clock) that carry them, with a missing fact rendering `-`, never `0` and never an invented value; (b) `ps` and `status` show the running world's lineage digests (rootfs, base key, seed) alongside the component tree; (c) `up -v` renders the per-node detail the deleted command used to show. Planning is parked to `0.2.0`: `slipfork test --dry-run` / a plan view ("31 cached · 9 rebuild across 8 worlds") is its sanctioned home because that is the first moment planning is a decision — record it as a one-line `0.2.0` note, never a `0.1.0` promise. Also name which `slipfork-dag` types keep a real `0.1.0` consumer (`ResourcePlan`/`ResourceStrategy` resolution, `TestSpec`/`Config` parsing) and which become unreachable and are deleted in the same sweep; never delete config-model types the daemon uses. Documentation obligation for `M3.28.14`: `AGENTS.md`'s claim that `0.1.0` ships "the `dag plan` planner" is false and must be fixed there.

Also required, same sweep:

- `ARCHITECTURE.md`: draw the new seams in §1 (daemon `IngressProbe`, `UnitInstaller`, engine `world_processes`) and update §2/§3 trait tables; the DI tree must be drawable with no downcast, no `match kind`, no boolean mode flag, and no new capability bag.
- `ADR-0011`: `slipfork daemon install`, port-80 system activation, the runtime-dir simplification, and why socket activation is the only legal bind path.
- `docs/design/target-dx.md` §1.2/§1.5/§1.6/§1.7 amended in place (one command table, no two specs disagreeing). Published `docs/**` and `README.md` are **not** touched here (zero vaporware; `M3.28.14` publishes after `M3.28.13` verifies).

# Acceptance

```console
$ test -s docs/design/dx-0.1.0-ux.md && echo present
present
$ for t in 'slipfork status' 'slipfork restart' 'slipfork ps' 'slipfork open --remote' 'user.name=slipfork' 'slipfork daemon install' '<state_dir>/run' '127.0.0.1:80' 'lineage'; do grep -qF "$t" docs/design/dx-0.1.0-ux.md || { echo "missing: $t"; exit 1; }; done; echo tokens-ok
tokens-ok
$ test -s docs/decisions/ADR-0011-*.md && grep -qi 'composition root' ARCHITECTURE.md && echo arch-ok
arch-ok
$ ! grep -qE '\[(derived|guessed)\]|TODO|TBD' docs/design/dx-0.1.0-ux.md ARCHITECTURE.md docs/decisions/ADR-0011-*.md && echo no-placeholders
no-placeholders
$ uv run --no-project tools/guard.py
guard: OK
$ uv run --no-project tools/plan.py validate
plan OK
```

- The token loop must print `tokens-ok` (each of the eight locked decisions is present in the spec, not just implied).
- `guard.py` exits 0 (no placeholder markers, no hygiene violations).
- If a decision cannot be locked honestly, do **not** write a placeholder: `uv run --no-project tools/plan.py --agent architect human add --kind direction-challenge --node M3.28.1 "<question + options A/B + recommendation>"` and return `STATUS: blocked`.
