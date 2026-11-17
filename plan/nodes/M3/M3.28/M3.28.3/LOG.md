# Log M3.28.3

## 2026-11-17T16:32 [planner]

created (task) DaemonControl truth: bound ingress addresses, resolved origin port, world state, and the per-world component tree

## 2026-11-17T17:53 [lead]

edited fields

## 2026-11-17T18:52 [lead]

status ready -> active

## 2026-11-17T18:52 [lead]

attempt 1/3 started

## 2026-11-17T18:52 [lead]

base f291377108292a04dfe5efe0e7627955466b889f

## 2026-11-17T18:58 [lead]

edited fields

## 2026-11-17T18:58 [lead]

lead rulings after attempt 1 (step cap, WIP on disk): (1) Wire uses canonical strings — world: String, parent: Option<String> carrying slipfork-edge registry::world_id(project, target); typed WorldId does NOT exist in slipfork-world (ARCHITECTURE:101 already says it is 0.2.0 target state) and creating it is not this node's scope. Fix ARCHITECTURE:93's contradictory claim + the dx §3 sentence in the same sweep. (2) paths widened: bin/slipforkd.rs + tests/support/mod.rs (DaemonWiring carries IngressInfo) and the two spec files for that one-line clarification.

## 2026-11-17T18:58 [lead]

attempt 2/3 started

## 2026-11-17T18:58 [lead]

base f291377108292a04dfe5efe0e7627955466b889f

## 2026-11-17T19:07 [lead]

status active -> review

## 2026-11-17T19:08 [lead]

quad round 1 (cruft): FAIL — (a) http_port() and registry::http_ingress_port() dead accessors; (b) doc claims advertise data the node does not have (WorldUpSummary.restarted join, WorldLineage as 'only source' while list()/world_processes hardcode unknown()); (c) DaemonRequest::Shutdown + DaemonControl::shutdown + serve_connection bool + AtomicBool have no production caller (test-only); (d) ARCHITECTURE 3.4 still says WorldId/parent: Option<WorldId>; (e) dx 141 duplicates the schema that ARCHITECTURE owns; (f) duplicate test fixtures; (g) roadmap node ids in production docs. Ruling: delete shutdown from .3 (no caller), .6 may re-add it only with its caller in the same commit.

## 2026-11-17T19:09 [lead]

IoC SPEC_BLIND_SPOT resolved (option A, matches ARCHITECTURE §2): the service taxonomy and the lineage record have ONE owner — crates/slipfork-engine. .3 moves pub enum ServiceKind + WorldServiceInfo + the WorldLineage family into the engine (pure data, serde) and deletes the edge duplicates (edge already depends on slipfork-engine, so no new dependency); .4 fills them from supervision records + injected clock and deletes WorldLineage::unknown(). No engine->edge mapping ladder, no second ServiceState, no service_state_wire adapter.

## 2026-11-17T19:11 [lead]

edited fields

## 2026-11-17T19:11 [lead]

attempt 3/3 started

## 2026-11-17T19:39 [lead]

status review -> done

## 2026-11-17T19:39 [lead]

quad gate round 3 closed all 8 findings (types moved to slipfork-engine/src/lineage.rs, shutdown removed, dead accessors removed, ARCHITECTURE 3.4 String wire, dx trimmed, IngressPortInvalid kind, shape assertions). Pre-commit: make check rc=0 on retry (first run hit a world-init straggler flake = M3.27's bug class), guard OK, plan OK.
