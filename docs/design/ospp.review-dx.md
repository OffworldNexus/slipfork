# DX review — M3.29.4 public secrets docs (`dx` lens)

Scope: `docs/reference/ospp.md` (new), `docs/reference/cli.md`, `docs/reference/manifest.md`, `docs/snow-modes.md`,
`docs/getting-started.md`, `docs/index.md`, `README.md`, `zensical.toml`. Verified against
`crates/slipfork/src/{main.rs,daemon.rs,layout.rs,vaults.rs,restart.rs}`, `crates/slipfork/src/bin/slipforkd.rs`,
`crates/slipfork-edge/src/daemon/server.rs`, `crates/slipfork-ospp/src/*`, `plan/nodes/M3/M3.29/M3.29.3/RESULT.md`.

Judge: what do I run, what happens, where do my secrets live — in 10 seconds, from a fresh checkout.
Positive: no dead links or anchors (all 11 cross-page anchors resolve; `zensical --strict` passed in `LOG.md`),
no false "remote secret on disk" claim, no "ephemeral session secret" claim, frame names all real.
Counts: **1 BLOCKER · 5 MAJOR · 7 MINOR · 2 NIT**

## BLOCKER

**B1. `docs/reference/ospp.md:5-16` — the pairing quickstart cannot be reached from the docs' own default path.**
The page's first step is a hand-started, 4-flag foreground `slipforkd`, while `docs/reference/cli.md:144` says
`slipforkd` "is started for you by `slipfork up` / `slipfork fork`". A fresh user follows
`getting-started.md` §3 (`up` spawns a daemon with no endpoint) and then this page: `slipfork auth` fails closed
(`daemon/ospp.rs`) and the manual replacement refuses to start — `replace_stale_socket` returns `SocketInUse` when
anything still answers `daemon.sock` (`crates/slipfork-edge/src/unix_socket.rs:72-81`). No `slipfork daemon stop`
exists (`cli.rs` `DaemonAction` = `unit|install` only), so the documented flow dead-ends. The page never says the
foreground daemon must stay up for the whole session either.
Fix: (a) add one line under the code block — "this `slipforkd` must stay in the foreground; a daemon started by
`up`/`fork`/`daemon install` has no endpoint and must be stopped before you start this one" — and do **not** print an
invented stop recipe; (b) escalate `plan.py human add --kind direction-challenge`: `--ospp-endpoint` must be reachable
from the auto-spawn path or the rendered systemd unit, or pairing is only possible with a permanently hand-run daemon.

## MAJOR

**M1. `docs/reference/cli.md:78` — `vars::DEBUG` is a capability no documented command has.**
`SecretVault::put` always writes `Namespace::Secrets` (`crates/slipfork/src/vaults.rs:81-100`) and `keys()` only
enumerates secrets (`vaults.rs:106-109`), so `slipfork secret set DEBUG` stores `secrets::DEBUG`, and `secret list`
will not list a `vars::` key. A user following the page renames their variable and still gets "missing required value".
Fix: replace "an override such as `vars::DEBUG`" with `secrets::DEBUG` (or a real `secrets::` key) and drop the claim
that the CLI manages `vars::`; state that `vars::` is read-only today.

**M2. `docs/reference/cli.md:96-98` vs `133-138` — the same three flags are documented twice, with different meanings.**
`--no-dump`/`--refresh-dump`/`--resources local` get a general-options row each and a second, differently worded row each
in the seed-dump table ("fails closed unpaired" vs "refused for a local strategy"). This breaks "one truth per fact"
(`NODE.md` §6) on one page.
Fix: keep the seed table (132-139) as the single home; delete the `--no-dump` and `--resources local` rows at 97-98 and
leave `see [seed dumps](#seed-dumps-and-sealed-bases)`.

**M3. `docs/reference/ospp.md:28-31` — pairing is impossible headless, and the page promises otherwise.**
`slipfork auth` prints the URL only *after* `client.pair()` returns, and it fails closed when the opener fails:
`on_handle` calls `browser.open_pairing_url(page)` and propagates the error (`daemon/ospp.rs`), which is
`xdg-open`/`$BROWSER` (`bin/slipforkd.rs:96-118`). On SSH/CI you get "cannot open the pairing page" and never see a URL.
Fix: add a bullet under `## Pairing`: requires a desktop opener (`BROWSER` or `xdg-open`); the URL is printed after
approval, so headless hosts cannot pair yet. Escalate printing the URL before the blocking wait as a direction-challenge.

