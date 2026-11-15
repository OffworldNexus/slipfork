# Log M2.16

## 2026-11-15T16:40 [planner]

created (task) CLI part 1: composition root, slipforkd binary, up/down/list/prune/secret/share/daemon unit

## 2026-11-15T18:39 [planner]

edited fields

## 2026-11-15T18:39 [lead]

edited fields

## 2026-11-15T18:51 [lead]

edited fields

## 2026-11-15T19:34 [lead]

edited fields

## 2026-11-15T19:36 [lead]

edited fields

## 2026-11-15T20:53 [lead]

edited fields

## 2026-11-15T20:53 [lead]

status ready -> active

## 2026-11-15T20:53 [lead]

attempt 1/3 started

## 2026-11-15T20:53 [lead]

base 0677c52045e7ac7c1b10262fad495bbdd8dd167a

## 2026-11-15T21:03 [lead]

attempt 2/3 started

## 2026-11-15T21:03 [lead]

edited fields

## 2026-11-15T21:03 [lead]

attempt 2 path widening (lead decisions): SecretProvider gains remove/keys (secret unset/list), ResourcesSpec gains the per-invocation Local override, ExecEnv gains a child-env iterator (thin CLI exec). No fail-closed holes in the shipped CLI.

## 2026-11-15T21:03 [lead]

edited fields

## 2026-11-15T21:11 [lead]

edited fields

## 2026-11-15T21:11 [lead]

attempt 3/5 started

## 2026-11-15T21:19 [lead]

status active -> review

## 2026-11-15T21:23 [lead]

edited fields

## 2026-11-15T21:23 [lead]

attempt 4/5 started

## 2026-11-15T21:23 [lead]

attempt 4 rulings: (a) inject ProjectVaults (project->Arc<dyn SecretProvider>) built in slipforkd, DomainService holds it and types collaborators as &dyn SecretProvider; (b) split MutableSecretStore (remove/keys) out of SecretProvider so no default-Unsupported port methods; (c) snow resolve becomes DaemonControl::snow_resolve (no CLI vault); (d) docs stop claiming the OpenCode plugin is wired (M2.18 owns the --snow-file/--workspace fix); (e) auto-spawn test must assert kill + refused connect.

## 2026-11-15T21:32 [lead]

attempt 5/5 started
