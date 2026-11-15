---
id: M1.7
title: "Repair make bench VCS root setup, regenerate live PoC report & add CHANGELOG.md (0.1.0)"
kind: task
status: done
owner: implementer
priority: 1
depends_on: [M1.6]
paths: [poc/run_benchmark.py, "docs/research/m1-poc-benchmark.md", docs/index.md, README.md, CHANGELOG.md]
attempts: 1
max_attempts: 3
escalation: none
created: "2026-11-15T02:16"
updated: "2026-11-15T02:27"
---

# Goal

Fix the Milestone 1 gate BLOCKER (`make bench` failing with exit code 2 on `d0b11e0`) and ratify `CHANGELOG.md` for `0.1.0`:

1. **Hermetic VCS Repository in `poc/run_benchmark.py`:**
   - `M1.6` enforced `ADR-0005` §3 canonical VCS `ProjectSlug` discovery in `crates/slipfork/src/main.rs` (`resolve_snow_file` -> `project_slug_for` -> `resolve_vcs`), which fails closed outside a `git` or `jj` repository.
   - `poc/run_benchmark.py`'s `setup_sample_project(proj_dir)` creates `~/.cache/slipfork-m1-bench/acme-shop` as a plain directory without `git init`, causing `slipfork snow resolve --file .../acme-shop/snow.yml` (`poc/run_benchmark.py:742`) and subsequent `slipfork exec --snow-file` calls to exit `1`.
   - Initialize `proj_dir` (`acme-shop`) with a real hermetic `git init -q` inside `setup_sample_project`, and pass a clean subprocess environment (`GIT_CEILING_DIRECTORIES=str(bench_root)`, `XDG_DATA_HOME=str(bench_root / "xdg-data")`, stripping any ambient `GIT_DIR`/`GIT_WORK_TREE`/`GIT_INDEX_FILE`/`GIT_COMMON_DIR`) to `run_cmd` and `subprocess.Popen` so `make bench` never writes `acme-shop.json` into `~/.local/share/slipfork/vaults/` and never leaks outer Git env vars.
2. **Re-measure Live PoC & Sync Published Numbers:**
   - Update the rendered report date in `poc/run_benchmark.py` (`2026-11-15`) and run `make bench` end-to-end against native PostgreSQL 18.7 + XFS `ioctl(FICLONE)` + isolated `netns` + headless Chromium (`Playwright`) to regenerate `docs/research/m1-poc-benchmark.md`.
   - Update any cited benchmark figures in `README.md` and `docs/index.md` if needed so published docs stay consistent with `docs/research/m1-poc-benchmark.md`.
3. **Ratify `CHANGELOG.md` (`0.1.0`):**
   - Create root `CHANGELOG.md` with `## 0.1.0 (2026-11-15)` accurately recording what ships in `0.1.0` (`slipfork-world`, `slipfork-snow`, `slipfork-dag`, `slipfork-engine`, `slipfork` CLI, `@slipfork/opencode` V2 plugin, `ADR-0001..0006` architecture, and the live PostgreSQL 18 + XFS `FICLONE` + `netns` + Playwright benchmark).

# Acceptance

- [ ] `rm -f ~/.local/share/slipfork/vaults/acme-shop.json && make bench && test ! -e ~/.local/share/slipfork/vaults/acme-shop.json` exits 0 and regenerates `docs/research/m1-poc-benchmark.md`.
- [ ] `test -f CHANGELOG.md && rg -q '^## 0\.1\.0 \(2026-11-15\)' CHANGELOG.md` exits 0.
- [ ] `make check` exits 0 (`plan.py validate`, `guard.py`, `ruff`, `pytest`, `bun test`, `cargo fmt/clippy/doc/test`, `zensical build --strict`, `check_docs_honesty.py`).
