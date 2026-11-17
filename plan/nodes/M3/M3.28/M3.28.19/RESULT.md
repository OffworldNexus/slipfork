# M3.28.19 — OSPP v1 draft factual-drift audit

Verified `docs/design/ospp.md` against `crates/slipfork-ospp/src/{protocol,client}.rs`, `crates/slipfork/src/daemon.rs`, `crates/slipfork-dag/src/resources.rs`, and `docs/reference/ospp.md`. 168 lines, 2 `sequenceDiagram`s kept; no target behaviour replaced.

## Drift items

| # | Location | Claim found | Verdict |
|---|---|---|---|
| 1 | `docs/design/ospp.md:1-3` | No version statement anywhere before §9; the page read as a shipped wire spec although `docs/reference/ospp.md:9,13` says the client ships but no provider implements it and the wire is unpublished. | **Fixed** — 2-sentence status block naming `0.1.0` vs the `0.2.0` target, linking `../reference/ospp.md`. |
| 2 | `:7` | "OSPP v1 is the clutch that configures the local cloud … `slipforkd` asks the CI-side environment store" — presented as running today. | **Fixed** — "In `0.2.0`, OSPP v1 configures … `slipforkd` resolves …". |
| 3 | `:11` | "synchronous `SecretProvider` and `RemoteResourceOffer` ports" — `RemoteResourceOffer` is a `slipfork-dag` value type (`resources.rs:352`), not a port/trait. | **Fixed** — "`SecretProvider` port and the `RemoteResourceOffer` value type". |
| 4 | `:13` (old) | "Kerfufoo … implement the server side" — present tense implied a running provider. | **Fixed** — "will implement". |
| 5 | `:15` | `wss://<host>:<port>/<mount>` with the mount filled by nothing; §8 item 1 calls it `INPUT NEEDED`, so readers could not tell what exists. | **Fixed** — mount marked §8 item 1 `INPUT NEEDED` and the shipped `0.1.0` paths `/_ospp/v1/connect`, `/_ospp/v1/pair` named. |
| 6 | `:19` | `version: 1` reused for a wire incompatible with `0.1.0`'s `version: 1` frames, with no statement that the two do not interoperate (`protocol.rs:57`, `client.rs:283-290`). | **Fixed** — "`0.1.0`'s unpublished `version: 1` frames are not interoperable with this shape". |
| 7 | `:125` | Capability table's client column stated target `slipforkd` behaviour in plain present tense. | **Fixed** — header now "Target client (`slipforkd`) behaviour". |
| 8 | `:148` | "not exposed by Kerfufoo today; Kerfufoo omits `resources.describe`/`dump.begin`" — an unverified provider fact that contradicts the draft's own `INPUT NEEDED` item 4. | **Fixed** — conditional voice ("if Kerfufoo offers no coordinates …"), pointing at item 4. |
| 9 | `:159` | "an in-tree synchronous reference server … passing the full client positive and adversarial suite" — the testkit exists but speaks the `0.1.0` frames. | **Fixed** — "rewritten to these frames alongside the client". |
| 10 | `:161` (old) | "The code in `crates/slipfork-ospp` predates this specification" left shipped tags unlabeled, and `ADR-0008` §§1–2 still claims to be the single normative wire spec (`protocol.rs:3-5`). | **Fixed** — one sentence: ADR-0008 §§1–2 normative only for the `0.1.0` client (incl. `/_ospp/v1/pair` pairing), superseded by this page at `0.2.0`. |

## Intentional `0.2.0` differences (kept, now version-labeled)

- The drift list is exhaustive for *false current-state claims*; the following are target design and were **not** rewritten:
  - `hello` / `auth_challenge` / `auth_proof` / `auth_ok` / `auth_fail` (shipped: `challenge` / `hello` / `welcome` / `reject`, `protocol.rs:77-88`).
  - `ProjectIdentity` (`remote`/`snow_sha256`/`slug`) instead of `project: &'a str` (`protocol.rs:40-72`).
  - `resolve` / `resolve_ok` / `resolve_err` batching `env.bundle` + `resources.describe` (shipped: separate `env.bundle`, `resources.describe` calls, `client.rs:344-455`).
  - `secrets.ensure_ok` reply tag (shipped: same-tag `secrets.ensure` echo, `protocol.rs:100-106`) and `capability.error.code` (shipped: `kind`, `protocol.rs:132-140`).
  - `deny_unknown_fields` on every control frame is a target MUST; shipped code applies it only to `S3Coordinates`/`PgDumpMetadata` (`resources.rs:25,103`).
- Spot-checked as accurate and left unchanged: 16 KiB / 64 KiB caps (`client.rs:60-63`), loopback-only `ws://` (`client.rs:138-143`), `ospp-v1` subprotocol (`client.rs:42`), 10 s / 30 s deadlines (`client.rs:72,90`), `p256:<66 hex>` key id, transcript `"ospp-v1" || nonce_s || nonce_c || server_name` (`client.rs:214-226`), `tpmd` `sealed` floor with `0600` software fallback (`tpmd.rs:219-243`, `software_key.rs:103-137`), `<state_dir>/dumps/` non-tmpfs (`daemon.rs:742`, `disk_dump.rs:39-48`), the `[resources] <id>: <strategy> -> <target> (<reason>)` row (`main.rs:1118`), `slipfork-ospp-testkit` existence, `<vcs_root>/slipfork.toml` `project` key (`docs/reference/manifest.md:114`).

## Out-of-scope findings (for the owner, not changed here)

- `ADR-0008` still reads `Accepted`; flipping it to `Superseded` is outside this node's paths — the draft now states the supersession itself.
- `docs/design/ospp.md:87` software-key fallback vs the page's zero-fallback negotiation rule is a target design decision, not factual drift.
- Spec-completeness gaps (not drift): §4 frame rows omit `type`; `read_cap_id`/`ResourceBundle`/`MethodProof` undefined; §8 and §10 duplicate the `INPUT NEEDED` list; both diagram directions say `dump.begin`.
- The `[resources]` diagnostic is always printed on `up`/`fork` (except `--json`), not verbose-gated; the draft's log string is correct.

## Gates

```console
$ uvx --offline zensical build --strict        # No issues found
$ uv run --no-project tools/guard.py           # guard: OK (560 files)
$ uv run --no-project tools/plan.py validate   # plan OK
$ grep -c '```mermaid' docs/design/ospp.md     # 2
$ ! grep -rniE 'ships? today|already implemented|is implemented in 0\.1\.0' docs/design/ospp.md   # clean
$ git status --short docs/design/ospp.md       # ?? docs/design/ospp.md
```
