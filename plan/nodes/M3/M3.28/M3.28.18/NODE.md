---
id: M3.28.18
title: "OSPP v1: honest reference page (what ships locally, wire unpublished), nav entry, ADR pointer, cross-links"
kind: task
status: done
owner: "doc-writer"
priority: 3
depends_on: [M3.28.11]
paths:
  - docs/reference/ospp.md
  - zensical.toml
  - "docs/snow-modes.md"
  - docs/reference/manifest.md
  - "docs/decisions/ADR-0008-ospp-v1-websocket-tpmd-resource-strategy-and-world-ux.md"
  - docs/index.md
  - README.md
  - docs/reference/cli.md
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-17T21:24"
updated: "2026-11-17T21:43"
---

# Goal

One honest, human-readable home for `OSPP v1` at `docs/reference/ospp.md` (≤ 60 lines, zero Mermaid, no wire tables), plus the nav entry, the one-line ADR pointer, and cross-links that keep `snow.yml`/`slipfork.toml` syntax owned by `snow-modes.md`/`manifest.md`.

The page states what OSPP v1 is, what ships locally in `0.1.0`, that the wire format is not published, and the two moments a user touches it (`!!`, `slipfork auth login <url>`). It must not present the handshake, capabilities, pairing walkthrough, or seed dumps as shipped behaviour.

# Context

Owner final correction (2026-11-17): there is no shipping OSPP v1 provider integration. The published page must not present the wire as a working feature; the client/daemon path is implemented and tested against Slipfork's loopback reference server, and a real provider is `0.2.0`. `slipfork auth` stays (evidence: `critical_ospp_daemon` 4/4).

Ground truth is the code, not the ADR:

- `crates/slipfork-ospp/src/protocol.rs` — post-`WELCOME` frames: `env.bundle`, `secrets.ensure`, `resources.describe`, `dump.begin`/`dump.end`, `capability.error`.
- `crates/slipfork-ospp/src/client.rs` — `ospp-v1` subprotocol, `/_ospp/v1/connect`, `/_ospp/v1/pair`, `version: 1`, 16 KiB control frames, 64 KiB dump frames, `CHALLENGE -> HELLO -> WELCOME | REJECT`.
- `crates/slipfork-ospp/src/pairing.rs` — `#handle=<64 lowercase hex>` fragment only, `pair_hello`/`pair_approved`/`reject`.
- `crates/slipfork-ospp/src/tpmd.rs` — `TpmdDeviceKeySigner`, namespace `slipfork`, `sealed` floor, sockets `$XDG_RUNTIME_DIR/tpmd/tpmd.sock` then `/run/tpmd/tpmd.sock`.
- `crates/slipfork-ospp/src/secret_provider.rs` — `OsppSecretProvider` reads `env.bundle`, compare-and-set `secrets.ensure`, zero filesystem writes.
- `crates/slipfork-ospp/src/resources.rs` + `crates/slipfork-dag/src/resources.rs` — `s3_buckets`/`pg_databases` offers, `local` / `prefer_remote` / `remote`, `not_offered`, `--no-dump` / `--refresh-dump`.
- `crates/slipfork/src/main.rs` — `auth_login` (URL or `-` from stdin, prints `[auth] paired <key_id> (backend=…)`), `snow resolve`, `up --refresh-dump`.
- `crates/slipfork-snow/src/model.rs` / `provider.rs` — `secrets::`/`vars::`, `!!` ensure, local `0600` vault at `$XDG_DATA_HOME/slipfork/vaults/<project>.json`, layered local-then-remote read with local-only writes.

# Design / Invariants

