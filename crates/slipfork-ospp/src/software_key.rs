//! `SoftwareP256Signer`: `DeviceKeySigner` fallback backed by an injected `DeviceKeyStore` and
//! `EntropySource`.
//!
//! This module is the single home of the software-key arc: the loaded `SoftwareP256Key` (a private
//! 32-byte scalar, zeroized on `Drop`, redacted `Debug`) and its adapter live here, while
//! [`device_key`](crate::device_key) owns only the port and its `KeyId` / `DeviceSignature` newtypes.
