---
id: M3.29.1
title: "Ripple the OSPP v1 spec through ARCHITECTURE, ADR-0001/0008, VISION and the DX specs; delete the obsolete wire and disk vaults"
kind: task
status: done
owner: architect
priority: 1
depends_on: []
paths:
  - ARCHITECTURE.md
  - "docs/decisions/ADR-0001-*.md"
  - "docs/decisions/ADR-0005-*.md"
  - "docs/decisions/ADR-0008-*.md"
  - "docs/decisions/ADR-0009-*.md"
  - "docs/decisions/ADR-0011-*.md"
  - docs/VISION.md
  - "docs/design/dx-0.1.0-ux.md"
  - "docs/design/target-dx.md"
  - "docs/design/world-base-image.md"
attempts: 1
max_attempts: 3
escalation: architect
created: "2026-11-18T00:08"
updated: "2026-11-18T01:26"
---

# Goal

Ripple `docs/design/ospp.md` (commit `9098b20`, **sole normative** OSPP v1 spec for `0.1.0`) across every design and decision document in the repo, and delete every obsolete wire shape, disk-vault story and `slipfork secret` command from the internal specs — before any code is rewritten.

# Context

`docs/design/ospp.md` is the input; this node never edits it. The obsolete material it supersedes:

- `ARCHITECTURE.md` §5–§7 (OSPP wire, `env.bundle`/`secrets.ensure`/`resources.describe`/`dump.begin`, `0600` vault, `tpmd` pairing, `capabilities[]`, `challenge`-first v0 draft).
- `ADR-0001` (OSPP as the CI clutch) and `ADR-0008` (secret provider + resource dumps) — their wire/vault sections must read as superseded by the new spec, with the *decision* (CI holds authority; Slipfork never stores secrets) preserved.
- `docs/VISION.md` (secret story), `docs/design/dx-0.1.0-ux.md` (§`secret`, §auth, §dump, the `--json` schemas), `docs/design/target-dx.md` (§1.4 service clients, pairing, resource dumps).

Owner directive: *"ripple out the shit of this spec into everything else that needs to be updated"*.

# Design / Invariants

1. **One normative source.** Where any doc describes the OSPP wire, it either quotes `docs/design/ospp.md` verbatim or links to it. No second wire table, no second frame list, no "v0 draft" residue.
2. **Delete, do not annotate.** `env.bundle`, `secrets.ensure`, `resources.describe`, `dump.begin`, `tpmd`-on-the-wire, `capabilities[]`, `challenge`-first handshake: removed. If history matters, it belongs in `ADR-0008`'s "superseded" paragraph, in one sentence, not in the architecture.
3. **Zero disk secrets is the contract.** Every mention of `FileVaultProvider`, `vaults/<project>.json`, `0600` secret files, `slipfork secret set|unset|list` is deleted from specs; the replacement is in-memory HPKE-decrypted env + in-memory session secrets for local-only runs.
4. **`claims` is ID-only and uniform** across `vars`, `secrets`, `resources.<kind>`, `domains`, `components`; a claimed entry is never resolved and its `${{ … }}` expression survives as text.
5. **Project identity on the wire is the verbatim Git `remote` string** — no slug, hash, mode tuple, or canonicalization on the wire (the local `ProjectSlug` stays a local concept; say so once).
6. **Live subscription semantics** must be unambiguous: the socket stays open while worlds run; `env_updated` restarts affected services; re-`subscribe` with the same `id` on config change; `unsubscribe` on stop.
7. **Dumps are out-of-band and encrypted**: `dump_request`/`dump_ready` + HTTPS `GET`, stateful HPKE (`enc` header, length-prefixed sealed frames, `len = 0` EOF), decrypted on the fly into `zstd -d | pg_restore`, zero plaintext at rest.
8. `ARCHITECTURE.md` must draw the DI consequences: the OSPP client is an adapter behind a capability trait; the encryption keypair is per-subscription, in memory; no new global state; the daemon owns the subscription lifecycle; the CLI never touches secrets.

# Acceptance

```console
$ ! grep -rn 'env\.bundle\|secrets\.ensure\|resources\.describe\|dump\.begin\|capabilities\[\]' ARCHITECTURE.md docs/decisions docs/design/*.md && echo old-wire-gone
old-wire-gone
$ grep -q 'never.*disk' ARCHITECTURE.md && grep -q 'local.*vault' ARCHITECTURE.md && echo two-tier-boundary
two-tier-boundary
$ grep -rq 'ospp-v1-secret' ARCHITECTURE.md && grep -rq 'ospp-v1-dump' ARCHITECTURE.md && echo hpke-documented
hpke-documented
$ grep -qE 'remote.*verbatim|verbatim.*remote' ARCHITECTURE.md && echo remote-verbatim
remote-verbatim
$ uv run --no-project tools/guard.py && uv run --no-project tools/plan.py validate
guard: OK
plan OK
$ uvx --offline zensical build --strict
... exit 0
```

- `old-wire-gone`, `disk-vault-gone`, `hpke-documented` and `remote-verbatim` are the four proofs; `S=`/`ephemeral` key handling must be described as in-memory only.
- `docs/design/ospp.md` is byte-identical to `9098b20` (verify with `git diff --exit-code 9098b20 -- docs/design/ospp.md`).
- If the spec is genuinely ambiguous on a point the architecture must state, file `plan.py human add --kind direction-challenge` and return `STATUS: blocked` — never invent a wire field.
