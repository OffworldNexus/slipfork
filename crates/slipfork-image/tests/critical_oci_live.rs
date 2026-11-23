//! Protected live suite for the native OCI Distribution v2 puller (`--features live`).
//!
//! Every byte moves over a real `127.0.0.1:0` Distribution v2 registry and every unpack target is
//! a real Btrfs subvolume on disk. On a non-Btrfs host the suite FAILS CLOSED with the mount
//! remedy instead of skipping, because a skipped CoW suite hides broken snapshots forever.

#[path = "support/mod.rs"]
mod support;

use std::io::Read;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use slipfork_image::{
    Credentials, Error, ImageLayout, ImageReference, OciPuller, RegistryClient, RegistryScheme,
};
use slipfork_world::{BtrfsEngine, Dirtiness, StoreLayout, WorldEngine};
use support::{Fixture, GZIP, OCI_MANIFEST, read_text, sha256_hex};

/// A real Docker Hub pull: proves the `registry-1.docker.io` API host and the
/// `auth.docker.io` bearer flow, which a loopback fixture can never exercise.
#[test]
fn real_docker_hub_pull() {
    let run = support::live_run_root("oci-hub");
    let store = StoreLayout::new(&run);
    let engine = Arc::new(BtrfsEngine::new(store.clone()));
    engine.ensure_store().expect("create the Btrfs image store");
    let images = ImageLayout::new(store.images_root());
    let arch = support::host_architecture();
    let puller = OciPuller::new(
        Arc::clone(&engine) as Arc<dyn WorldEngine>,
        images,
        Credentials::Anonymous,
        arch,
        "slipfork-tests/0.1".to_owned(),
    );
    let reference =
        ImageReference::parse("docker.io/library/alpine:3.20").expect("valid Hub reference");
    assert_eq!(
        reference.registry(),
        "registry-1.docker.io",
        "Docker Hub aliases must normalize to the Distribution API host"
    );
    let pulled = puller.pull(&reference).unwrap_or_else(|error| {
        panic!(
            "fail closed: could not pull {reference} from Docker Hub: {error}\nnext: allow HTTPS egress to registry-1.docker.io and auth.docker.io, then re-run the live suite"
        )
    });
    assert_eq!(
        pulled.architecture,
        arch,
        "the multi-arch index must resolve to linux/{}",
        arch.oci()
    );
    assert!(
        engine.is_sealed(&pulled.rootfs).expect("probe the seal"),
        "the real image rootfs must be sealed"
    );
    assert!(
        pulled.rootfs.join("etc/alpine-release").is_file(),
        "the alpine rootfs must unpack its release marker"
    );
    engine
        .destroy(&pulled.rootfs, Dirtiness::Clean, false)
        .expect("destroy the real image rootfs");
    std::fs::remove_dir_all(&run).expect("remove the real-pull scratch run");
}