**M4. `docs/reference/ospp.md:5-13` — the page's only worked example is not reproducible.**
`STRIPE_SECRET_KEY` appears with no `snow.yml`; nothing says it must be declared in `modes.<mode>.envs` and left
unclaimed so the server resolves it. A reader cannot get `injected` out of that block.
Fix: one sentence after the block — "`STRIPE_SECRET_KEY` is an ordinary `modes.<mode>.envs` entry you do not claim
locally; a claimed key resolves from your vault, an unclaimed one from the provider (`snow-modes.md`, `!!`/`??`)."

**M5. `docs/reference/cli.md:78` — the two-tier paragraph is restated instead of linked, against the node's own rule.**
`NODE.md` §6 says the paragraph lives in `ospp.md` and `cli.md` links it; line 78 restates it in full, and the same
"never written to disk" clause now appears in 8 files (`ospp.md:24`, `cli.md:78/129-131/140`, `manifest.md:90`,
`snow-modes.md:96`, `getting-started.md:108`, `index.md:34-35`, `README.md:36`).
Fix: cut line 78 to "CI-resolved values are the other tier: [two tiers](ospp.md#two-tiers)"; keep the full statement in
`ospp.md` only, and the vault path (`snow-modes.md:96`) as the one place the path is spelled out.

## MINOR

**N1. `zensical.toml:24` vs `docs/reference/ospp.md:1`** — nav says "Secrets & OSPP v1", the H1 says
"OSPP v1 (secret provider)", and the acronym is never expanded on the page. Fix: H1 `# Secrets & OSPP v1`; first
sentence "Open Secret Provider Protocol (OSPP) v1 …"; expand HPKE at `ospp.md:24` ("end-to-end encrypted (HPKE)").

**N2. `docs/reference/ospp.md:10`** — `backend=software:file-0600` is internal jargon inside a copy-paste transcript,
explained only at `:37`. Fix: drop the parenthetical from the transcript; the machine-key bullet already says it.

**N3. `docs/reference/ospp.md:67`** — the quoted error is not the shipped string: `daemon/ospp.rs` is
"start \`slipforkd\` with \`--ospp-endpoint <wss://...>\`". Fix: copy it verbatim (this page quotes errors elsewhere,
so drift is visible).

**N4. `docs/reference/cli.md:3`** — "doctor and `dag plan` read the host locally, `snow resolve` asks `slipforkd` … and
every world command talks to …" omits `secret`, which is also daemon-backed; a user setting a password before first
boot gets "no slipforkd … run `slipfork up`" (`M3.29.3/RESULT.md`). Fix: list `secret` next to `snow resolve` in that
sentence, plus one line in the section: "needs a running `slipforkd`, like `snow resolve`".

**N5. `docs/reference/cli.md:61` and `:81`** — the same `printf '%s' "$PGPASSWORD" | slipfork secret set PG_PASSWORD`
example twice on one page. Fix: keep the table row terse (`slipfork secret set PG_PASSWORD`); the working example
belongs in the section only.

**N6. `docs/getting-started.md:65`** — `$XDG_RUNTIME_DIR/slipfork/daemon.log` does not exist; `Layout::with_roots`
is unconditionally `<state_dir>/run` (`layout.rs:91`), as `cli.md:43` states. Fix: `~/.cache/slipfork/run/daemon.log`
and delete the `XDG_RUNTIME_DIR` parenthetical.

**N7. `docs/reference/cli.md` is 212 lines** against the ≤160-line user-page budget (`dx-first` §2) — this node added
~18 and the reversible duplication in M2 keeps it over. Fix in M2/N5 rather than a rewrite.

## NIT

**T1. `plan/nodes/M3/M3.29/M3.29.4/NODE.md:38`** — invariant 4 still mandates an "ephemeral **session** secret", which
the owner ruling and `LOG.md:29` supersede and the published page correctly omits. A future reviewer will read the page
as non-conforming. Fix: lead rewrites invariant 4 to "`!!` always persists in the local vault".

**T2. `docs/reference/ospp.md:20`** — "Without one, every local path below works unchanged" refers to no local path
listed below. Fix: "every local world still boots unchanged".

VERDICT: CHANGES-REQUIRED
