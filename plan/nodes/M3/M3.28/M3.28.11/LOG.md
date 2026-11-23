# Log M3.28.11

## 2026-11-17T16:33 [planner]

created (task) Polish: dag plan never silent, quiet world entry, per-component log prefixes, [resources] behind -v

## 2026-11-17T17:32 [lead]

edited fields

## 2026-11-17T17:53 [lead]

edited fields

## 2026-11-17T21:44 [lead]

From M3.28.18 RESULT (route): BootEvent::Secrets is rendered by crates/slipfork/src/main.rs:1110-1115 but never emitted by slipforkd (ensure path emits nothing), and no -v flag exists yet. With -v landing here: either emit [secrets] ensured secrets::KEY -> <vault> (0600) from the !! ensure path or delete the unused event+renderer; do not let docs describe a line that never prints.

## 2026-11-18T04:10 [lead]

status ready -> active

## 2026-11-18T04:10 [lead]

attempt 1/3 started

## 2026-11-18T04:10 [lead]

base 97efc4ac173767820f5a1332fc6e25d6342f9f0e

## 2026-11-18T04:14 [lead]

edited fields

## 2026-11-18T04:14 [lead]

attempt 1 blocked correctly on three scope questions; rulings: (1) A — critical_slug.rs added to paths, delete ONLY its dag-plan blocks (snow resolve stays); (2) A — the cli-planner-free grep drops SupportRegistry/category_label/evidence_json because 'slipfork support' legitimately uses them; it stays a planner-free check (DagPlanner/FsDigestCache/FsMerkleHasher); (3) split the two non-CLI polish bugs into M3.28.22 (quiet world entry) and M3.28.23 (labelled logs) since they are engine/edge work, not main.rs. Invariant 6 ([resources] behind -v) is already satisfied — verify, do not redo.

## 2026-11-18T04:15 [lead]

edited fields

## 2026-11-18T04:15 [lead]

attempt 2/3 started

## 2026-11-18T04:33 [lead]

edited fields

## 2026-11-18T04:33 [lead]

attempt 2 done and verified in an isolated worktree (make check exit 0 there): dag deleted everywhere, support/planner machinery kept, [resources] already behind -v. Paths widened for the three companions it correctly refused to touch: support/integrations.toml recipe_page -> manifest.md, the dag-plan sentence in docs/reference/manifest.md, and ARCHITECTURE.md:92's stale claim (it keeps src/fs.rs + the fs feature; only the CLI dependency's feature is dropped).

## 2026-11-18T04:34 [lead]

status active -> done

## 2026-11-18T04:34 [lead]

companions settled: recipe_page -> manifest.md, manifest.md's dag sentence rewritten, ARCHITECTURE:92 corrected (keeps src/fs.rs + the crate's fs feature; drops only the CLI dependency's). check_support OK 10 integrations, docs honesty OK 10 docs, guard 604, plan OK, docs-check green. Committed together with M3.28.22.
