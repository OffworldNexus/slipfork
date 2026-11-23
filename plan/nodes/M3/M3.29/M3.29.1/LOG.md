# Log M3.29.1

## 2026-11-18T00:08 [planner]

created (task) Ripple the OSPP v1 spec through ARCHITECTURE, ADR-0001/0008, VISION and the DX specs; delete the obsolete wire and disk vaults

## 2026-11-18T00:09 [lead]

status ready -> active

## 2026-11-18T00:09 [lead]

attempt 1/3 started

## 2026-11-18T00:09 [lead]

base 9098b204b338c53a676d8b77052e8434fba45675

## 2026-11-18T00:55 [lead]

status active -> review

## 2026-11-18T00:56 [lead]

edited fields

## 2026-11-18T00:56 [lead]

round 1 (cruft): FAIL — BLOCKER: ARCHITECTURE + ADR-0008 restate the entire ospp-v1 wire while declaring ospp.md sole normative (three copies will drift). MAJORs: 'slipfork auth <URL>' vs 'auth login' two spellings; docs/reference/ospp.md says the spec 'is being written'; tpmd residue in Cargo.toml/pub mod tpmd/ADR-0011 (code sweep is .29.2/.29.3, but ADR-0011's justification is yours); dumps described two ways across ADR-0009 + world-base-image (paths widened); ADR-0005 names the deleted OsppSecretProvider (path widened); duplicate tombstones; 'hpke' named as a dep that .29.2 must actually declare.

## 2026-11-18T00:57 [lead]

round 1 (functional): PASS — old-wire-gone, two-tier-boundary, hpke-documented, remote-verbatim, spec byte-identical, guard/plan/zensical green. NODE acceptance updated to two-tier-boundary (the disk-vault-gone grep was superseded by the owner ruling). MINORs routed to the architect: claims derivation must list components everywhere; SubscribeRequest must note encryption is appended per subscription; PairingApproval.public_key is not a wire field; auth_pair should carry a zeroizing handle type; ADR-0005/ADR-0011 tpmd residue is now in .29.1 paths.

## 2026-11-18T01:26 [lead]

status review -> done

## 2026-11-18T01:26 [lead]

quad: functional PASS, ioc resolved by ruling (SecretVault port, RemoteResourceOffer deleted, RemoteStream deleted -> daemon streams SetupBaseR into zstd -d|pg_restore, WorldEngine::update_service_env, one OsppEndpoint, auth_failed code, ADR-0011 tpmd superseded), legality PASS (no drift in frame names/info/aad/encodings/EOF), cruft FAIL items closed (one home per wire, auth verb single spelling deferred to .29.3's rename, tpmd target-state labelled, dumps one story). Spec byte-identical; guard/plan/zensical green.
