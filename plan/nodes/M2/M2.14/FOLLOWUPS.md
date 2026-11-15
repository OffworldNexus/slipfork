# Follow-ups for M2.14 (non-blocking)

These are clean-up opportunities observed while landing the capability, VerifiedDump, and
reference-server work. None changes current behaviour; collect them in the next OSPP-adjacent node.

1. **One canonical `scope_label`.** `client.rs::scope_label`, `secret_provider.rs::scope`, and the
   inline `format!("{project}:{mode}")` in `DumpStreamer::fetch` build the same
   `"<project>:<mode>"` diagnostic label three times. Hoist one helper (or a small `OsppScope`
   newtype) so capability errors cannot drift in format.
2. **One `exchange(...)` helper.** `env_bundle`, `ensure_secret`, `resources_describe`, and
   `dump.begin` each repeat the same request-id/send/read/`into_refusal` boilerplate. A private
   `OsppClient::exchange(capability, request, expect: fn(ServerFrame, u32) -> Option<T>)` would
   remove four near-identical error ladders without changing the wire.
3. **Pairing/server hex predicates.** `pairing.rs::from_hex`, `reference_server::decode_32`/
   `parse_verifying_key`/`signature_verifies`, and `hex::is_sha256_hex` each re-implement
   "64/66/128 lowercase-hex characters". Add `hex::decode_fixed::<N>` (crate-private) and reuse it
   on both sides so the test double's alphabet matches the spec in one place.
4. **`CAPABILITY_*` table usage.** `protocol.rs` exports the five capability names, but the
   reference server still writes `"env.bundle"`, `"secrets.ensure"`, `"resources.describe"`,
   `"dump.begin"`, and `"capability.error"` as literals. Import the constants (plus a
   `CAPABILITY_ERROR` constant) so a typo in the harness cannot silently pass.
5. **`send_reject` / `send_pair_reject` dedupe.** The reference server has two functions that build
   the same `{"type":"reject","reason":{...}}` envelope with different reason enums. One generic
   `send_reject(socket, reason: serde_json::Value)` caller-side conversion would remove the copy.
6. **Drop the unused `getrandom` dependency.** `crates/slipfork-ospp/Cargo.toml` still lists
   `getrandom.workspace = true`; nothing in `src/`/`tests/` names it (entropy is injected through
   `slipfork_snow::EntropySource`). Remove it once the workspace dependency audit allows.
