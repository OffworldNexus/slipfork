# Log M3.29.3

## 2026-11-18T00:08 [planner]

created (task) Delete the disk secret vault: in-memory session secrets, live slipforkd OSPP subscriptions, streaming HPKE dump restore and auth pairing

## 2026-11-18T00:30 [lead]

RESCOPE (owner clarification 2026-11-18): do NOT delete FileVaultProvider/vaults.rs/'slipfork secret'. Scope them to LOCAL credentials only (locally-provisioned backing-service passwords that must survive restarts, local-mode !!, locally-managed vars/secrets) and use the vault contents + local resource/domain declarations to derive OSPP claims (ids only). Add the hard invariant: no remote OSPP secret is ever persisted — assert it with a byte scan of <state_dir> after a full boot with a known remote secret. In-memory session secrets remain for local-only runs without a vault entry.

## 2026-11-18T00:30 [lead]

edited fields

## 2026-11-18T00:55 [lead]

edited fields

## 2026-11-18T01:26 [lead]

edited fields
