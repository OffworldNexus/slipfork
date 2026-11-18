# M3.29.4 — public secrets docs (two-tier vault vs memory-only remote)

Base `4a1428f` · branch `release/0.1.0` · docs landed as `d24a6c5` by the lead; `README.md`,
`zensical.toml`, and this `RESULT.md` were still uncommitted when the gate ran.

## What landed

- **`docs/reference/ospp.md` rewritten (78 lines, was an 18-line stub with false claims).** What OSPP is
  for, a runnable pairing/boot example, the **two tiers** paragraph (local `0600`
  `vaults/<project>.json` = `secrets::` overrides + local `!!`; remote secrets = HPKE-encrypted,
  decrypted in `slipforkd` memory, injected into children, never persisted), the pairing rules
  (server-supplied `<url>#handle=…`, fragment-only/single-use, in-memory pairing, one server
  identity, `0600` device key), local `claims`, an 11-row frame table pointing at the normative
  `docs/design/ospp.md`, and byte-exact failure strings with next commands.
- **`cli.md`** — new `## \`slipfork secret\` (local vault)` section (`0600` path, survives-restart
  rationale, `secrets::`-only writes, needs a running `slipforkd`), `auth` row now names the
  server-supplied URL, seed-dump section rewritten (streamed `zstd -d | pg_restore`, no
  `.pgdump`/`.pgcustom`/cached dump; one flag table instead of two divergent ones), global
  `-v, --verbose` documented, `slipforkd --ospp-endpoint` documented, `origin URL` → `app URL`.
- **`manifest.md` / `snow-modes.md` / `getting-started.md` / `index.md` / `README.md`** — vault is
  local-only with links to `ospp.md#two-tiers`; `!!` survives restarts; `auth login` → `auth`;
  stale `[resources] not_offered` / `[ready]` / `[origin]` transcripts replaced with the shipped
  `world`/`app`/`worktree`/`next`/`lineage` shape (`-v` gates `[resources]`, reason
  `no_remote_session`); `XDG_RUNTIME_DIR` daemon-log path corrected.
- **`zensical.toml`** — nav entry relabelled "Secrets & OSPP v1", beside the CLI reference.

## Acceptance

```console
$ test -s docs/reference/ospp.md && echo ospp-page
ospp-page
$ grep -q 'slipfork secret' docs/reference/cli.md && echo local-vault-documented
local-vault-documented
$ grep -q '0600' docs/reference/ospp.md && echo two-tier-stated
two-tier-stated
$ ! grep -rn 'env\.bundle\|secrets\.ensure\|resources\.describe\|dump\.begin\|capabilities\[\]' docs/ README.md && echo no-old-wire
no-old-wire
$ uv run --no-project tools/check_docs_honesty.py
check_docs_honesty: OK (11 doc(s) match real CLI subcommands: published pages plus support/demo/README.md)
$ uvx --offline zensical build --strict
No issues found
$ [ "$(wc -l < docs/reference/ospp.md)" -le 170 ] && echo lean
lean
$ make docs-check
No issues found · check_docs_honesty: OK   (exit 0)
```

Grep of the seven pages: no `env.bundle`/`secrets.ensure`/`resources.describe`/`dump.begin`/
`capabilities[]`, no `not_offered`, no `auth login`, no `.pgcustom`/`.pgdump`, no
"no provider implements it".

## Critic gate

`docs/design/ospp.review-{dx,ai-ergonomics,cruft}.md` — 0 open BLOCKER, 0 open MAJOR.
Resolved: unreachable pairing quickstart (ordering + foreground + socket-steal refusal now stated),
headless pairing truth (URL prints only after approval; desktop host required; a human must
approve), `vars::` is not CLI-writable (`secret` writes `secrets::` only), the `trust`-auth
Postgres password example, `-v` undocumented, duplicate dump-flag tables, non-verbatim errors.
Open MINOR/NIT: `domains::` provider wording drift between `manifest.md`/`snow-modes.md` (now
aligned to "no provider in `0.1.0`"), `p256:02b1…` ellipsis, `cli.md` 212 lines vs the ≤160
guideline (was 198 before this node; reference pages only).

## Deviations / reported to lead

1. **No "ephemeral session secret" published.** NODE invariant 4 and the parent M3.29 plan say an
   unpaired local run generates an in-memory session secret; the shipped resolver always
   `ensure_secret`s into the local vault (`crates/slipfork-snow/src/faceless.rs:230`,
   `crates/slipfork/src/daemon.rs:4091`). The page documents persistence instead. Invariant 4
   should be rewritten by the lead.
2. **`--ospp-endpoint` is unreachable from the default path** (direction-challenge): `up`/`fork`
   auto-spawn and `daemon unit`/`install` render no endpoint, and there is no `daemon stop`, so
   pairing requires a hand-run foreground `slipforkd` and cannot survive a reboot. Documented as
   the constraint it is; filing the human request belongs to the lead.
3. `slipfork open`/`ps`/`restart` exist in the tree now (M3.28.8/.9) but are not documented here —
   they belong to those nodes.
