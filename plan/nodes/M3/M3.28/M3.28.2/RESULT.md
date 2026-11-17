# M3.28.2 RESULT — `dx` + `ai-ergonomics` Critic Pass on the `0.1.0` UX DX Spec, Applied In Place

- **Status:** `done`
- **Node:** `M3.28.2` (`release/0.1.0`, base `eb15d1f`)
- **Reviewers:** two fresh-context `critic` subagents spawned in parallel, one per lens, each given only the spec, the owner's 12 verbatim complaints, and its lens.

## Deliverables

- `docs/design/dx-0.1.0-ux.md` (370 lines): every accepted correction applied in place — still the single normative home for the `0.1.0` CLI table and `--json` schemas. All nine locked decisions and the seven locked tokens survive.
- `docs/design/dx-0.1.0-ux.review-dx.md` (41 lines): `dx` lens, 14 MAJOR + 11 MINOR, `VERDICT: changes-applied`, plus the author disposition.
- `docs/design/dx-0.1.0-ux.review-ai-ergonomics.md` (40 lines): `ai-ergonomics` lens, 2 BLOCKER + 9 MAJOR + 10 MINOR, `VERDICT: changes-applied`, plus the author disposition.
- No code, no `plan/` state, no other doc touched.

## Corrections applied (owner-evidence driven)

1. **Every promised fact traces to a daemon field.** `IngressInfo.addrs` (adopted HTTP listeners) backs the `slipforkd … ingress …` header; `WorldUpSummary.changed/restarted/services/next`, `WorldSummary.lineage`, `WorldProcessReport.state/lineage/services` back the blocks; `up`/`restart`/`fork` return `services[]` + `next` in one document, and the port rides in `origin`; `open --remote`'s URL is the origin host plus the constant `8080`; `doctor`'s report projects to `"healthy"` and `ServiceImageLineage.id` to `"image"`, so `ok` never doubles as a verdict.
2. **One way to do one thing.** One output block and key order (`world`, `app`, `worktree`, `restarted`/`cd`, `next`, `lineage`) across `up`/`restart`/`fork`; one `lineage` renderer with one order and one cache vocabulary (`base … reused` replaces the footnote `-`); `status` uses `next` (no `->`); one `WorldId` vocabulary (`demo:feat-ux`, not `:feat-ux`); `daemon unit` is declared render-only (no `unit | sudo tee`); `-v` is human-only detail while `--json` is always maximal.
3. **No internal vocabulary on a happy path.** `[wrote]`/`[ok]`/`[enabled]`/`[restarted]` tags became keyed rows; `ps` drops the `KIND` column and says ports are in-world; `[resources]`/`[lineage]` survive only under `-v`.
4. **Every failure ends in one actionable line naming the next command.** `sudo slipfork daemon install` (with `sudo`) in the `:40645` notice, both fail-closed install paths, and the `doctor` ingress hints; resolved fork values instead of `<fork>`; one shared "not running" string; `ps` no longer fails for a world that exists; unknown-world gets its own line; `--json` failures are `{"ok": false, "code", "error", "next"}` on stdout with exit `1`/`2`.
5. **No man page, `getent`, or `/proc` detour.** The install transcript's `enabled … as slipfork` line names the daemon account; `list` gains a paste-ready `NEXT` column (`slipfork open --world <id>`); `ps` names `slipfork db` so nobody tries host `psql -p 5432`; the `shell` orientation line tells the user to exit before running host commands.
6. **Owner rulings held.** `dag` is deleted with no alias, no `slipfork plan`, and no `0.2.0` sample output in `0.1.0` material; `share` remains absent with a single `Local` origin kind.
7. **`--json` for agents.** One exhaustive contract: object verbs `{"ok": true, ...}`, four named collection verbs (with their shipped consumers named), `logs` the single NDJSON stream, `exec`/`shell`/`db`/`redis` reject `--json`, `WorldLineage` typed with explicit `null`s, both `state` enums defined, global flags pinned `global = true`.

## Verified `# Acceptance`

