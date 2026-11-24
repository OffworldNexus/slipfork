# Log M3.115

## 2026-11-24T16:19 [conductor]

created (task) up must fingerprint Dockerfiles/context so edits rebuild the image

## 2026-11-24T16:19 [conductor]

status ready -> ready

## 2026-11-24T16:47 [conductor]

edited fields

## 2026-11-24T16:47 [conductor]

Owner: up/restart do not converge after a Forkfile or Dockerfile change. up_plan compares a fingerprint of the Forkfile+features only (no Dockerfile/context); restart (daemon/lifecycle/core.rs:198) never re-evaluates anything and restarts the old world. Requirement: both verbs run the same staleness check (Forkfile + every image's Dockerfile + the canonical context digest) and reconcile (stop -> rebuild changed stages -> boot) before any restart; unchanged = restart services as today. Tests: edit Forkfile (add/rename a service) and Dockerfile separately, restart/up converge; unchanged restart still restarts.

## 2026-11-24T16:54 [conductor]

OWNER EVIDENCE (dev active): up --json => {"event":"ready","nodes":[],"elapsed_ms":0} (Idle, no reconcile); slipfork exec web -- pwd => /; /project/hello.py = NEW content, /code/hello.py and ./hello.py = OLD content; Dockerfile now has WORKDIR /code line 11 but the image predates it. So the dev bind works; the image stamp/up fingerprint is stale. Fix must make the sealed stamp and up_plan both see Dockerfile+context changes (not just Forkfile) and never return Idle over a stale image; after a rebuild the new WORKDIR is detected by M3.118 and the live context mounted there.

## 2026-11-24T17:47 [conductor]

REPRODUCED by the M3.117 lead on the VM: build with RUN echo one, edit to RUN echo two; exit 9; up prints nothing; after down, up shows image cached and the failing command never runs. This is the image build cache, not the renderer. M3.121 was filed as a duplicate and is superseded here. Must go before the next release.

## 2026-11-24T17:51 [conductor]

OWNER: no leftover spaghetti. Do NOT add a second cache/fingerprint layer. First fix the existing image state key / context digest so a Dockerfile/context edit changes the key; if any stopgap is unavoidable, mark it TEMPORARY in the node log and delete it in M3.120 slice 4. No parallel cache paths, no legacy aliases; the Triad arch reviewer must confirm one owner for the decision.

## 2026-11-24T18:10 [conductor]

status ready -> active

## 2026-11-24T18:13 [lead]

status active -> active

## 2026-11-24T18:13 [lead]

attempt 1/3 started

## 2026-11-24T18:13 [lead]

base e374e5d2b1eb5b760d3da37b4530bffbbd9e61a4

## 2026-11-24T19:02 [lead]

edited fields

## 2026-11-24T19:03 [lead]

fix: canonical build-input digest (Dockerfile bytes + .dockerignore context via BuildContext::for_walk) folded into plan_epoch/trusted_stamp and the daemon fingerprint; ImageNode::paths + fingerprint computed outside every core() lock; restart shares world_stale and reconciles on a changed head; reconcile reply tagged so the CLI never prints a false 'Restarted (0 pods)'. Live VM: failing RUN edit after down rebuilt and failed (exit 9), repaired RUN/context edits rebuilt, Forkfile add-service restart converged to 2 pods, unchanged up idle, unchanged restart in place; make check, critical_world_journey, Triad clean (arch/tests/ux).
