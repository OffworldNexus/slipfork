//! `TpmdDeviceKeySigner`: the `DeviceKeySigner` adapter that will talk to `tpmd` through the pinned
//! `tpmd-client` / `tpmd-ipc` crates, using a private, instance-scoped `current_thread` `tokio`
//! runtime and injected `KeyStorePaths`.