```console
$ test -s docs/design/dx-0.1.0-ux.review-dx.md && test -s docs/design/dx-0.1.0-ux.review-ai-ergonomics.md && echo both
both
$ grep -q '^VERDICT: \(pass\|changes-applied\)' docs/design/dx-0.1.0-ux.review-dx.md && grep -q '^VERDICT: \(pass\|changes-applied\)' docs/design/dx-0.1.0-ux.review-ai-ergonomics.md && echo verdicts
verdicts
$ for t in 'slipfork status' 'slipfork restart' 'slipfork ps' 'slipfork open --remote' 'user.name=slipfork' 'slipfork daemon install' 'lineage'; do grep -qF "$t" docs/design/dx-0.1.0-ux.md || { echo "lost: $t"; exit 1; }; done; echo still-ok
still-ok
$ ! grep -qE '\[(derived|guessed)\]|TODO|TBD' docs/design/dx-0.1.0-ux*.md && echo clean
clean
$ uv run --no-project tools/guard.py && uv run --no-project tools/plan.py validate
guard: OK (544 file(s) checked)
plan OK
```

Role gate as well:

```console
$ uvx --offline zensical build --strict
Build started
No issues found
Build finished in 0.21s
```

## Attempt 2 — out-of-path findings closed (`docs/design/target-dx.md`, `ARCHITECTURE.md`)

Paths widened by the lead; no `docs/design/dx-0.1.0-ux.md` decision changed.

- **`docs/design/target-dx.md` now renders the dx spec only.** §1.2's block was replaced by the dx transcripts verbatim: `up` (dx §1) and `restart` (dx §4) are **byte-identical**, the `db`/`logs` rows are gone (folded into the single `next` list), `restarted pg_clusters.psql, api` replaces `[restarted] pg_clusters.main, api`, and `base 4570132bffb8 reused` replaces `base … -`. The already-`running` `up` block is **not** restated: §1.2 points at dx §4. The `-v`/`--no-dump`/`--refresh-dump`/`--resources local` lines remain, and `-v` is the only detail switch (`--json` is the maximal machine document; `[resources]` rows never land on stdout).
- **Other duplicated renderings aligned:** §1.5's `slipfork list` table now uses dx §10's `WORLD / STATE / ORIGIN / NEXT` columns with paste-ready `slipfork open --world <id>` cells (and §1.6's cross-project remedy now points at `status --json`'s `worktree` instead of the removed `WORKTREE` column); §1.3's `shell` orientation line is dx §8.4's exact sentence; §1.1's install invocation and rootless-fallback notice use `sudo slipfork daemon install` (with `--user <account>` documented as the override); §1.7's two ingress hints are ``run `sudo slipfork daemon install` `` and both `doctor` transcripts carry `/home/slipfork/.cache/slipfork`, making them byte-identical to dx §2.
- **One example world.** `acme` → `demo` (and `/home/acme` → `/home/slipfork`, `/repo` → `/home/slipfork/demo`, `project = "demo"`) across §1.1–§1.7 and the `slipfork.toml` sample, so the doc never shows two spellings of the same world. `billing` stays the fork example (`demo:billing`).
- **`ARCHITECTURE.md:1002` narrowed to one sentence** that links §10 instead of restating schemas: `ok` envelope for human-readable commands, the four bare collection verbs, and an `{"ok": false, "code", "error", "next"}` failure document.
- **No resurrection:** `dag` appears nowhere; `share` appears only as the `M3.28.15` deletion; `--watch` appears only as "do not exist in `0.1.0`".

```console
$ grep -n 'base <key12> -\|\[restarted\]' docs/design/target-dx.md; echo "---"
---
$ diff <(sed -n '/^```console/,/^```$/p' docs/design/dx-0.1.0-ux.md) <(sed -n '/^```console/,/^```$/p' docs/design/target-dx.md) >/dev/null; echo "transcripts-differ=$?"
transcripts-differ=1
```

