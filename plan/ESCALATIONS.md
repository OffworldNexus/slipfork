# Escalations to the expensive model

Every call to an expensive-tier agent (`strategist`, `architect`, `expert`) is recorded here before spawning it, via `uv run --no-project tools/plan.py --agent <caller> escalate <node> --to <agent> --reason "..."`.

| time | node | to | by | reason |
|---|---|---|---|---|
| 2026-11-14T23:16 | M1.6 | architect | conductor | Integrate owner M1 design decisions (.localhost daemon, weaved IPC, git/jj remote, language-agnostic RunnerProtocol) |
| 2026-11-15T00:26 | M1.6 | architect | lead | Quad gate attempt 2 SPEC_BLIND_SPOT: TunnelLease ownership (WorldRegistry/daemon vs CLI), slipforkd release_tunnel IPC port, and WorldTarget/ProjectSlug single rule-table + project identity type ownership |
| 2026-11-15T00:42 | M1.6 | expert | lead | Quad gate attempt 3: legality BLOCKERs (plugin --snow-file regression + unactionable error), ioc MAJOR (slipfork.toml dual ownership [test].after_suite vs DagConfig), jj/git root guessing, missing crates/slipfork e2e slug tests; attempts exhausted |
| 2026-11-15T01:03 | M1.6 | architect | lead | Final gate legality BLOCKER: git submodule/separate-git-dir root misidentification (fix spec) + jj canonical root unverifiable without owner-approved .jj/repo contract; rule fail-closed+defer vs documented pointer read |
| 2026-11-15T02:01 | M1 | strategist | conductor | milestone gate |
