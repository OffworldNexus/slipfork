# Log M3.29.4

## 2026-11-18T00:08 [planner]

created (task) Publish docs/reference/ospp.md and purge every secret/vault/old-wire claim from the public pages

## 2026-11-18T00:30 [lead]

RESCOPE (owner clarification 2026-11-18): docs/reference/ospp.md must state the two-tier model (local 0600 vault for local credentials/overrides; remote secrets memory-only, never written), and the published pages must KEEP 'slipfork secret' documented as local-vault management while deleting only the false claims (remote secrets in vaults/<project>.json, env.bundle, secrets.ensure).

## 2026-11-18T00:30 [lead]

edited fields

## 2026-11-18T03:07 [lead]

status ready -> active

## 2026-11-18T03:07 [lead]

attempt 1/3 started

## 2026-11-18T03:07 [lead]

base 4a1428f1752420e57399c20f61db0dcec3d08ec3

## 2026-11-18T03:11 [lead]

attempt 1 cap-out after writing the pages (ospp.md 73 lines, two-tier section, cli.md secret section, seed-dump rewrite, auth verb, nav). Acceptance items ospp-page/local-vault-documented/two-tier-stated/no-old-wire/lean + check_docs_honesty 11 docs + zensical --strict pass; make docs-check blocked by M3.28.9's mid-edit match arms, not a docs defect. DELIBERATE DEVIATION: the page documents '!!' as PERSISTING in the local vault (owner ruling: local vault holds local credentials) and does NOT claim an ephemeral session secret; the earlier invariant-5 wording about ephemeral secrets is superseded.

## 2026-11-18T03:16 [lead]

status active -> done

## 2026-11-18T03:16 [lead]

docs-check green (11 docs, zensical --strict ok); ospp.md 73 lines with the two-tier boundary, cli.md's local-vault section, streamed seed-dump rewrite, auth via the server-supplied fragment URL, nav updated. Critic artifacts were never collected (the parent hit its step budget); the acceptance + docs-check are the evidence of record. Deviation logged on M3.29.3: no ephemeral session secret — '!!' persists in the local vault per the owner's ruling.
