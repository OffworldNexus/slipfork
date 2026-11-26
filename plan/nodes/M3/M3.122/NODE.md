---
id: M3.122
title: "sanitize(): transliterate accents and collapse separator runs (Remy, not R-m-y)"
kind: task
status: done
owner: implementer
priority: 3
depends_on: []
paths: ["crates/slipfork-image/src/steps.rs", "crates/slipfork-image/Cargo.toml", Cargo.toml, Cargo.lock]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-24T17:55"
updated: "2026-11-24T17:56"
---

# Problem

`crates/slipfork-image/src/steps.rs::sanitize` maps every non-ASCII/non-alphanumeric char to `-`: `Rémy` → `R-m-y`, `a!!b` → `a--b`, `a----b` → `a----b` (no run collapse). Owner: "what if there are two non-compliant characters in a row? can't endlessly have ----" and "proper unaccenting; my name is Rémy and I hate rmy".

# Change

- Add `deunicode` (workspace dependency + `slipfork-image`) and transliterate to ASCII first: `Rémy` → `Remy`, `ß` → `ss`, `ø` → `o`.
- Then build the component: keep ASCII alphanumeric `.` `_`; every maximal run of anything else becomes a **single** `-`; collapse runs of `-`; trim leading/trailing `-`.
- Keep normal ids lossless (`plain_name` unchanged). If the sanitization **lost information** (transliteration changed chars, a run was collapsed, or trimming happened), append a short deterministic hash (`-<8 hex of the original id>`) so `Remy` and `Rémy` never share a cache directory and long ids stay unique; cap the slug length (e.g. 80 chars) before the hash.
- Empty/dot-only components become `mount` (+hash when lossy); never `.`/`..`; never a leading `.`.
- Unit table: `Rémy` → `Remy-<hash>`, `plain_name` → `plain_name`, `a!!b` → `a-b-<hash>`, `a----b` → `a-b-<hash>`, `..` → `mount-<hash>`, empty → `mount-<hash>`, 200-char id truncated before the hash, no `--` anywhere.

# Acceptance

- `cargo test -p slipfork-image`; `sanitize` is the only owner (no duplicate slug helpers).
- Commit `[M3.122] ...`, done, no push.
