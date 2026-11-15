//! Non-template `slipforkd.socket` / `slipforkd.service` renderer over a typed `UnitSpec`.
//!
//! Rendering is pure (zero filesystem access): explicit `User=<developer-user>` resolved at the CLI
//! composition boundary, fixed listener order matching the activation fd order, empty
//! `CapabilityBoundingSet=`, and `ReadWritePaths=` for the explicit state and runtime directories.