1. `docs/reference/ospp.md` has exactly four sections, no extra:
   1. What OSPP is — 2 sentences: the secrets/values a local world needs come from the same place CI gets them (a team's cloud-in-a-box); Slipfork owns OSPP v1.
   2. What ships in `0.1.0` — `vars::`/`secrets::`/`!!` resolve locally, `!!` generated once and persisted; the client ships, no provider implements the protocol.
   3. The protocol — one present-tense line: the wire format is not published.
   4. When you'll touch it — `!!` in `snow.yml`; `slipfork auth login <url>` with "no provider ships in `0.1.0`".
2. Zero Mermaid on the page; no handling of the wire, capability frames, pairing walkthrough, `tpmd`, or seed dumps. It must render under `uvx --offline zensical build --strict`.
3. Banned throughout the page: history, "we considered", alternatives, trade-off essays, kernel/AppArmor/TMPFS prose, "target state", plan node ids, ADR references in user prose, changelog voice, hedging ("should", "may eventually"), emoji, bold-shouting, nested bullet pyramids. Every sentence states what exists, what works, or when the reader touches it.
4. `zensical.toml`: one nav entry `{ "OSPP v1 (secret provider)" = "reference/ospp.md" }` under `Reference`. Do not touch the `testing-dag.md` nav row (owned by `M3.28.11`).
5. `docs/decisions/ADR-0008-…md`: add exactly one line immediately under the title: `> User-facing documentation: [OSPP v1](../reference/ospp.md).` The ADR body is unchanged (line 3 of the ADR's `# Decision` may still claim `WorldOrigin::Shared`/tunnel sharing; `M3.28.15` deletes that code, so the new page must not reference it).
6. `docs/snow-modes.md` + `docs/reference/manifest.md` keep `snow.yml`/`slipfork.toml` syntax. Replace duplicated OSPP protocol semantics with a link to `reference/ospp.md`; nothing about pairing, device keys, resource offers, or dumps is restated there. No duplicated fact across the three pages.
7. `docs/index.md`: link the OSPP page from the resolver row/next steps without adding a paragraph. Only if the link fits the existing sentence; skip otherwise.

# Acceptance

Owner correction of 2026-11-17 supersedes the original 9-section / ≥3-diagram / ≤170-line acceptance: the published page states OSPP v1 identity, what ships locally in `0.1.0`, that the wire is unpublished, and the two moments a user touches it. **≤ 60 lines, zero Mermaid, no wire tables, no provider claim, `slipfork auth` one line.**

```console
$ uvx --offline zensical build --strict
... exit 0
$ make docs-check
... exit 0
$ test "$(wc -l < docs/reference/ospp.md)" -le 60
$ ! grep -qE 'mermaid|sequenceDiagram|flowchart' docs/reference/ospp.md
$ grep -qF 'docs/design/ospp.md' docs/reference/ospp.md
$ ! grep -inE 'we (decided|considered)|alternativ|trade-off|target state|ADR-0|M3\.[0-9]|Spike|Option [AB]|eventually|TBD|should' docs/reference/ospp.md
$ ! grep -nE 'env\.bundle|dump\.begin|challenge|hello|tpmd|p256|not_offered|refresh-dump' docs/reference/ospp.md
$ uv run --no-project python tools/check_docs_honesty.py
check_docs_honesty: OK
$ uv run --no-project tools/plan.py validate
plan OK
$ grep -qF 'reference/ospp.md' zensical.toml && grep -qF 'User-facing documentation: [OSPP v1](../reference/ospp.md).' docs/decisions/ADR-0008-ospp-v1-websocket-tpmd-resource-strategy-and-world-ux.md && echo wired
$ ! grep -rnE 'paired OSPP|OSPP.*seed dump|env\.bundle read' README.md docs/index.md docs/snow-modes.md docs/reference/manifest.md && echo no-shipped-provider-claims
$ make check
... exit 0
```

`RESULT.md` must list every code↔ADR mismatch found, the final line count, the diagram count (zero), and the auth evidence verbatim (`cargo test -p slipfork --test critical_ospp_daemon` 4/4 including `paired_ospp_resolves_remote_secrets_restores_a_real_dump_and_forks_rows`; client/daemon tested against the loopback reference server, real provider in `0.2.0`).

# Sequencing

Owner urgency: write `docs/reference/ospp.md` first (no conflict with `M3.28.11`). Nav entry, ADR pointer and cross-links after the page exists. Only `zensical.toml`/`docs/index.md` overlap `M3.28.11`; keep those edits to the minimal added lines.
