//! Shared [`WorldRootfs`] conformance checks for every [`slipfork_world::rootfs::BaseImageBuilder`].
//!
//! The suite asserts only what every builder must satisfy without Docker, an OCI registry, or a
//! host ABI: a content-addressed identity, a readable `/.slipfork/bin` layout, metadata that agrees
//! with `snow.yml`, and host caches that are either absent or strictly read-only. Real `buildx`
//! extraction is asserted by `tests/critical_rootfs_live.rs`.

#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;

use slipfork_engine::rootfs::WorldRootfsRequest;
use slipfork_engine::rootfs::{CRUN_GUEST_PATH, WORLD_INIT_GUEST_PATH};
use slipfork_world::MountMode;
use slipfork_world::rootfs::WorldRootfs;

/// Verify the shared [`WorldRootfs`] contract for any builder.
///
/// # Panics
///
/// Panics with a precise diagnostic when `rootfs` violates the contract for `request`.
pub fn assert_world_rootfs_conformance(rootfs: &WorldRootfs, request: &WorldRootfsRequest) {
    assert_eq!(rootfs.digest.hex().len(), 64, "digest must be 64 hex chars");
    assert!(
        rootfs
            .digest
            .hex()
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
            && rootfs.digest.hex() == rootfs.digest.hex().to_lowercase(),
        "digest must be lowercase hex"
    );
    assert!(
        rootfs.app_rootfs.is_absolute() && rootfs.app_rootfs.is_dir(),
        "app_rootfs '{}' must be an absolute existing directory",
        rootfs.app_rootfs.display()
    );
    assert_eq!(
        rootfs.crun_bin.as_os_str(),
        CRUN_GUEST_PATH,
        "crun_bin must be the guest path {CRUN_GUEST_PATH}"
    );
    for guest in [CRUN_GUEST_PATH, WORLD_INIT_GUEST_PATH] {
        let path = slipfork_engine::rootfs::guest_binary(rootfs, guest)
            .unwrap_or_else(|| panic!("app_rootfs must contain {guest}"));
        let metadata = std::fs::metadata(&path).expect("runtime binary metadata");
        assert!(
            metadata.permissions().mode() & 0o111 != 0,
            "{guest} must be executable"
        );
    }

    let requested: BTreeSet<_> = request.services.iter().cloned().collect();
    let offered: BTreeSet<_> = rootfs.service_images.keys().cloned().collect();
    assert_eq!(
        offered, requested,
        "service_images keys must equal the requested snow.yml resources"
    );
    for (key, image) in &rootfs.service_images {
        assert!(
            image.digest.starts_with("sha256:") && image.digest.len() == 71,
            "{key} OCI digest must be sha256:<64 hex>, got '{}'",
            image.digest
        );
        assert!(
            image.rootfs.is_absolute(),
            "{key} OCI rootfs must be absolute"
        );
        assert!(
            image.config.is_absolute(),
            "{key} OCI config must be absolute"
        );
    }

    assert!(
        matches!(
            rootfs.metadata.os_release.id.as_str(),
            "debian" | "ubuntu" | "alpine"
        ),
        "metadata.os_release.id must be a supported distro, got '{}'",
        rootfs.metadata.os_release.id
    );
    assert!(
        !rootfs.metadata.elf_abi.arch.is_empty(),
        "metadata.elf_abi.arch must be populated"
    );
    let allowed = request.ports.allowed_ports();
    for port in &rootfs.metadata.exposed_ports {
        assert!(
            allowed.contains(port),
            "exposed port {port} must be declared by snow.yml components/ingresses"
        );
    }

    if request.host_caches.mounts.is_empty() {
        assert!(
            rootfs.host_caches.is_empty(),
            "host_caches must stay empty without --host-caches"
        );
    } else {
        assert_eq!(
            rootfs.host_caches, request.host_caches.mounts,
            "host_caches must mirror the requested read-only mounts"
        );
        for mount in &rootfs.host_caches {
            assert_eq!(
                mount.mode,
                MountMode::ReadOnly,
                "host cache '{}' must be read-only",
                mount.host_path.display()
            );
        }
    }
}
