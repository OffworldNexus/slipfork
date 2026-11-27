# Log M3.156

## 2026-11-25T11:10 [conductor]

created (task) CLI fmt canonical still drops the authored header blank (HEAD red): update critical_fmt_migrate and cover the CLI crate

## 2026-11-25T11:10 [conductor]

status ready -> active

## 2026-11-25T11:12 [lead]

CANONICAL keeps the authored blank after '# forkfile: 0.1'; no other CLI fmt golden encoded blank-eating (grep clean). M3.155 gate missed it because it ran -p slipfork-forkfile + forkfile suites, never the CLI crate's critical_fmt_migrate. Verified on a pristine 6b095fa worktree: critical_fmt_migrate 3 passed, slipfork-forkfile green, fmt --check clean. Commit e5d2c39.

## 2026-11-25T11:12 [lead]

status active -> done
