# Follow-ups for M2.14 (non-blocking, from the M2.13 Quad Gate)

These were accepted as non-blocking by `reviewer`, `review-legality`, `review-ioc`, and
`review-cruft`. None changes current behaviour; collect them when M2.14 extends the OSPP surface.

1. **Wire-frame enum dedupe.** `client.rs` has separate `ConnectFrame`, `PairingFrame`,
   `WireReject`, and `WirePairingReject` enums even though `CHALLENGE` and `REJECT` share a shape.
   M2.14's capability dispatch should introduce one `ServerFrame` enum with a shared `Reject` body,
   keeping the typed reasons separate only where the reason sets differ.

2. **`is_lower_hex_digit` is copied three times** (`client.rs`, `device_key.rs`, `pairing.rs`).
   Hoist one crate-private `hex` helper module (`is_lower_hex_digit`, `encode_hex_into`,
   `hex_fixed`, `hex_str`) and delete the copies. The helpers added in the gate fix are the natural
   seed for it.

3. **`Deadline` phase/budget reporting.** `OsppConnection::read_frame_text` always reports
   `self.timeouts.handshake` in `OsppError::Deadline`, even after `set_io_timeout()` switches the
   socket to the post-handshake budget. Report the budget actually armed for the phase (handshake vs
   io) so an operator diagnostic does not name the wrong number.

4. **Hoist the `tpmd-proto` dev-dependency pin.** `crates/slipfork-ospp/Cargo.toml` pins
   `tpmd-proto` at the same git revision as `tpmd-client`/`tpmd-ipc`, because M2.13's path lock did
   not include the workspace root. Add `tpmd-proto` to `[workspace.dependencies]` and switch the
   dev-dependency to `.workspace = true` alongside the other two pinned crates.

5. **Purity-grep fragility.** The acceptance checks are literal `rg "std::env"` / `rg "der::"`
   over `src/`. `der::` also matches ordinary identifiers such as `Builder::`, so a harmless
   constructor name forced a `TokioRt` alias. Replace these with semantic checks (for example, a
   grep for `std::env::` and for DER/ASN.1 imports or `from_der`/`to_der` calls) before the surface
   grows further.
