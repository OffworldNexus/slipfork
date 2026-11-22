# Log M3.50.6

## 2026-11-22T12:41 [planner]

created (task) Rebuild .deb + live 165.232.86.174 proof: real-PTY shell, <1s 8-component fork

## 2026-11-22T12:42 [lead]

edited fields

## 2026-11-22T13:13 [lead]

edited fields

## 2026-11-22T13:21 [lead]

edited fields

## 2026-11-22T13:23 [lead]

edited fields

## 2026-11-22T13:23 [lead]

OWNER REALITY CHECK (mandatory): this gate must WALK EVERY SHIPPED VERB live over a REAL PTY in /home/slipfork/demo on 165.232.86.174 and inspect actual rendered output (not --json, not substring greps): slipfork, ps (demo + .forks/feat-auth), doctor, fork tralala (<1.0s, DECSTBM row H, 1 line/task, cached), curl demo/mail/assets(200)/uploads(403 anon,200 SigV4), drop (dirty refusal + -f), Forkfile edit in a temp fork (remove service -> ps orphaned -> up reaps; break Dockerfile -> up prints exact build failure -> logs <image> shows it), shell api (bash,/dev/pts,0 can't access tty), shell backend, shell pg + shell app_db (PTY psql AND piped SELECT 1), shell cache (PING->PONG), shell uploads (1-line refusal), exec api -- env + exit-code propagation, logs / logs api worker / --since 2m / --since 10m --until 1m -t / -a / -f across restart api, env api + --reveal on TTY, secret list|set|unset, fmt --check, open, completions bash|zsh|fish. Fix ANY ugly/hanging/escape-leaking/first-touch failure AND lock it with a real openpty/script E2E suite added to LIVE_SUITES. Rename support/demo database to app_db so 'slipfork shell app_db' is a real declared-database target (verify seed/api).

## 2026-11-22T13:27 [lead]

edited fields

## 2026-11-22T13:29 [lead]

OWNER LOG CASE: prove historical logs replay while DOWN and follow across down->up. Real-PTY steps: up -> down -> logs / logs api / logs --since 5m / logs -a all replay history; start logs -f while down, then up, and verify new lines arrive. Also verify inside a stopped fork (~/demo/.forks/feat-auth after down) logs reads <store>/logs/feat-auth--demo/*.log (not bare feat-auth).

## 2026-11-22T15:54 [lead]

edited fields

## 2026-11-22T15:58 [lead]

edited fields

## 2026-11-22T15:58 [lead]

status ready -> active

## 2026-11-22T15:58 [lead]

attempt 1/3 started

## 2026-11-22T15:58 [lead]

base 3df87cb2d19bf2601b9a53e92ef35e34477751e6; ROOT CAUSE CONFIRMED: unsynced PGDATA at snapshot time (app_db CREATE not durable) -> fork pg panic; host 'sync' before fork makes it pass. Implement a cheap pre-fork syncfs flush instead of SQL CHECKPOINT.
