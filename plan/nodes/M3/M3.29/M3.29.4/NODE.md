---
id: M3.29.4
title: "Publish docs/reference/ospp.md (two-tier secrets: local 0600 vault vs memory-only remote) and purge only the false wire/vault claims"
kind: task
status: ready
owner: "doc-writer"
priority: 2
depends_on: [M3.29.3]
paths:
  - docs/reference/ospp.md
  - docs/reference/cli.md
  - docs/reference/manifest.md
  - "docs/snow-modes.md"
  - "docs/getting-started.md"
  - docs/index.md
  - README.md
  - zensical.toml
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-18T00:08"
updated: "2026-11-18T00:30"
---

# Goal

Publish the OSPP v1 surface as user-facing documentation, state the **two-tier secret model** once, and purge the *false* wire/vault claims from the public pages: `docs/reference/ospp.md` becomes a real page because `0.1.0` now implements it, `slipfork secret` stays documented as **local**-vault management, and every mention of remote secrets living in a `0600` file (or of `env.bundle`/`secrets.ensure`) is deleted.

# Context

`docs/design/ospp.md` (`9098b20`) is the normative spec; `M3.29.2`/`M3.29.3` implemented it. Owner clarification (2026-11-18): *"I guess we still need a file vault, for example to store the local postgresql credentials, but definitely not for the remote secrets."* So published docs must **keep** `slipfork secret` (local credentials/overrides that survive restarts) and **delete** only the lies: that a `0600` file ever holds a CI-resolved secret, that `env.bundle`/`secrets.ensure`/`resources.describe`/`dump.begin` exist, and that pairing computes the URL client-side.

# Design / Invariants

1. **Publish** `docs/reference/ospp.md`: a lean page (<= 170 lines) for the user, not the spec — what OSPP is for, the **two-tier** rule in one paragraph (local `0600` vault = your local credentials/overrides, e.g. the local Postgres password that must survive a restart; remote secrets = HPKE-encrypted, decrypted in memory, never written), the pairing quickstart (`slipfork auth` → open the printed URL → approve in the browser), and a compact frame table pointing at `docs/design/ospp.md`. Add it to `zensical.toml` nav next to the CLI reference.
2. **Keep** the `slipfork secret set|unset|list` rows in `docs/reference/cli.md` and `docs/reference/manifest.md`, described as **local** vault management (`0600`, `vaults/<project>.json`), including the "survives a daemon restart" rationale. `!!` in `snow.yml` stays local-vault-backed.
3. **Delete** every claim that a remote/CI secret is stored on disk, and every `env.bundle`/`secrets.ensure`/`resources.describe`/`dump.begin` mention, from `cli.md`, `manifest.md`, `snow-modes.md`, `getting-started.md`, `index.md`, `README.md`.
4. **`!!` story:** local-mode `!!` values live in the local vault; an unpaired/offline run with no vault entry generates an ephemeral **session** secret; CI-provided secrets arrive HPKE-encrypted, are decrypted in memory, and are injected into child processes only.
5. **Pairing** documented as `slipfork auth` + the server-supplied `<url>#handle=<handle>` (fragment-only, single-use), never a URL Slipfork constructs.
6. **One truth per fact:** the frame table lives in `docs/reference/ospp.md` only; other pages link. The two-tier paragraph lives there too, linked from `cli.md`'s `secret` section rather than restated.
7. Zero vaporware: every command and flag documented must exist in `slipfork --help` (`tools/check_docs_honesty.py` enforces this) and every behaviour documented must have been exercised by `M3.28.13`/`M3.29.3`'s tests.

# Acceptance

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
check_docs_honesty: OK
$ uvx --offline zensical build --strict
... exit 0
$ [ "$(wc -l < docs/reference/ospp.md)" -le 170 ] && echo lean
lean
$ make docs-check
... exit 0
```

- `local-vault-documented` and `two-tier-stated` prove the vault survived **scoped** to local credentials; `no-old-wire` proves the superseded frame names are gone. `docs/design/ospp.md` and `docs/decisions/**` are excluded from `no-old-wire` (internal docs may discuss the transition in one sentence).
- Any behaviour not proven by the live/tests evidence is deleted from the page and reported to the lead, not published.
