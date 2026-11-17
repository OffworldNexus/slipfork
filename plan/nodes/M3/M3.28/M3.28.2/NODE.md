---
id: M3.28.2
title: "Critic pass (dx + ai-ergonomics) on the 0.1.0 UX DX spec, applied in place"
kind: task
status: ready
owner: "doc-writer"
priority: 2
depends_on: [M3.28.1]
paths:
  - "docs/design/dx-0.1.0-ux.md"
  - "docs/design/dx-0.1.0-ux.review-dx.md"
  - "docs/design/dx-0.1.0-ux.review-ai-ergonomics.md"
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-17T16:32"
updated: "2026-11-17T16:32"
---

# Goal

Run the mandatory fresh-context `dx` and `ai-ergonomics` critic pass over `docs/design/dx-0.1.0-ux.md` **before any code**, apply every accepted correction in place, and record both lens reports next to the spec.

# Context

The owner's real transcripts (milestone Context, facts 1–12) are the acceptance evidence: four bare `slipfork` invocations wanting status, a `restart` reflex, a hand-written `ssh -L`, a `getent passwd` detour, and one happy-path line of internal jargon. Every extra flag, every internal noun (`prefer_remote`, `not_offered`, `netns`, `L0`), every unprompted wall of text, and every silence is a defect (skill `dx-first` §1.2, §2).

# Design / Invariants

1. **Review, do not invent scope.** Corrections are wording, ordering, defaults, and deletions. A missing *decision* (not a wording problem) goes to `plan.py human add --kind direction-challenge`; never guess.
2. **Hunt these five defects specifically:**
   - output the daemon cannot produce with the locked capabilities (would force a later fallback or a lie);
   - a second way to do one thing (e.g. `up --watch` beside `restart`, `daemon unit | sudo tee` beside `daemon install`, an env switch beside `-v`) — `review-cruft` will reject it;
   - internal vocabulary on a happy path (fact 8) and any `[tag]` line whose only reader is a developer;
   - a failure that does not end in exactly one actionable line naming the next command;
   - a step that requires reading a man page, `/etc/passwd`, or `/proc` to proceed (facts 4, 9, 10).
3. **AI-ergonomics lens:** can an agent (and a tired human) predict the output from the verb? Is every failure self-describing enough to act on without a second command? Are `--json` payloads sufficient for a program (world id, state, port-less origin, ingress, process tree) with stdout purity?
4. Apply corrections directly in `docs/design/dx-0.1.0-ux.md` (single spec). If a seam sentence in `ARCHITECTURE.md` becomes wrong, fix that sentence too.
5. Each lens file is short (`<= 60` lines), lists concrete findings, and ends with a single line `VERDICT: pass` or `VERDICT: changes-applied`.

# Acceptance

```console
$ test -s docs/design/dx-0.1.0-ux.review-dx.md && test -s docs/design/dx-0.1.0-ux.review-ai-ergonomics.md && echo both
both
$ grep -q '^VERDICT: \(pass\|changes-applied\)' docs/design/dx-0.1.0-ux.review-dx.md && grep -q '^VERDICT: \(pass\|changes-applied\)' docs/design/dx-0.1.0-ux.review-ai-ergonomics.md && echo verdicts
verdicts
$ for t in 'slipfork status' 'slipfork restart' 'slipfork ps' 'slipfork open --remote' 'user.name=slipfork' 'slipfork daemon install'; do grep -qF "$t" docs/design/dx-0.1.0-ux.md || { echo "lost: $t"; exit 1; }; done; echo still-ok
still-ok
$ ! grep -qE '\[(derived|guessed)\]|TODO|TBD' docs/design/dx-0.1.0-ux*.md && echo clean
clean
$ uv run --no-project tools/guard.py && uv run --no-project tools/plan.py validate
guard: OK
plan OK
```

- Both lens files exist, are non-empty, and end with a `VERDICT:` line.
- The seven locked tokens survive the edits (the critic may reword prose, not drop a decision).
- A `direction-challenge` in `plan/HUMAN.md` is the correct outcome instead of a guess; then return `STATUS: blocked` (`BLOCK_KIND: direction-challenge`).
