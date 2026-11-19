# Review: dx lens

Reviewed: `docs/design/dx-0.1.0-ux.md` @ `eb15d1f` (M3.28.1, 335 lines). Line refs are `§<section>:<line>` in that file.

[MAJOR] §3:105 — `status` prints the fork world as `:feat-ux`, while §7:231 prints `demo:feat-ux-pass` and `--json` `"world"` is `WorldId` (`demo:<fork>`); `:feat-ux` is not accepted verbatim by `--world`, so copy-paste from the dashboard fails -> print `demo:feat-ux` (the indent already shows the parent).
[MAJOR] §1:21 vs §3:103 vs §5:190 — three compact `lineage` renderings with different field order and membership (`ps` silently drops the service-image cell) -> one renderer, one order: `rootfs <d12> <cache> · <service-id> <cache> · seed <d12> <cache> · base <k12> <seal>`.
[MAJOR] §4:165 + §4:167 — warm base reuse renders `-` while rootfs/seed reuse renders `cached`, and the `-` needs a three-line footnote to be readable -> render `base 4570132bffb8 reused`, delete the footnote, mirror in §8.2:268.
[MAJOR] §4:180 vs §5:197 — two different strings for the same "world not running" condition, one leaking `in this slipforkd` -> one string: `slipfork: world 'demo' is not running; boot it with slipfork up`.
[MAJOR] §2:77 — the single actionable stderr line names `slipfork daemon install`, which itself fails closed ("run it as root"), and hardcodes `demo` -> `slipfork: serving http://demo.localhost:40645 — run \`sudo slipfork daemon install\` once for port-less http://demo.localhost`.
[MAJOR] §2:60-65 — the fail-closed lines print `sudo slipfork daemon install --user slipfork` (one hardcoded account) and `--user <account>` (a literal placeholder no CLI can print) -> both end with `sudo slipfork daemon install`; `--user` appears only in rule 1 as an override.
[MAJOR] §2:43 — the canonical first-run transcript teaches `sudo slipfork daemon install --user slipfork` although `is_root` already falls back to `invoking_account()` -> `sudo slipfork daemon install`.
[MAJOR] §2:44-53 — install prints four `[wrote]`/`[ok]`/`[enabled]` `/etc` paths plus a second six-line transcript only to show idempotency -> two lines: `[ok] installed slipforkd.socket + slipforkd.service (ingress :80)` / `next  cd <repo> && slipfork up`; an unchanged re-run is one line.
[MAJOR] §5:191-193 — `KIND backing_service` is internal vocabulary and `PORT 5432` is in-world only, so the host `psql -p 5432` it invites is refused (`target-dx.md:63`) -> kinds `db`/`app`, header `WORLD PORT`, trailing `next  slipfork db`.
[MAJOR] §8.4:295 — the shell orientation line tells the user to run `slipfork ps` from inside a rootfs with no host tools, so the `ps: command not found` reflex still fires -> `world rootfs has no host tools (no ps/curl); exit and run slipfork ps | slipfork logs | slipfork open`.
[MAJOR] §10:320 — `slipfork status` still has no next-command line (evidence #11) while every other inspection command gained one -> add one trailing `next  slipfork open --world <world>`.
[MAJOR] §1:18-19 — three of seven boot lines are static command hints and `db  slipfork db` reads as a key whose value is a command -> delete the `db` and `logs` rows; `next  slipfork open  |  restart · db · logs`.
[MAJOR] §1:20 vs §4:164,177 — the `next` order flips between transcripts for the same `running` state -> one order per state, `open` first when the world just booted.
[MAJOR] §1:33 / §8.2:274 — `-v` and `--json` are presented as interchangeable ways to get detail -> `-v` is the only detail switch; delete "(or `--json`)".
[MINOR] §10:320 — `list --json` is a bare array where §3:122 promises one `{"ok": true, …}` envelope (same break in `secret list`, `support`, `slipfork env --reveal`) -> wrap in the shared projection or state the exception once.
[MINOR] §2:91 — the `installed but inactive` hint teaches raw `sudo systemctl start slipforkd.socket` beside the single installer -> `sudo slipfork daemon install`.
[MINOR] §1:115 — `no worlds running — cd into a checkout and run \`slipfork up\`` names no single command -> `no worlds — run \`slipfork up\` inside a project checkout`.
[MINOR] §7:234 — `cd  cd /home/slipfork/demo/.worktrees/feat-ux-pass` duplicates the `worktree` row printed one line above -> delete the row.
[MINOR] §7:242 — the runtime message prints the literal `<fork>` placeholder -> print the resolved fork name.
[MINOR] §1:34 — a six-line `Layout::for_account`/`ResolvedAccount`/`sun_path` paragraph sits in the lead "four-command" section -> move to §2; §1 keeps the `--state-dir` default in one sentence.
[MINOR] §8.1:255-256 — `M3.28.x` task bookkeeping and a quoted `0.2.0` sample output (`"31 cached · 9 rebuild across 8 worlds"`) live in `0.1.0` material -> keep one sentence ("planning is `0.2.0` scope; no `0.1.0` surface") and move the rest to the plan node.
[MINOR] §10:308 — `--runtime-dir` is a global user flag beside `--state-dir`, giving two knobs for one socket path -> hide it (daemon/unit-only); `--state-dir` is the single user knob.
[MINOR] §6:211-213 — the URL row is labelled `open` here but `app` in §1/§4/§7 -> label it `app`.
[MINOR] §3:107 — `status` uses `->` where `up`/`fork`/`restart` use `next` -> one label.
[MINOR] §2:40 — crate paths (`render_service_unit`, `crates/slipfork-edge/tests/golden/slipforkd.service`) sit in the section promoted verbatim to published docs -> move into §2's rules or leave to `ADR-0011`.

## Needs human decision

(none)

## Author disposition (`M3.28.2`)

- **Applied in place:** all 14 MAJOR and 10 of 11 MINOR findings — one lineage renderer with `base … reused`, one `world` id vocabulary (`demo:feat-ux`), one "not running" string, `sudo` in every actionable line, canonical install transcript without `--user`, `ps` without internal `KIND` plus in-world port note, actionable `list` `NEXT` column, shell orientation line naming host commands from outside, `-v` vs `--json` split, doctor hint via the installer, resolved fork values in errors, Layout paragraph moved to `§2`, `0.2.0` sample removed, `--runtime-dir` labelled as override only.
- **Rejected (MINOR, with reason):** the fork `cd` row stays. `worktree` is the data field an editor needs; `cd` is the one pasteable command that gets the user there. They render different things and both are cheap.
- **Resolved as documented instead of changed:** `list --json` stays a bare array (shipped consumers `packages/opencode-slipfork/src/cli.ts`, `tools/check_support.py`, `critical_cli.rs` parse it today; folding it under `ok` needs a node that owns `packages/opencode-slipfork/**`).

- **Round 3 (reviewer gate):** the `review-ioc` / `review-legality` / `review-cruft` rulings R1–R14 are applied on top — failure document with per-code extras, per-verb `--json` documents with no shared projection, `ps` state from the same read, daemon-only `--runtime-dir`, `up`/`fork`-only auto-spawn, `up` carrying `services[]` and `next`, reserved `slipfork env --reveal` keys, always-present keys, `restarted` id uniqueness, the `next` table, non-semantic `addrs` order, one home per fact, the four-word outcome vocabulary, and one remedy per failure.

VERDICT: changes-applied