`transcripts-differ=1` is expected: `target-dx` keeps its install, hook-rescue, `list`, doctor and `toml` blocks, which dx does not own. Byte-identical where they repeat: `up` (dx §1), `restart` (dx §4), `doctor` (dx §2). `target-dx` carries no `status`, `ps`, or `open` transcript at all — §1.5 delegates those to dx §3/§5/§6 — so none could diverge.

Node acceptance re-run after the edit: `both`, `verdicts`, `still-ok`, `clean`, `guard: OK (545 file(s) checked)`, `plan OK`, and `uvx --offline zensical build --strict` → `No issues found`.

## Round 3 — Quad Review rulings R1–R14 (attempt 3)

All rulings applied; no new decision invented. Line refs are `docs/design/dx-0.1.0-ux.md` unless said otherwise.

- **R1 failure document** — defined once at §10:316–335: open base `{"ok": false, "code", "error", "next", …extras}` with a closed per-code extras table (`no_daemon`→`path`, `unknown_world`/`not_running`/`fork_exists`→`world`, `boot_failed`→`world`+`state`, `no_browser`/`no_branch`/`not_root`→none, `ingress_missing`→`addrs`, `variable_collision`→`key`). §4/§5/§6 now only name their code instead of restating the object.
- **R2 no shared envelope** — §3:143 and §10:314 say each verb owns its document; §10's rows are labelled object / collection / no document. The "folding them under `ok` is a follow-up" framing is gone. `ARCHITECTURE.md:1005` is now one sentence: each command owns its document, no shared projection, `logs --json` is the NDJSON stream, `exec`/`shell`/`db`/`redis` reject `--json`, failures use R1 (it links §10 rather than restating schemas).
- **R3 `ps` state** — §5:176–193 has `ps` render and return `state` from the same `world_processes` read (no second `status`); `ARCHITECTURE.md:854` `WorldProcessReport { state, lineage, services }` with `M3.28.3` named as the wiring leaf; §10 `ps` row carries `"state"`.
- **R4 no client `--runtime-dir`** — §2 rule 6 (:110) makes it `slipforkd`-only (plus the unit renderer) and states the reason (a client override could point at a different `daemon.sock` than the installed unit and silently win); §10:312 deletes it from the global flags; `ARCHITECTURE.md:365` `Layout::resolve` takes no runtime-dir override. `M3.28.6`/`.7` implement, `M3.28.14` publishes.
- **R5 read verbs never auto-spawn** — §1:43 states `up`/`fork` are the only spawners with the exact one-line `no_daemon` remedy; §3:137 (bare `slipfork` on a cold host), §10's `no_daemon` row and the `up`/`fork` rows agree. `target-dx.md` §1.1 now says the same and points at §1.
- **R6 `up --json` self-sufficient** — §4:173–174 + §10:340: `world`, `state`, `changed`, `restarted`, `worktree`, `origin`, `lineage`, `services[]`, `resources[]`, `next`; `ARCHITECTURE.md:861` `WorldUpSummary` gains `services` and `next`. `restart`/`fork` reuse the document.
- **R7 `snow resolve` collision** — §10:359 keys are the project's variables, `ok`/`code`/`error`/`next` are reserved, and a collision fails closed with the exact one-liner (`variable_collision`).
- **R8 always-present keys** — §8.2:284 states the typing rule once (every key present, `null` when unknown); §10:314 references it; `doctor`'s `ingress` sub-object and `hook run`'s `world` lost their `?` markers.
- **R9 `restarted` ids** — §4:170 keeps `[str]` and says entries are this world's `services[].id`; §3:141 states id uniqueness within `services[]` as the wire invariant that makes the join sound; `ARCHITECTURE.md:866` repeats it.
- **R10 `next` table** — §1:34–41 is the only membership statement (`running`→`open`,`db`,`logs`,`restart`,`down`; `failed`→`logs`,`restart`,`down`; `stopped`→`up`,`logs`,`down`), with the `--world` rule; every transcript and `§10`'s `list` `NEXT` column derive from it. The "23 top-level subcommands … = 24 entries" arithmetic is deleted from §3.
- **R11 `addrs` order** — §3:141 and `ARCHITECTURE.md:820` say the order is non-semantic ("in adopt order" dropped).
- **R12 one home per fact** — lineage renderer/key order only in §8.2 (§1:32 points at it); `doctor`'s document only in §10 (`target-dx.md` §1.7 points at it and no longer restates `--json` keys); `dag`/outbound-sharing tombstones reduced to one line each (§8.1:255, §9:304) pointing at `ADR-0011` + the epic node worklist, and the five+ restatements in `ARCHITECTURE.md` (§1, §2 rows, §3.4, `IntegrationCategory`, H-005/H-006) and `target-dx.md` §1.1/§1.5 are stripped.
- **R13 one outcome vocabulary** — §8.2:263–284: human text uses only `cached` / `rebuilt <dur>` / `reused` / `sealed <dur>` (wire `hit|miss|miss_offline` is JSON-only, `miss_offline` renders `-`); `base … reused` and `<image> cached` are used consistently in every transcript and in the `-v` rows; the blanket "every unmeasured field renders `-`" claim is replaced by "a node the daemon did not record renders `-`".
- **R14 one remedy per failure** — §2 rule 2 no longer mentions `daemon unit | sudo tee`; rule 5's userns/ingress hints end at `sudo slipfork daemon install`; `target-dx.md`'s userns hint drops `daemon unit --only apparmor`; the `$XDG_RUNTIME_DIR/slipfork` migration prose is gone from §2, `target-dx.md` §1.1, and `ARCHITECTURE.md` §1; `ARCHITECTURE.md:737` no longer offers `daemon unit --only apparmor` as a remedy.

