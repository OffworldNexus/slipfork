# Log M3.28

## 2026-11-17T16:32 [planner]

created (epic) 0.1.0 UX pre-release pass (owner hands-on findings): :80 ingress, bare status, restart/ps/open, fork identity

## 2026-11-17T19:39 [lead]

M3.28.3 (dc9bd20) and M3.28.5 (b49faef) committed. Note for the gate: critical_guard --node <ID> is single-node, so with two nodes' protected edits inside one base..tree range each run flags the sibling's file. Each node's own changes are authorized by its own paths (verified), and the epic gate uses the no-node report form, which is OK.

## 2026-11-17T23:36 [lead]

note: docs/design/ospp.md is dirty in the worktree from outside this epic (OSPP e2e public_key rewrite) — NOT ours, never staged by M3.28 commits. Leave it alone unless the owner asks.

## 2026-11-18T22:19 [planner]

status ready -> done: Remaining scope absorbed into M3.30.
