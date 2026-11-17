# M3.28.18 — RESULT

## Deliverables

- `docs/reference/ospp.md` (new): **18 lines**, **0 diagrams**. Four sections — what OSPP v1 is; what ships in `0.1.0`; the draft wire spec pointer (`docs/design/ospp.md`, inline code) with “describes only what ships”; the two moments a user will touch it. No Mermaid, no handshake/capability/refusal/failure tables.
- `zensical.toml`: one `Reference` nav row `{ "OSPP v1 (secret provider)" = "reference/ospp.md" }` after the CLI entry. The `testing-dag.md` row is untouched.
- `docs/decisions/ADR-0008-…md`: one line under the H1, `> User-facing documentation: [OSPP v1](../reference/ospp.md).` Body unchanged.
- `docs/snow-modes.md`: `snow.yml` syntax only. The `!!` ensure row states generation + local persistence with no provider clause. The read subsection restores the vault fact (`$XDG_DATA_HOME/slipfork/vaults/<project>.json`, mode `0600`, `--state-dir`/`--runtime-dir` do not move it) and points remote providers at `ospp.md` without a provider claim.
- `docs/reference/manifest.md`: syntax only. `!!` persistence links `snow-modes.md`; the `[resources.<kind>.<id>]` rule keeps the three strategies and states that `remote` needs a provider and fails closed without one, with seed dumps and `--no-dump`/`--refresh-dump` pointing at `cli.md#seed-dumps-and-sealed-bases`.
- `docs/index.md`: Backing-services row no longer mentions OSPP; resolver row says the vault is local and links the new page.
- `README.md` (same sweep): Backing-services bullet drops the seed-dump restoration; the resolver/DAG bullet drops the paired `env.bundle` read and keeps the local `0600` vault.
- `docs/reference/cli.md` (same sweep): one sentence appended to the seed-dumps section — “No OSPP v1 provider ships in `0.1.0`; this flow requires a compatible provider.” The conditional prose and the `auth login` row are unchanged.

## Owner corrections applied

1. **The first cut was wrong in kind.** A 135-line page with three `sequenceDiagram`s presented OSPP as a working wire feature. OSPP v1 has no shipping provider integration, so the page now states the protocol's identity and scope only: 18 lines, 0 diagrams, no Mermaid, no `sequenceDiagram`.
2. **The version stays.** It is **OSPP v1**; `version: 1` is what the negotiation carries and there is no earlier generation.
3. **Client/provider split stated in one sentence** (round-2 IoC delta): “Nothing in `0.1.0` requires a provider: the OSPP v1 client ships, but no provider implements the protocol yet.” This is the owner-accepted split, not a claim that remote reads work.
4. **`slipfork auth login <url>` stays, one bullet.** It is the real command path. The page gives it no walkthrough and no diagram. `docs/reference/cli.md`'s `auth login` row is untouched.
5. **One home for the “no provider” claim.** It appears only in the page's two `0.1.0` sentences and, per the owner's instruction, once at the end of `cli.md`'s seed-dumps section. `snow-modes.md`, `manifest.md`, `index.md`, and `README.md` no longer carry it.
6. **`docs/design/ospp.md` is named in inline code, never linked.** The file now exists as the architect's `0.2.0` draft (landed by `M3.28.19`); `design/` is excluded from the published site, so a markdown link would fail `invalid_links` and add a nav-dead page. The page therefore carries the pointer as code only.
7. **Kerfufoo is not named on the page.** It is recorded here only, as the provider implementer.
8. **Final micro-pass and functional-gate fix.** §The protocol carries the owner-mandated draft pointer — “The wire specification is being written in `docs/design/ospp.md`; no provider implements it yet, so this page describes only what ships.” — and ends there; the earlier conditional “A paired session would supply remote secrets and seed dumps.” line was deleted because it re-introduced an unshipped dump capability. `snow-modes.md` says “Remote provider protocol” and `manifest.md` says the `remote` strategy “fails closed without a provider”. The `auth login` bullet is unchanged. `NODE.md` acceptance now asserts the pointer positively (`grep -qF 'docs/design/ospp.md'`) instead of forbidding it.

## Auth evidence (owner-verified, retained in RESULT, not on the page)

`cargo test -p slipfork --test critical_ospp_daemon` → 4/4 passing, including `paired_ospp_resolves_remote_secrets_restores_a_real_dump_and_forks_rows` (drives `slipfork auth login -` end-to-end against the loopback reference OSPP server). Split: the client/daemon path is implemented and tested against our reference server; a real provider (Kerfufoo) is `0.2.0`.

## Code ↔ brief ↔ ADR mismatches found (code wins; recorded, not published)