Round-3 verification (R14 scoped to the three docs this node owns; the CI renderer-parity mentions in `do-vm-harness.md` / `world-base-image.md` are legitimate uses of the still-real `daemon unit --only apparmor` renderer subcommand and are out of scope by ruling):

```console
$ ! grep -rn 'daemon unit --only\|daemon unit | sudo tee\|\$XDG_RUNTIME_DIR/slipfork\|readiness_port' docs/design/dx-0.1.0-ux.md docs/design/target-dx.md ARCHITECTURE.md && echo no-second-path
no-second-path
$ grep -c 'ok' docs/design/dx-0.1.0-ux.md >/dev/null && grep -n 'shared projection' docs/design/dx-0.1.0-ux.md ARCHITECTURE.md; echo "---"
---
$ uvx --offline zensical build --strict
Build started
No issues found
$ uv run --no-project tools/check_docs_honesty.py
check_docs_honesty: OK (9 published doc(s) match real CLI subcommands)
$ uv run --no-project tools/guard.py && uv run --no-project tools/plan.py validate
guard: OK (545 file(s) checked)
plan OK
```

Node acceptance re-run: `both`, `verdicts`, `still-ok`, `clean`, `guard: OK`, `plan OK`. `target-dx`'s `up`, `restart`, and `doctor` transcripts remain byte-identical to dx §1/§4/§2.

### Round-3 close-out

- **R14 verification scoped:** the ban applies to the *install remedy*, not to the renderer subcommand. `daemon unit --only apparmor` stays a real command (and stays covered by the CI byte-equality assertion in `do-vm-harness.md` and the `M3.15` ledger row in `world-base-image.md`, both outside this node). The scoped grep over this node's three documents is clean: `no-second-path`.
- **`ADR-0011` tombstone amendment:** `docs/decisions/ADR-0011-port-80-daemon-install-and-runtime-dir.md` gains `## Deletion Tombstone (0.1.0)` — two lines recording that `0.1.0` deleted the `dag` command plus the unreachable planner surface behind it (plan view re-enters at `M3.2`) and the whole tunnel-sharing surface (`slipfork share`, the `TunnelRegistrar` family, `WorldOrigin::Shared`; sharing re-enters at `M4.1`/`H-006`). §8.1 and §9 now say `tombstone: `ADR-0011``, so the one-line pointers land on a real record; no deletion narrative is restated in the specs.

## Attempt 4 — confirmation review S1–S7

