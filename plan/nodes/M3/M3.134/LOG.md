# Log M3.134

## 2026-11-24T22:52 [conductor]

created (epic) ForkLedger: one state DB for worlds and forks, flat envs with lineage, deterministic convergence

## 2026-11-24T23:06 [conductor]

Owner override on the store layer: use sea-orm + sea-orm-migration (entities + migrations/ + Migrator at daemon start), NOT raw rusqlite. specs/03 §7 is being revised accordingly. Any plan text mentioning bundled rusqlite is superseded by §7.

## 2026-11-25T12:31 [conductor]

Epic delivered by M3.138 (ledger), M3.139 (flat forks + lineage), M3.140 (daemon cutover + converge), M3.141 (ps lineage/broken/offline), M3.131 (init hygiene), M3.157 (status-live contract) — closing.

## 2026-11-25T12:31 [conductor]

status ready -> done
