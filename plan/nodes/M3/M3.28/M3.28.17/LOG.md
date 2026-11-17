# Log M3.28.17

## 2026-11-17T19:46 [planner]

created (task) Restart vs up: one boot pipeline, truthful WorldUpSummary.changed and restarted ids

## 2026-11-17T21:00 [lead]

status ready -> active

## 2026-11-17T21:00 [lead]

attempt 1/3 started

## 2026-11-17T21:00 [lead]

base ae71c454a0bb3c5c9fd594b627ff020cfd0daa15

## 2026-11-17T21:18 [lead]

status active -> review

## 2026-11-17T21:20 [lead]

quad round 1 (cruft): FAIL — (a) restarted_service_ids has two owners (LiveWorld bookkeeping vs engine rows) + guards for states that cannot occur, and a service exiting before the summary turns a good boot into an error; (b) boot_verb's Option and the caller re-checks are unreachable; (c) refresh_world's doc ('snow.yml is re-read so an edited component/env/hook applies') outruns the code — live.resources, the registry ingress table and live.rootfs are not refreshed, so summary reports pre-edit resources and the ingress caveat lives only in RESULT.md; (d) WorldState::Stopped is unreachable (down releases the record); (e) Restart(WorldSelector) diverges from the normative RestartRequest newtype. Ruling: one owner for restarted ids; refresh everything or narrow the doc; keep WorldSelector and fix ARCHITECTURE to match the code; M3.28.8 must serialize WorldUpSummary instead of hand-building a 3-key subset (added to its invariants).

## 2026-11-17T21:20 [lead]

edited fields

## 2026-11-17T21:20 [lead]

quad round 1 (legality): FAIL — MAJOR: reboot never re-runs the registry attach step, and the registry has no update verb (re-attach fails WorldAlreadyRegistered), so an edited ingresses:/domains:/internal_ports is NOT applied while changed:true is returned — new prefixes 404, renamed components keep stale routes 502. RULING: fix it (release+attach inside reboot, one registry path), do not document a down+up caveat: 'restart applies edits' is the locked contract. MINORS: restarted join must require ServiceState::Running; a world with no components/services must not report changed:true; record_booted must insert before summarising; component_envs must fail closed instead of starting with an empty env; add a real restart-after-edit test (hooks fired, restarted ids, worktree+CoW kept).

## 2026-11-17T21:21 [lead]

quad round 1 (ioc): FAIL — SPEC_BLIND_SPOT resolved by the lead, not escalated: restart MUST re-derive every boot-scope fact (resources plan, origin/coords, registry routes, component env/hooks) rather than overloading LiveWorld.snow against boot-time values. 'restart applies edits' is the locked contract, so the answer is one resolver shared by boot and reboot, not a frozen-vs-current split. Also: reboot stops services from the caller-side LiveWorld.services list while world_processes is specified as the authoritative supervision record -> add WorldEngine::stop_world_services and derive restarted from engine rows; extract the env/enabled_components resolution so boot and reboot cannot drift; ARCHITECTURE's RestartRequest newtype loses to the shipped WorldSelector (one selector shape).

## 2026-11-17T21:28 [lead]

attempt 2/3 started

## 2026-11-17T21:28 [lead]

quad round 1 (functional): FAIL — MAJOR: up's Rerun path silently drops req.dump/resources/host_caches (up --refresh-dump on a held Failed world exits 0 having refreshed nothing); MAJOR: nothing executes restart/reboot in any test — the core claim (hooks re-fire after an edit, restarted == spawned ids, second up spawns nothing) is unproven and the edge test is a fixture tautology. MINORS: boot_verb Restart+Stopped=>Rerun contradicts dx 4; mem::take drops handles when one stop fails (same as ioc); world_next_commands advertises 'slipfork restart' before M3.28.7 lands (wire-only, check_docs_honesty cannot see it).

## 2026-11-17T21:48 [lead]

attempt 2: rulings 1-4 applied (one resolve_world used by boot AND reboot; registry.replace_world re-attaches routes/domains; stop over &live.services with clear-after-success; BootPolicy threading so up's Rerun honours dump/resources flags; restarted requires Running; changed = spawned>=1 or hook ran; record before summarise; missing env fails closed; Restart+Stopped refuses; ARCHITECTURE drops RestartRequest). New real test restart_applies_an_edited_snow_file_and_a_second_up_touches_nothing (first execution of reboot). REMAINING GAP (accepted, reassigned): proving restarted == real spawn ids / a hook re-fire needs ServiceHandle::mint to leave pub(crate) or the heavy harness, both outside this node; M3.28.13 now carries the end-to-end proof (edited hook + app re-fire on the VM) as a mandated evidence line.

## 2026-11-17T21:49 [lead]

edited fields

## 2026-11-17T21:49 [lead]

attempt 3/4 started

## 2026-11-17T21:49 [lead]

round 2 (ioc) FAIL: two BLOCKERs left, both about ownership of 'what this world runs' — teardown is still a caller-side loop (no WorldEngine::stop_world_services) and restarted still originates from live.services with world_processes only validating it. Paths widened to slipfork-engine so the port can be added properly; enabled-component resolution duplicated between resolve_world and start_world (carry ResolvedComponent).

## 2026-11-17T22:13 [lead]

status review -> done

## 2026-11-17T22:13 [lead]

pre-commit: make check rc=0 (load 10), engine lib 66, slipfork lib restart 4, critical_edge 40, clippy clean, fmt-ok, critical_guard OK, guard OK.
