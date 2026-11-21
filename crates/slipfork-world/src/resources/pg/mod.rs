//! Golden `postgres:18` `initdb` template and the `< 1ms` per-world `PGDATA` clone.

mod launcher;
mod template;

pub use launcher::{PostgresClone, PostgresLauncher};
pub use template::{PostgresTemplate, TemplateState};

/// Store subdirectory holding sealed golden templates.
pub const TEMPLATES_DIR: &str = "templates";
/// The one store-global cluster every world clones.
pub const TEMPLATE_NAME: &str = "postgres-18";
/// Marker recording the `initdb` contract a sealed template was built with.
pub const TEMPLATE_VERSION: &str = "postgres:18/initdb/v1";
/// Version file written **inside** staging before the atomic promote.
pub const TEMPLATE_STAMP: &str = "SLIPFORK_TEMPLATE_VERSION";
/// The in-container `PGDATA` mount for every postgres pod.
pub const PGDATA: &str = "/var/lib/postgresql/data";
/// Unix-socket directory inside the cloned `PGDATA`; readiness never opens a TCP port.
pub const SOCKET_DIR: &str = ".sock";
/// Dev-server flags (`specs/03-under-the-hood.md` §3): no fsync, minimal WAL, POSIX shm.
const DEV_FLAGS: [&str; 20] = [
    "-c",
    "fsync=off",
    "-c",
    "synchronous_commit=off",
    "-c",
    "full_page_writes=off",
    "-c",
    "wal_level=minimal",
    "-c",
    "max_wal_senders=0",
    "-c",
    "autovacuum=off",
    "-c",
    "shared_buffers=32MB",
    "-c",
    "dynamic_shared_memory_type=posix",
    "-c",
    "unix_socket_directories=/var/lib/postgresql/data/.sock",
    "-c",
    "listen_addresses=",
];
/// Rootfs-relative `initdb` locations of the PGDG, source-built, and distro images.
const INITDB_CANDIDATES: [&str; 4] = [
    "usr/lib/postgresql/18/bin/initdb",
    "usr/local/pgsql/bin/initdb",
    "usr/local/bin/initdb",
    "usr/bin/initdb",
];
/// Rootfs-relative `postgres` locations, in the same order.
const POSTGRES_CANDIDATES: [&str; 4] = [
    "usr/lib/postgresql/18/bin/postgres",
    "usr/local/pgsql/bin/postgres",
    "usr/local/bin/postgres",
    "usr/bin/postgres",
];