1. **`[auth]` success line.** The brief's `(backend=tpmd)` never prints: the label is `tpmd:sealed`/`tpmd:hardware`/`tpmd:file`/`tpmd:keyring`/`software:file-0600` (`crates/slipfork-ospp/src/tpmd.rs::label_for`, `software_key.rs:136`), and ` account=<account>` is always appended (`crates/slipfork/src/main.rs:980-988`, `daemon.rs`).
2. **`[secrets] ensured …` line does not exist.** No `-v` flag exists, and `BootEvent::Secrets` is rendered by `main.rs:1110-1115` but never emitted by `slipforkd` (emission sites are `Resources`, `Ready`, `Hook`, `HookOutput`, `HookFailed`). Code gap.
3. **`slipfork secret set secrets::KEY` would store a key literally named `secrets::KEY`** (`daemon.rs::validate_secret_key` accepts `::`); the working form is `printf '%s' "$VALUE" | slipfork secret set KEY`.
4. **Failure-string quoting.** The brief quotes commands with single quotes; the binary uses backticks (`main.rs:1198`, `daemon.rs:1267`). These strings are no longer published.
5. **ADR-0008 §2 “`env.bundle` `domains` applies strictly to `cloud` mode”** has no code guard: `EnvBundle` and `OsppSecretProvider::get` serve `Namespace::Domains` for any mode (`protocol.rs`, `secret_provider.rs:66-72`).
6. **ADR-0008 §1 “5-minute server TTL” on pairing handles** is unverifiable client-side — `pairing.rs` has no TTL or expiry field, and the server is out of tree.
7. **`WorldOrigin::Shared` / tunnel sharing mismatch not reproduced.** At review time `WorldOrigin::Shared` still exists (`crates/slipfork-world/src/lib.rs:366`) and the ADR's “`share` fails closed” wording matches the code. The deletion expected from `M3.28.15` had not landed.
8. **`p256:<66 hex>` — no mismatch.** `device_key.rs:33` documents and enforces `p256:<66 lowercase hex>`, matching the ADR.
9. **`resources::pg_databases.<id>.url` is never remote.** `ResourceCoordinates` always renders `postgres://127.0.0.1:5432/<id>` (`crates/slipfork-snow/src/model.rs`); only S3 overrides are remote, and a PostgreSQL offer is a seed dump, not coordinates.
10. **`slipfork snow resolve` uses the layered vault**, not just the local vault: `resolve_snow` → `vault_for(project)` builds the same `LayeredSecretProvider` (local first, paired `env.bundle` second), while `resources::` coordinates stay canonical-world local. A down paired host maps to “unpaired” (`Err(OsppError::Transport) => Ok(None)`).

## Round-2 gate (legality + cruft) verdicts

- **Legality:** the page no longer advertises a wire nobody implements, no longer links or names a non-existent design doc, and the “no provider” fact has one home per surface. The `auth login` line matches the shipped command and the tested daemon path.
- **Cruft:** the six duplicated “no remote provider ships” statements are deleted (`snow-modes.md` row + read subsection, `manifest.md` ×2, `index.md`, `README.md` ×2); `README.md` and `cli.md` were swept in the same change so no page still claims a paired seed-dump restore or a paired `env.bundle` read; `snow-modes.md` keeps the vault path fact its read subsection depends on.
- **IoC:** client and provider are named as separate roles; no page implies the client is the integration.

## Acceptance

| Command | Result |
|---|---|
| `uvx --offline zensical build --strict` | PASS (exit 0, “No issues found”) |
| `make docs-check` | PASS (exit 0) |
| `python tools/check_docs_honesty.py` | PASS (`check_docs_honesty: OK`, 10 published docs) |
| `tools/plan.py validate` | PASS (`plan OK`) |
| `tools/guard.py` | PASS |
| banned-pattern grep | no match |
| `! grep -n 'mermaid\|sequenceDiagram\|docs/design/ospp' docs/reference/ospp.md` | no match |
| `wc -l docs/reference/ospp.md` | 18 (≤ 60) |
| `grep -qF 'docs/design/ospp.md' docs/reference/ospp.md` | match (draft pointer present, inline code only) |
| nav + ADR pointer grep | wired |
| `make check` | blocked by environment only: `bun` is absent (`ts-test` 127). Every other target passes: `plan-check`, `guard`, `py-lint`, `py-test`, `support-check`, `rs-fmt`, `rs-lint`, `rs-test` (exit 0, full workspace), `docs-check`. |

## Follow-ups (outside this node's path lock)

- `docs/getting-started.md:119` still explains `not_offered` in terms of a paired OSPP session (“no paired OSPP session offered a remote database”); outside this node's paths, same sweep needed.
- `support/integrations.toml`: `ospp-v1-secrets-and-dumps.recipe_page` points at `docs/snow-modes.md`; retarget to `docs/reference/ospp.md` and run `make support-sync`.
- Emit `BootEvent::Secrets` from the ensure path, or delete the unused event and its renderer.
- Publish the OSPP v1 wire spec when a provider integration lands.
- `slipforkd` never calls the remote `secrets.ensure`: `LayeredSecretProvider::ensure_secret` only calls its primary.

## Lead verification (2026-11-17, pre-commit)

- `cargo`/`bun`/`uv` are installed but **not on the default `PATH`**; with `$HOME/.cargo/bin` and `$HOME/.bun/bin` added, `make plan-check guard py-lint py-test support-check ts-test rs-fmt rs-lint docs-check` exits 0 and the full workspace `rs-test` log ends green (`/tmp/opencode/rs-test2.log`). The "bun absent" note above is a PATH artifact, not an environment gap.
- `critical_guard.py --base ae71c45 --node M3.28.18` reports one violation: `crates/slipfork-edge/tests/critical_edge.rs`, modified by `M3.28.17` (not staged, not this node).
- `docs/design/ospp.md` appeared as an untracked file during this node's work (not created by this node, not in its paths, not committed). It uses `auth_ok`/`auth_fail` frame names rather than the shipped `welcome`/`reject`; the published page does not reference it.
- Final: `docs/reference/ospp.md` **19 lines**, **0 diagrams**.
