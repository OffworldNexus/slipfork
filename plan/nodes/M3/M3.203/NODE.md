---
id: M3.203
title: "Docs: support scope statement (no tiers/SLAs; DigitalOcean infra; manual only for absolute emergencies)"
kind: task
status: done
owner: "doc-writer"
priority: 3
depends_on: []
paths: [docs/index.md, README.md]
attempts: 0
max_attempts: 3
escalation: none
created: "2026-11-27T14:19"
updated: "2026-11-27T14:19"
---

# Goal

The owner wants a **support-scope statement**, not a support-tier matrix: "let's not go into which are the levels, let's just mention what is the scope of what we do… infrastructure is entirely managed by DigitalOcean, things we would have to do would be manual interventions for stuff that isn't our fault (third party failing), fixing an edge case in some process that is blocking everything… It would have to constitute an absolute emergency, and is very rare. So that's why we don't think it's required at this stage."

# Where

- `docs/index.md`: one short **Support** section (3–4 sentences max), after the pre-1.0 note or near the end.
- `README.md`: one sentence in the existing pre-1.0 area, no repetition of the whole block.

# What it must say (and not say)

- No tiers, no plan names, no SLA, no uptime numbers, no "priority" vocabulary.
- The infrastructure is **managed by DigitalOcean** — we don't operate the metal.
- We intervene **manually only in an absolute emergency**: a third-party failure we can work around, or an edge case in one of our processes that blocks everything.
- That is **rare**, and at this stage there is **no support plan**; ordinary reports go to the issue tracker on a best-effort basis.
- No promises of response times.

# Suggested copy (adapt, keep it lean and in the project's voice)

```
## Support

There are no support tiers or SLAs at this stage. The infrastructure runs on DigitalOcean, which we do not operate.
We step in manually only for an absolute emergency — a third-party failure we can work around, or a blocking edge case in our own process — and that is rare.
Everything else is best-effort through the [issue tracker](https://github.com/OffworldNexus/slipfork/issues).
```

# Acceptance

- No tier/SLA/priority language anywhere in `docs/` or `README.md`.
- `make docs-check` green; commit `[M3.203] ...` with `docs/index.md` and `README.md` only, done, no push.
