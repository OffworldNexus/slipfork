//! `slipfork-world`: the single canonical home for world identity, the [`WorldEngine`] port, and
//! every plain value an engine, the edge proxy, and the CLI exchange.
//!
//! This crate is intentionally tiny and depends only on [`serde`], [`thiserror`], [`sha2`], and
//! [`hex`]. It owns value and port definitions only: it performs no bind, connect, process spawn,
//! or filesystem/environment I/O, and holds no global. The one OS type it names is the
//! `dial_world_tcp -> std::net::TcpStream` **result** of the engine port — the port's shape, never
//! a socket this crate opens. Every consumer can therefore share the same
//! [`WorldTarget`] / [`LocalHostRoute`] / [`WorldOrigin`] / [`port`] vocabulary without inverting
//! layering. [`merkle`] (`file_digest`, `merkle_hex`, `glob_matches`) and
//! [`format`](mod@crate::format) (`quoted`) are the only implementations of those shared helpers, so
//! `slipfork-dag` and `slipfork-engine` delegate instead of forking them:
//!
//! ```text
//! slipfork-world  <--  slipfork-snow (manifest validation & coordinates)
//!        ^
//!        +----------  slipfork-wire (control contract framing & relay)
//!        +----------  slipfork-engine (BtrfsEngine implements WorldEngine)
//!        +----------  slipfork-edge / slipfork-test (M2/M3)
//! ```

#![forbid(unsafe_code)]

pub mod env;
pub mod format;
pub mod identity;
pub mod lineage;
pub mod logs;
pub mod merkle;
pub mod pod;
pub mod port;
pub mod rootfs;
pub mod socket;
pub mod stream;
pub mod weaver;

pub use env::{EnvNameError, ExecEnv, InheritedVar, REDACTED};
pub use identity::{
    DNS_LABEL_REASON, LocalHostRoute, ProjectSlug, WorldError, WorldOrigin, WorldTarget,
    is_valid_dns_label, normalize_host, validate_fork_name, validate_unreserved_dns_label,
};
pub use lineage::{
    BaseLineage, CacheOutcome, RootfsLineage, SeedLineage, ServiceImageLineage, ServiceKind,
    WorldLineage, WorldServiceInfo,
};
pub use logs::{SERVICE_LOG_CAP_BYTES, ServiceLogs};
pub use pod::{
    PodCommand, PodError, PodLocation, PodMount, PodNamespaces, PodVolume, ServicePodSpec,
    VOLUMES_DIR, VolumePlan,
};
pub use port::{
    BootLineage, EngineError, ExecOutput, LocalIngressBridge, MountMode, ServiceHandle,
    ServiceRuntime, ServiceSpec, SnapshotId, StateSubdirMount, WorldEngine, WorldHandle,
    WorldMount, WorldSpec,
};
pub use socket::{UNIX_SOCKET_PATH_MAX, unix_socket_path_fits};
pub use stream::{EntryKind, LogChunk, ServiceState, StreamChunk, StreamSource, WorldEntryCommand};
pub use weaver::{SERVICE_NAME_MAX, WeaverServiceName, derive_weaver_service_name};
