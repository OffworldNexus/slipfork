//! `DeviceKeySigner` port with its validated `KeyId` (`p256:<66 lowercase hex>`) and
//! `DeviceSignature` (raw 64-byte `r || s`) newtypes, plus the injected `DeviceKeyStore` that yields
//! the private key material the [`software_key`](crate::software_key) adapter signs with.