- **S1 `status` carries no process rows (§10 status row).** The per-world row is `{world, parent, state, origin, worktree, lineage}`; `services` is deleted from it and the row's summary points at `slipfork ps --json`. `WorldSummary` in `ARCHITECTURE.md` §3.4 is untouched (no `services`), and `up`/`restart`/`fork` remain the only documents with `services[]`.
- **S2 every `fork` failure has a code.** §10's closed table gains `in_fork_worktree` (extras `worktree`) and `name_collides_with_project` (extras `name`), and §7's four fail-closed cases each name their code (`no_branch`, `in_fork_worktree`, `fork_exists`, `name_collides_with_project`).
- **S3** `snow resolve`'s collision message now says "collides with the reserved ok/code/error/next keys" (no envelope wording, per R2).
- **S4** `no_daemon` keeps exactly one remedy: ``slipfork: no slipforkd on <path> — run `slipfork up` `` (the `sudo systemctl start` parenthetical is gone).
- **S5** `ADR-0011`'s non-root message is now byte-identical to dx §2 rule 1 (`… run `sudo slipfork daemon install``), with no `--user <account>` placeholder.
- **S6** `ARCHITECTURE.md`'s `ResourceCoordinates` bullet points at the tombstone instead of restating the `Shared` variant (`one origin kind — tombstone in `ADR-0011``).
- **S7** `RESULT.md`'s corrections item 1 now states what the spec says: `up`/`restart`/`fork` return `services[]` + `next` in one document, and the port rides in `origin` — no `parent`/`ingress` claim.

```console
$ grep -n 'services' docs/design/dx-0.1.0-ux.md | sed -n '1,40p'
141: … `WorldUpSummary { …, services: Vec<WorldServiceSummary>, … }`, `WorldProcessReport { state, lineage, services }`, …
149: `restart` stops the world's supervised components and `crun` backing services …
170: each entry is a `services[].id` of this world …
172: `up`, `restart`, and `fork` each return … `services[]`, `resources[]`, and `next` …
178: `world_processes(&WorldSelector) -> WorldProcessReport { state, lineage, services }` …
193: `ps` document — `{"ok": true, "world", "state", "lineage", "services"}`
264: `services: Vec<ServiceImageLineage>` (lineage images) …
273-274: lineage table rows
342-348: `up` / `restart` / `ps` / `down` table rows
# no `status` row carries `services`
$ grep -c 'in_fork_worktree\|name_collides_with_project' docs/design/dx-0.1.0-ux.md
4
$ uvx --offline zensical build --strict && uv run --no-project tools/check_docs_honesty.py && uv run --no-project tools/guard.py && uv run --no-project tools/plan.py validate
No issues found
check_docs_honesty: OK (9 published doc(s) match real CLI subcommands)
guard: OK (545 file(s) checked)
plan OK
```

Node acceptance re-run: `both`, `verdicts`, `still-ok`, `clean`, `guard: OK`, `plan OK`; `target-dx`'s `up` / `restart` / `doctor` transcripts remain byte-identical to dx §1/§4/§2.

## Followups (outside this node's `paths` — for `lead`/`conductor`)

- Folding `list --json`'s bare array under the `ok` envelope needs a node that owns `packages/opencode-slipfork/**` (its `parseList` reads the array today), plus `tools/check_support.py` and `crates/slipfork/tests/critical_cli.rs` for the other three collection verbs.
- R14's ban is on the install *remedy*, not the renderer subcommand: the three `daemon unit --only apparmor` mentions in `docs/design/do-vm-harness.md` and `docs/design/world-base-image.md` are a CI byte-equality assertion and a milestone ledger row, so the R14 grep is scoped to this node's three documents (`no-second-path`).
- `ADR-0011` now carries the `dag` / outbound-sharing tombstone (`## Deletion Tombstone (0.1.0)`), so the `§8.1`/`§9` pointers land on a real record.
- `docs/design/target-dx.md` keeps its own non-duplicated material (install, hook rescue, `toml` sample, `M3.2` Phase-2 section). Nothing there contradicts the dx spec after this pass, but `M3.28.14` should re-grep it during the published-docs sweep.
