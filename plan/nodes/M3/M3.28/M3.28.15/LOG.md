# Log M3.28.15

## 2026-11-17T17:40 [planner]

created (task) Delete the unimplemented tunnel-sharing surface (slipfork share, TunnelRegistrar, WorldOrigin::Shared, support tier)

## 2026-11-17T18:22 [lead]

edited fields

## 2026-11-17T18:22 [lead]

added docs/decisions/ADR-0005-*.md to paths: the accepted ADR still describes WorldOrigin::{Local,Shared}/slipfork share/TunnelRegistrar in the present tense and must be annotated superseded-for-0.1.0 by ADR-0011 in the same sweep

## 2026-11-18T01:37 [lead]

edited fields

## 2026-11-18T03:16 [lead]

edited fields

## 2026-11-18T03:16 [lead]

status ready -> active

## 2026-11-18T03:16 [lead]

attempt 1/3 started

## 2026-11-18T03:16 [lead]

base d24a6c584dc5ea31c58f9980ca65c2b34c958a63

## 2026-11-18T03:20 [lead]

edited fields

## 2026-11-18T03:20 [lead]

attempt 2/5 started

## 2026-11-18T03:20 [lead]

attempt 1 cap-out mid-sweep (tree RED). DONE and kept: world crate (TunnelRegistrar family + TunnelLease/Error/Request + Shared origin deleted; WorldOrigin collapsed to struct {url} with local() as sole ctor and try_from validation; critical_world adapted), edge (tunnel.rs deleted, registry share/shared_hosts/RouteDecision/308-421 removed, registry::new(engine, ingress_port), origin() derives the sole origin, ingress redirect/misdirected deleted, daemon wire + client + server share removed, three EdgeError variants deleted), slipfork daemon.rs (imports, registry ctor sites, DaemonService share/unshare deleted, fixture updated). REMAINING: cli.rs Share variant, main.rs dispatch+fn share, slipforkd.rs doc line, dag support IntegrationCategory::Tunnel, critical_cli share test, critical_edge share/308/421 tests + helpers, docs rows, ADR-0005 annotation, then fmt/acceptance/gates/RESULT. Two out-of-glob edits disclosed: slipfork-world/Cargo.toml description, slipfork-snow/tests/critical_snow.rs.

## 2026-11-18T03:36 [lead]

edited fields

## 2026-11-18T03:36 [lead]

status active -> review

## 2026-11-18T03:43 [lead]

status review -> done

## 2026-11-18T03:43 [lead]

landed together with its sibling: both nodes edit cli.rs/main.rs/lib.rs/critical_cli.rs, and splitting would require hand-editing code. make check GREEN (44+ suites), check_stub_adapters exit 0, docs honesty 11 docs.
