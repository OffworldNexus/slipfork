# Cruft / SemVer-honesty review — OSPP user-facing docs

Scope: the seven OSPP-touched public pages (`README.md`, `docs/index.md`,
`docs/getting-started.md`, `docs/agents-and-opencode.md`,
`docs/reference/ospp.md`, `docs/reference/cli.md`, `docs/reference/manifest.md`,
`docs/snow-modes.md`). Workspace version is `< 1.0.0`: no compat shims, no
legacy aliases, no dual syntaxes may survive.

Counts: **0 BLOCKER · 0 MAJOR (open) · 3 MINOR · 3 NIT** — plus 7 previously
reported items now `RESOLVED`.

## Open

### MINOR

1. `docs/reference/manifest.md:87` — `domains::<ID>` row says "no provider today —
   fails closed", while `docs/snow-modes.md:119` says "no provider in `0.1.0`";
   the `snow-modes.md` sentence then correctly credits the edge with resolving
   `domains::<id>` itself, so the manifest table reads as if the variable never
   resolves. Two phrases for one fact is exactly the drift this lens rejects.
   Fix: use one canonical phrase in both — pasted verbatim in `manifest.md`:
   `no protocol provider in 0.1.0 — resolved locally by the edge; otherwise fails closed`.
2. `docs/reference/cli.md:59` — the `list` row still says "exactly one **origin
   URL**". `up` (`cli.md:55`), `docs/index.md:12,32` and `README.md:14` now say
   "app URL"; the `list` row is the last user-facing survivor of the internal
   `WorldOrigin` term (cf. `RESOLVED` #5 below, which fixed only the `up` row).
   Fix: `exactly one app URL`.
3. `docs/reference/ospp.md:24` — negative-only phrasing ("**never** written to
   the vault, a dump file, or any other path on disk") is consistent with the
   streamed pipe described in `cli.md`, but a reader who only skims concludes a
   dump *file* exists. Fix: state the positive fact first — "travels only as a
   streamed pipe into `slipforkd` memory" — then keep "never written to the vault
   or any path on disk"; drop the "a dump file" enumeration.

### NIT

4. `docs/reference/ospp.md:10` — `p256:02b1…` abbreviates a hex fingerprint with a
   Unicode ellipsis; unquoted, it invites copy-paste as if it were a real key.
   Fix: `p256:02b1…` → `p256:02b1<…>` (or `<fingerprint>`), matching the
   `<32-byte hex>` placeholder style used two lines above.
5. `docs/reference/cli.md:71` — anchor `#slipfork-secret-local-vault` is linked
   from `docs/reference/ospp.md:71` and must stay pinned to the heading text
   "Local vault & `slipfork secret`". Fix: no wording change; add a
   `<!-- anchor: slipfork-secret-local-vault — pinned, see ospp.md -->` comment
   so the next prose edit does not silently break the cross-page link.
6. `docs/agents-and-opencode.md:35` — same "single **origin URL**" residue as
   finding 2, outside the audited `up`/`list` pair. Fix: "single app URL".

## RESOLVED (previously reported, verified fixed)

- `RESOLVED` — **MAJOR** `docs/reference/ospp.md:6-18` — quickstart hand-started
  `slipforkd --ospp-endpoint` with no explanation, implying the auto-spawn or
  installed unit pairs too. Now explicit at `ospp.md:17-18`: neither the
  auto-spawn nor `slipforkd.service` carries the flag, so `slipfork auth` fails
  closed. Verified: `./target/debug/slipfork daemon unit --only service` carries
  no `--ospp-endpoint`.
- `RESOLVED` — **MAJOR** `docs/reference/cli.md:97,137,140` — `--refresh-dump`
  collapsed two distinct fail-closed reasons. Rows now read "Require a live
  paired session (refused for a local strategy) and seed from a dump", and the
  prose names both `requires an authenticated OSPP session` and the
  local-strategy refusal.
- `RESOLVED` — **MAJOR** local-vault example claimed a "local Postgres password".
  `crates/slipfork/src/daemon.rs` boots Postgres with
  `POSTGRES_HOST_AUTH_METHOD=trust`, and `postgres://127.0.0.1:5432/<id>` carries
  no password; examples now say an app secret / "a password for a database you
  run yourself" (`ospp.md:24`, `cli.md:78`).
- `RESOLVED` — **MINOR** "offered dump" → "requested dump" (`cli.md:97`).
- `RESOLVED` — **MINOR** `up` "origin URL" → "app URL" (`cli.md:55`,
  `docs/index.md:12,32`, `README.md:14`). Residual `list` row tracked as
  finding 2.
- `RESOLVED` — **NIT** `README.md:36` "never written to disk" → "never written to
  disk by Slipfork".
- `RESOLVED` — **NIT** `docs/getting-started.md:126` fork URL now shows the
  port-less `:80` case with `:<port>` conditional.

## Confirmed clean (no cruft)

- `up` output block is exactly `world`/`app`/`worktree`/`next`/`lineage`
  (`restart.rs`); no legacy `origin`/`url` rows anywhere.
- `[resources]` rows appear only under `-v`; `no_remote_session` is the single
  serde label (`resources.rs:276`), so the prose name matches the machine name.
- One vault path and one device-key path documented
  (`$XDG_DATA_HOME/slipfork/vaults/<project>.json`, `device-key-software.json`
  `0600`; `layout.rs:121-165`); no alternate-path fallbacks described.
- Zero old OSPP wire names, dual syntaxes, or "legacy mode" wording remain across
  the seven pages.

VERDICT: CHANGES-REQUIRED (0 BLOCKER, 0 MAJOR open; three MINOR wording/terminology items, three NITs)