#[test]
fn native_oci_pull_lifecycle() {
    let run = support::live_run_root("oci");
    let store = StoreLayout::new(&run);
    let engine = Arc::new(BtrfsEngine::new(store.clone()));
    engine.ensure_store().expect("create the Btrfs image store");
    let images = ImageLayout::new(store.images_root());

    let fixture = Fixture::start();
    let arch = support::host_architecture();
    let puller = OciPuller::new(
        Arc::clone(&engine) as Arc<dyn WorldEngine>,
        images.clone(),
        Credentials::basic("slipfork", "test-secret"),
        arch,
        "slipfork-tests/0.1".to_owned(),
    )
    .with_scheme(RegistryScheme::Http);

    // Multi-arch manifest list, gzip + zstd layers, `.wh.` deletion and `.wh..wh..opq`.
    let happy = puller
        .pull(&fixture.reference("library/demo"))
        .expect("pull the multi-arch demo image");
    assert_eq!(happy.architecture, arch);
    assert_eq!(happy.digest, fixture.demo_digest(arch.oci()));
    assert!(
        engine.is_sealed(&happy.rootfs).expect("probe the seal"),
        "the unpacked image subvolume must be sealed"
    );
    assert!(
        fixture
            .manifest_refs()
            .contains(&format!("library/demo/{}", happy.digest)),
        "expected the per-architecture manifest to be fetched, saw {:?}",
        fixture.manifest_refs()
    );
    assert_eq!(read_text(&happy.rootfs.join("app/keep.txt")), "keep");
    assert!(
        !happy.rootfs.join("app/old.txt").exists(),
        "`.wh.old.txt` must delete the lower layer entry"
    );
    assert!(!happy.rootfs.join("app/dir/a.txt").exists());
    assert!(!happy.rootfs.join("app/dir/b.txt").exists());
    assert_eq!(read_text(&happy.rootfs.join("app/dir/new.txt")), "new");
    assert_eq!(
        std::fs::metadata(happy.rootfs.join("bin/tool"))
            .expect("stat the unpacked tool")
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    let write = std::fs::write(happy.rootfs.join("blocked"), b"x")
        .expect_err("a sealed image must reject writes");
    assert_eq!(
        write.raw_os_error(),
        Some(libc::EROFS),
        "expected EROFS from BTRFS_SUBVOL_RDONLY, got {write}"
    );

    // Bearer challenge: the `/v2/` probe is unauthenticated; the manifest request is challenged
    // and then retried with the bearer token.
    assert!(fixture.token_requests() >= 1, "expected a token fetch");
    assert_eq!(
        fixture.first_v2_auth(),
        None,
        "the first probe must be challenged without auth"
    );
    assert!(
        fixture.bearer_used(),
        "expected the retried request to carry `Bearer`"
    );

    // Cache hit: zero blob requests on the second pull.
    let blobs_before = fixture.blob_requests();
    let cached = puller
        .pull(&fixture.reference("library/demo"))
        .expect("pull the cached image");
    assert_eq!(cached.digest, happy.digest);
    assert_eq!(
        fixture.blob_requests(),
        blobs_before,
        "a cache hit must not download a single blob"
    );
    assert!(engine.is_sealed(&cached.rootfs).expect("probe the seal"));

    // Unauthenticated mirror: `public/**` answers direct 200s, so anonymous credentials never
    // reach the token endpoint.
    let tokens_before = fixture.token_requests();
    let anonymous = OciPuller::new(
        Arc::clone(&engine) as Arc<dyn WorldEngine>,
        images.clone(),
        Credentials::Anonymous,
        arch,
        "slipfork-tests/0.1".to_owned(),
    )
    .with_scheme(RegistryScheme::Http);
    let public = anonymous
        .pull(&fixture.reference("public/demo"))
        .expect("pull the unauthenticated image");
    assert_eq!(
        read_text(&public.rootfs.join("usr/share/demo.txt")),
        "public"
    );
    assert!(engine.is_sealed(&public.rootfs).expect("probe the seal"));
    assert_eq!(
        fixture.token_requests(),
        tokens_before,
        "an anonymous pull of a public repository must never fetch a token"
    );

    // Corrupted blob: typed digest mismatch, staging removed, never sealed.
    let corrupt = puller
        .pull(&fixture.reference("library/corrupt"))
        .expect_err("a corrupted layer must fail the pull");
    assert!(
        matches!(corrupt, Error::DigestMismatch { .. }),
        "unexpected error: {corrupt}"
    );
    assert!(
        partial_entries(images.root()).is_empty(),
        "a failed pull must delete its staging subvolume"
    );

    // Character devices and fifos have no rootfs representation: refused, staging removed.
    let device = puller
        .pull(&fixture.reference("library/device"))
        .expect_err("a character-device layer entry must be refused");
    assert!(
        matches!(device, Error::UnsupportedEntry { .. }),
        "unexpected error: {device}"
    );
    assert!(
        partial_entries(images.root()).is_empty(),
        "a refused layer must delete its staging subvolume"
    );

    // Path traversal and escaping symlinks are refused.
    let escape = puller
        .pull(&fixture.reference("library/escape"))
        .expect_err("a `..` layer entry must be refused");
    assert!(
        matches!(escape, Error::UnsafePath { .. }),
        "unexpected error: {escape}"
    );
    // Container-absolute links are legitimate: a hardlink rebases onto the rootfs, a symlink
    // is stored verbatim, and an absolute target is never followed while extracting.
    let links = puller
        .pull(&fixture.reference("library/links"))
        .expect("pull the container-absolute link image");
    let busybox = links.rootfs.join("bin/busybox");
    let arch_bin = links.rootfs.join("bin/arch");
    let stat = |path: &Path| std::fs::symlink_metadata(path).expect("stat a pulled link");
    assert_eq!(
        stat(&busybox).ino(),
        stat(&arch_bin).ino(),
        "`bin/arch -> /bin/busybox` must share one inode inside the rootfs"
    );
    assert_eq!(read_text(&arch_bin), read_text(&busybox));
    assert_eq!(
        std::fs::read_link(links.rootfs.join("bin/sh")).expect("read the absolute symlink"),
        Path::new("/bin/busybox"),
        "an absolute symlink must be stored verbatim, never rebased or followed"
    );
    // A hardlink that climbs out of the rootfs is still refused, and staging is rolled back.
    let link_escape = puller
        .pull(&fixture.reference("library/link-escape"))
        .expect_err("a `..` hardlink must be refused");
    assert!(
        matches!(link_escape, Error::UnsafeLink { .. }),
        "unexpected error: {link_escape}"
    );
    assert!(partial_entries(images.root()).is_empty());

    cleanup(&engine, images.root());
    drop(fixture);
    let _ = std::fs::remove_dir_all(&run);
}

#[test]
fn oci_wire_protocol_reference_registry() {
    let scratch = support::scratch_root().join(format!(
        "oci-wire-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock reads before the epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&scratch).expect("create the wire scratch root");
    let fixture = Fixture::start();
    let arch = support::host_architecture();
    let client = RegistryClient::new(
        Credentials::basic("slipfork", "test-secret"),
        "slipfork-tests/0.1".to_owned(),
    )
    .with_scheme(RegistryScheme::Http);

    let demo = fixture.reference("library/demo");
    let mut session = client.session(&demo);
    let resolved = session
        .manifest(arch)
        .expect("resolve the multi-arch manifest list");
    assert_eq!(resolved.digest, fixture.demo_digest(arch.oci()));
    assert!(fixture.token_requests() >= 1, "expected a token fetch");
    assert_eq!(fixture.first_v2_auth(), None);
    assert!(fixture.bearer_used());

    let root = scratch.join("rootfs");
    std::fs::create_dir_all(&root).expect("create the unpack root");
    let unpacker = slipfork_image::LayerUnpacker::new(&root);
    for layer in &resolved.manifest.layers {
        let mut blob = session.blob(&layer.digest).expect("stream the layer blob");
        let mut bytes = Vec::new();
        blob.read_to_end(&mut bytes).expect("read the layer blob");
        assert_eq!(
            sha256_hex(&bytes),
            layer.digest,
            "a layer must stream back byte-identical"
        );
        unpacker
            .apply(
                &mut &bytes[..],
                layer.media_type.as_deref().expect("layer media type"),
            )
            .expect("unpack the layer");
    }
    assert_eq!(read_text(&root.join("app/keep.txt")), "keep");
    assert!(!root.join("app/old.txt").exists());
    assert!(!root.join("app/dir/a.txt").exists());
    assert!(!root.join("app/dir/b.txt").exists());
    assert_eq!(read_text(&root.join("app/dir/new.txt")), "new");

    let escape_reference = fixture.reference("library/escape");
    let mut escape = client.session(&escape_reference);
    let evil = escape.manifest(arch).expect("resolve the escape image");
    let mut blob = escape
        .blob(&evil.manifest.layers[0].digest)
        .expect("stream the escape layer");
    let mut bytes = Vec::new();
    blob.read_to_end(&mut bytes).expect("read the escape layer");
    let error = slipfork_image::LayerUnpacker::new(scratch.join("escape-root"))
        .apply(&mut &bytes[..], GZIP)
        .expect_err("a `..` entry must be refused");
    assert!(
        matches!(error, Error::UnsafePath { .. }),
        "unexpected error: {error}"
    );

    let link_reference = fixture.reference("library/links");
    let mut link = client.session(&link_reference);
    let evil = link.manifest(arch).expect("resolve the links image");
    let mut blob = link
        .blob(&evil.manifest.layers[0].digest)
        .expect("stream the links layer");
    let mut bytes = Vec::new();
    blob.read_to_end(&mut bytes).expect("read the links layer");
    let link_root = scratch.join("link-root");
    slipfork_image::LayerUnpacker::new(&link_root)
        .apply(&mut &bytes[..], GZIP)
        .expect("a container-absolute layer unpacks");
    assert_eq!(
        std::fs::read_link(link_root.join("bin/sh")).expect("read the stored symlink"),
        Path::new("/bin/busybox")
    );
    assert_eq!(
        std::fs::symlink_metadata(link_root.join("bin/arch"))
            .expect("stat the rebased hardlink")
            .ino(),
        std::fs::symlink_metadata(link_root.join("bin/busybox"))
            .expect("stat the hardlink target")
            .ino()
    );

    // The demo manifest is a real OCI manifest document, not a fixture shortcut.
    assert_eq!(
        resolved.manifest.media_type.as_deref(),
        Some(OCI_MANIFEST),
        "expected a per-architecture OCI manifest"
    );
    assert!(!resolved.manifest.layers.is_empty());

    let _ = std::fs::remove_dir_all(&scratch);
}

fn partial_entries(root: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(root)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with(".partial-"))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn cleanup(engine: &BtrfsEngine, root: &Path) {
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let _ = engine.destroy(&entry.path(), Dirtiness::Dirty, true);
        }
    }
}
