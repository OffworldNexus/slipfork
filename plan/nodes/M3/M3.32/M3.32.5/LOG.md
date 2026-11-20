# Log M3.32.5

## 2026-11-19T13:02 [planner]

created (task) Forkfile visual docs, CHANGELOG & multi-service (api + front + postgres) support/demo

## 2026-11-19T13:02 [lead]

edited fields

## 2026-11-19T13:03 [lead]

edited fields

## 2026-11-19T19:59 [lead]

note (from M3.32.3.1 Triad): published docs must show the new ingress shape — a <label>.* subdomain route (e.g. mail.*) and that <world>.localhost:<any port> (ssh -L) routes identically to :80; docs/reference/manifest.md:25 still only shows prefix /.

## 2026-11-19T22:20 [lead]

status ready -> active

## 2026-11-19T22:20 [lead]

attempt 1/3 started

## 2026-11-19T22:20 [lead]

base 5eef927e76e744ee559c0bdac205065fa32b3f2c

## 2026-11-19T22:22 [lead]

edited fields

## 2026-11-19T22:22 [lead]

attempt 2/4 started

## 2026-11-19T22:22 [lead]

attempt 2: demo done+tested; docs remain. Acceptance grep narrowed: snow.yml allowed only in migrate-input docs and docs/design+decisions blueprints

## 2026-11-19T22:23 [writer]

Demo rehearsal (VM-deferred; host is not Btrfs): support/demo ships api ./api (port 8000, route /api, volume .venv, task migrate) + front ./front (port 5173, route /) + postgres pg { database demo }. Expected: 'slipfork up' -> base snapshot (api, front, postgres:18), 'api setup & pre-deploy [2/2]', '● demo http://demo.localhost'; curl -H 'Host: demo.localhost' http://127.0.0.1/api -> 200; 'slipfork run migrate' -> 'demo-api: migrations applied'; 'slipfork fork demo-a' -> '● demo-a http://demo-a.demo.localhost'. Parse proof: cargo test -p slipfork-forkfile --test demo_forkfile.

## 2026-11-19T22:24 [lead]

status active -> review

## 2026-11-19T22:24 [lead]

attempt 3/4 started

## 2026-11-19T22:24 [lead]

Triad fix pass base 5eef927e76e744ee559c0bdac205065fa32b3f2c

## 2026-11-19T22:25 [lead]

status review -> done
