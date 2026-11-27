---
id: M3.117
title: "Two-level boot progress: outer [i/n] plan rows, indented inner Dockerfile steps, level-aware bar"
kind: task
status: done
owner: implementer
priority: 3
depends_on: [M3.112]
paths:
  - crates/slipfork/src/boot.rs
  - "crates/slipfork/src/boot/**"
  - "crates/slipfork/src/ui/**"
  - crates/slipfork/src/world/runtime/ready.rs
  - "crates/slipfork-world/src/bin/world_init/pod/launch.rs"
  - crates/slipfork/tests/critical_status_live.rs
  - crates/slipfork/tests/critical_world_journey.rs
  - crates/slipfork/tests/status_surface.rs
attempts: 3
max_attempts: 3
escalation: expert
created: "2026-11-24T16:29"
updated: "2026-11-24T18:10"
---

# Problem (owner)
`[i/n]` is used for both the outer `up` plan (image/setup/service/hook) and the inner Dockerfile steps, with different denominators and no nesting; a cached image row has no counter while `✓ [3/3] start web` does, so `[3/3]` reads as the image's.

# Design (Opus, option A — two-level tree; v3.2 role map unchanged)
```
✓ [1/3] image  api      cached        8ms              ← cached: no inner rows
→ [1/3] image  api                                     ← fresh build opens the image
  ✓ 1/6 FROM  python:3.13-slim  <1ms                    ← inner: indent 2, `n/N` dim, no brackets, no target
  ✓ 2/6 COPY  . /app  <1ms
  ⠸ 3/6 RUN   sh -c "…"  1.2s
    │ fetching installer                                ← gutter indent 4 under the inner row
✓ [1/3] image  api      built 6 steps  12.3s           ← replaces the old `✓ image api (N steps, Xs)` end row
• [2/3] setup  uv sync  up to date    <1ms
✓ [3/3] start  web      @uv run blastit dev --port 8000  195ms
✓ Ready -> http://test1.localhost
```
- Outer rows: col 0, glyph + `[i/n]` (dim) + kind (magenta) + target (bold) + detail/command (plain) + elapsed (dim); EVERY outer row carries `[i/n]` (image, setup, start, hook). Columns padded to the plan's max visible widths.
- Inner rows: indent 2, glyph + `n/N` (dim, no brackets) + instruction (magenta) + args (plain) + elapsed (dim); no target (the parent row names it). Output gutter `│`/`┃` at indent 4. Settled inner steps stay one row each; the failing inner step stays expanded.
- Failure (build): `  ✗ 4/6 RUN  sh -c "…"` + its full gutter output, then col-0 verdict `✗ RUN api failed (exit 1)` + `  next: slipfork logs -r <id>`. Service failure: outer row `✗ [3/3] start web …` + its boot output gutter at indent 2, verdict `✗ start web failed (exit 7)` + next.
- Bottom bar (TTY): percent = (outer_done + inner_done/inner_total for the building image) / outer_total — no double count; label `[1/3] image api · step 3/6 RUN · 3.1s` while a build runs, `[2/3] setup uv sync · …` otherwise.
- Append-only (NO_COLOR / non-TTY): same rows and indents, `→` when started, re-anchor rows keep their level (`→ [1/3] image api` / `  → 3/6 RUN …`), zero `\x1b[`. `--json` unchanged (frames already carry outer `step/total` and inner `image/index/total`).
- No `palette.kind`/cyan; spinner neutral; one hue per role.

# Acceptance
- Golden byte test (styled + NO_COLOR) pins: cached outer image row, outer start row, inner running row, inner settled row, image `built N steps` row, verdict, next.
- Live PTY (critical_status_live, reuse its world): cached image shows no inner rows and `[1/n] image`; fresh build shows indented `n/N` inner rows under `[1/n] image` and never an inner `[i/n]`; build failure and service failure each end with one verdict + one `next:`; the bar label switches level during a build.
- All boot/** files <= 500 (boot.rs is at 500: move code out); fmt, clippy -D warnings, `cargo test -p slipfork --bins`; lead runs VM live + PTY capture.
