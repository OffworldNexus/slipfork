//! `DaemonControl` port, its `0600` `<runtime_dir>/daemon.sock` JSON-line server, and the
//! `DaemonControlClient` the thin CLI drives.
//!
//! Frames are bounded, every `cwd`-sensitive request carries an explicit `cwd` so the daemon never
//! resolves its own working directory, and only the `exec_target` reply may carry `ExecEnv`.
