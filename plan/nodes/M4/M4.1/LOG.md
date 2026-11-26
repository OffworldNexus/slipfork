# Log M4.1

## 2026-11-16T17:25 [strategist]

created (task) Isolated: concrete WeavedRegistrar IPC adapter and declared support promotion (gated on H-006)

## 2026-11-16T17:25 [strategist]

status ready -> blocked: Awaiting H-006 weaved IPC spec

## 2026-11-17T12:45 [lead]

NOTE: the 'gated on H-006' reference predates H-006's actual filing; H-006 is now the M3.19 AppArmor direction-challenge, not the WeavedRegistrar request. Re-file the WeavedRegistrar missing-input when M4 starts.

## 2026-11-17T17:40 [lead]

RE-ENTRY GATE for tunnel sharing (owner ruling, M3.28.15): 'slipfork share' was deleted from 0.1.0 as vaporware (UnavailableTunnelRegistrar::register always returned DaemonUnavailable). Bringing it back requires ALL of: (1) a real weaved IPC spec (docs/design/) with the frame/reconnect contract, (2) a concrete WeavedRegistrar adapter implementing the restored capability trait, (3) a restored shared-trait conformance suite (in-memory double + live harness), (4) the shared-host redirect/Upgrade handling that was deleted with WorldOrigin::Shared, (5) a support-registry tier backed by a recipe page and a protected critical test. Until H-006 is answered this node stays blocked.

## 2026-11-25T19:21 [strategist]

superseded: slipfork-edge and UnavailableTunnelRegistrar were deleted in M3.28.15/M3.37 clean-sheet rebuild

## 2026-11-25T19:21 [strategist]

status blocked -> superseded
