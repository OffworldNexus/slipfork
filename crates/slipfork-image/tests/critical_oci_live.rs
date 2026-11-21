//! Protected live suite for the native OCI Distribution v2 puller (`--features live`).
//!
//! Every byte moves over a real `127.0.0.1:0` Distribution v2 registry and every unpack target is
//! a real Btrfs subvolume on disk. On a non-Btrfs host the suite FAILS CLOSED with the mount
//! remedy instead of skipping, because a skipped CoW suite hides broken snapshots forever.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use slipfork_image::{
    Architecture, Credentials, Error, ImageLayout, ImageReference, LayerUnpacker, OciPuller,
    RegistryClient, RegistryScheme,
};
use slipfork_world::{BtrfsEngine, Dirtiness, StoreLayout, WorldEngine, is_btrfs_path};

const GZIP: &str = "application/vnd.oci.image.layer.v1.tar+gzip";
const ZSTD: &str = "application/vnd.oci.image.layer.v1.tar+zstd";
const OCI_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
const OCI_INDEX: &str = "application/vnd.oci.image.index.v1+json";
const MOUNT_REMEDY: &str = "mount -o user_subvol_rm_allowed,noatime <device> <mount>";

fn scratch_root() -> PathBuf {
    if let Some(root) = std::env::var_os("SLIPFORK_TEST_ROOT") {
        return PathBuf::from(root);
    }
    let home = std::env::var_os("HOME")
        .expect("fail closed: HOME is unset; export SLIPFORK_TEST_ROOT=<btrfs dir>");
    PathBuf::from(home).join(".cache/slipfork/tests")
}

fn live_run_root() -> PathBuf {
    let scratch = scratch_root();
    assert!(
        !scratch.starts_with("/tmp"),
        "fail closed: `{}` is RAM-backed tmpfs and has no Btrfs ioctls.\nnext: set SLIPFORK_TEST_ROOT to a Btrfs directory such as ~/.cache/slipfork/tests or /var/tmp/slipfork",
        scratch.display()
    );
    std::fs::create_dir_all(&scratch).expect("create Btrfs scratch root");
    match is_btrfs_path(&scratch) {
        Ok(true) => {}
        Ok(false) => panic!(
            "fail closed: `{}` is not on Btrfs (BTRFS_SUPER_MAGIC 0x9123683E missing).\nnext: run on a Btrfs host or export SLIPFORK_TEST_ROOT=<btrfs dir>; {MOUNT_REMEDY}",
            scratch.display()
        ),
        Err(err) => panic!(
            "fail closed: probing `{}` failed: {err}\nnext: {MOUNT_REMEDY}",
            scratch.display()
        ),
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock reads before the epoch")
        .as_nanos();
    let run = scratch.join(format!("oci-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&run).expect("create run root");
    run
}

fn host_architecture() -> Architecture {
    match std::env::consts::ARCH {
        "x86_64" => Architecture::X86_64,
        "aarch64" => Architecture::Aarch64,
        other => panic!("fail closed: unsupported test architecture `{other}`"),
    }
}

#[test]
fn native_oci_pull_lifecycle() {
    let run = live_run_root();
    let store = StoreLayout::new(&run);
    let engine = Arc::new(BtrfsEngine::new(store.clone()));
    engine.ensure_store().expect("create the Btrfs image store");
    let images = ImageLayout::new(store.images_root());

    let fixture = Fixture::start();
    let arch = host_architecture();
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

    // Path traversal and escaping symlinks are refused.
    let escape = puller
        .pull(&fixture.reference("library/escape"))
        .expect_err("a `..` entry must fail the pull");
    assert!(
        matches!(escape, Error::UnsafePath { .. }),
        "unexpected error: {escape}"
    );
    let link = puller
        .pull(&fixture.reference("library/symlink"))
        .expect_err("an escaping symlink must fail the pull");
    assert!(
        matches!(link, Error::UnsafeLink { .. }),
        "unexpected error: {link}"
    );
    assert!(
        partial_entries(images.root()).is_empty(),
        "a rejected layer must delete its staging subvolume"
    );
    assert!(!images.root().join("escape.txt").exists());

    // Incomplete rootfs: device/fifo entries are refused instead of silently dropped.
    let device = puller
        .pull(&fixture.reference("library/device"))
        .expect_err("a character-device entry must fail the pull");
    assert!(
        matches!(device, Error::UnsupportedEntry { .. }),
        "unexpected error: {device}"
    );
    assert!(
        partial_entries(images.root()).is_empty(),
        "a refused layer must delete its staging subvolume"
    );

    cleanup(&engine, images.root());
    drop(fixture);
    let _ = std::fs::remove_dir_all(&run);
}

#[test]
fn oci_wire_protocol_reference_registry() {
    let scratch = scratch_root().join(format!(
        "oci-wire-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock reads before the epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&scratch).expect("create the wire scratch root");
    let fixture = Fixture::start();
    let arch = host_architecture();
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
    let unpacker = LayerUnpacker::new(&root);
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
    let error = LayerUnpacker::new(scratch.join("escape-root"))
        .apply(&mut &bytes[..], GZIP)
        .expect_err("a `..` entry must be refused");
    assert!(
        matches!(error, Error::UnsafePath { .. }),
        "unexpected error: {error}"
    );

    let link_reference = fixture.reference("library/symlink");
    let mut link = client.session(&link_reference);
    let evil = link.manifest(arch).expect("resolve the symlink image");
    let mut blob = link
        .blob(&evil.manifest.layers[0].digest)
        .expect("stream the symlink layer");
    let mut bytes = Vec::new();
    blob.read_to_end(&mut bytes)
        .expect("read the symlink layer");
    let error = LayerUnpacker::new(scratch.join("link-root"))
        .apply(&mut &bytes[..], GZIP)
        .expect_err("an escaping symlink must be refused");
    assert!(
        matches!(error, Error::UnsafeLink { .. }),
        "unexpected error: {error}"
    );

    let _ = std::fs::remove_dir_all(&scratch);
}

fn read_text(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|err| panic!("read `{}`: {err}", path.display()))
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

// ---------------------------------------------------------------------------------------------
// Loopback Distribution v2 reference registry
// ---------------------------------------------------------------------------------------------

struct Fixture {
    addr: String,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

#[derive(Default)]
struct State {
    token: String,
    images: BTreeMap<String, Served>,
    blob_requests: u64,
    token_requests: u64,
    v2_auth: Vec<Option<String>>,
    manifest_refs: Vec<String>,
    arch_digests: BTreeMap<(String, String), String>,
}

#[derive(Default)]
struct Served {
    manifests: BTreeMap<String, ServedManifest>,
    blobs: BTreeMap<String, Vec<u8>>,
}

struct ServedManifest {
    media_type: String,
    body: Vec<u8>,
}

impl Fixture {
    fn start() -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind the loopback registry");
        let addr = listener
            .local_addr()
            .expect("read the loopback address")
            .to_string();
        listener
            .set_nonblocking(true)
            .expect("make the listener non-blocking");
        let state = Arc::new(Mutex::new(State {
            token: "slipfork-test-token".to_owned(),
            ..State::default()
        }));
        seed_images(&mut state.lock().expect("lock the registry state"));
        let stop = Arc::new(AtomicBool::new(false));
        let accept = {
            let state = Arc::clone(&state);
            let stop = Arc::clone(&stop);
            thread::spawn(move || accept_loop(listener, state, stop))
        };
        Self {
            addr,
            state,
            stop,
            accept: Some(accept),
        }
    }

    fn reference(&self, repo: &str) -> ImageReference {
        ImageReference::parse(&format!("{}/{}:latest", self.addr, repo))
            .expect("fixture reference parses")
    }

    fn demo_digest(&self, arch: &str) -> String {
        self.state
            .lock()
            .expect("lock the registry state")
            .arch_digests
            .get(&("library/demo".to_owned(), arch.to_owned()))
            .cloned()
            .expect("demo manifest for the host architecture")
    }

    fn blob_requests(&self) -> u64 {
        self.state
            .lock()
            .expect("lock the registry state")
            .blob_requests
    }

    fn token_requests(&self) -> u64 {
        self.state
            .lock()
            .expect("lock the registry state")
            .token_requests
    }

    fn first_v2_auth(&self) -> Option<String> {
        self.state
            .lock()
            .expect("lock the registry state")
            .v2_auth
            .first()
            .cloned()
            .flatten()
    }

    fn bearer_used(&self) -> bool {
        self.state
            .lock()
            .expect("lock the registry state")
            .v2_auth
            .iter()
            .flatten()
            .any(|value| value.starts_with("Bearer "))
    }

    fn manifest_refs(&self) -> Vec<String> {
        self.state
            .lock()
            .expect("lock the registry state")
            .manifest_refs
            .clone()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(&self.addr);
        if let Some(handle) = self.accept.take() {
            let _ = handle.join();
        }
    }
}

fn accept_loop(listener: TcpListener, state: Arc<Mutex<State>>, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let state = Arc::clone(&state);
                thread::spawn(move || handle(stream, state));
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(2));
            }
            Err(_) => break,
        }
    }
}

fn read_request(stream: &mut TcpStream) -> Option<(String, BTreeMap<String, String>)> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..read]);
        if buf.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    if buf.is_empty() {
        return None;
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines = text.split("\r\n");
    let path = lines.next()?.split_whitespace().nth(1)?.to_owned();
    let mut headers = BTreeMap::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    Some((path, headers))
}

fn handle(mut stream: TcpStream, state: Arc<Mutex<State>>) {
    let Some((path, headers)) = read_request(&mut stream) else {
        return;
    };
    let path_only = path.split('?').next().unwrap_or(&path).to_owned();
    let local = stream
        .local_addr()
        .map(|addr| addr.to_string())
        .unwrap_or_default();
    let mut guard = state.lock().expect("lock the registry state");

    if path_only == "/token" {
        guard.token_requests += 1;
        let body = format!("{{\"token\":\"{}\"}}", guard.token);
        drop(guard);
        respond(
            &mut stream,
            200,
            "OK",
            &[("Content-Type", "application/json".to_owned())],
            body.as_bytes(),
        );
        return;
    }

    let expected = format!("Bearer {}", guard.token);
    let authorization = headers.get("authorization").cloned();
    if path_only.starts_with("/v2") {
        guard.v2_auth.push(authorization.clone());
    }
    // `/v2/` never challenges and `public/**` is an unauthenticated mirror, so an anonymous
    // client can pull it without ever visiting `/token`.
    let anonymous = path_only == "/v2/" || is_public(&path_only);
    if !anonymous && authorization.as_deref() != Some(expected.as_str()) {
        let challenge = format!(
            "Bearer realm=\"http://{local}/token\",service=\"slipfork-test\",scope=\"repository:library/demo:pull\""
        );
        drop(guard);
        respond(
            &mut stream,
            401,
            "Unauthorized",
            &[("WWW-Authenticate", challenge)],
            b"",
        );
        return;
    }
    if path_only == "/v2/" {
        drop(guard);
        respond(
            &mut stream,
            200,
            "OK",
            &[("Content-Type", "application/json".to_owned())],
            b"{}",
        );
        return;
    }
    let Some(rest) = path_only.strip_prefix("/v2/") else {
        drop(guard);
        respond(&mut stream, 404, "Not Found", &[], b"");
        return;
    };
    if let Some((repo, reference)) = rest.split_once("/manifests/") {
        guard.manifest_refs.push(format!("{repo}/{reference}"));
        let manifest = guard
            .images
            .get(repo)
            .and_then(|image| image.manifests.get(reference))
            .map(|manifest| (manifest.media_type.clone(), manifest.body.clone()));
        drop(guard);
        match manifest {
            Some((media_type, body)) => {
                let digest = sha256_hex(&body);
                respond(
                    &mut stream,
                    200,
                    "OK",
                    &[
                        ("Content-Type", media_type),
                        ("Docker-Content-Digest", digest),
                    ],
                    &body,
                );
            }
            None => respond(&mut stream, 404, "Not Found", &[], b""),
        }
        return;
    }
    if let Some((repo, digest)) = rest.split_once("/blobs/") {
        guard.blob_requests += 1;
        let blob = guard
            .images
            .get(repo)
            .and_then(|image| image.blobs.get(digest))
            .cloned();
        drop(guard);
        match blob {
            Some(bytes) => respond(
                &mut stream,
                200,
                "OK",
                &[("Content-Type", "application/octet-stream".to_owned())],
                &bytes,
            ),
            None => respond(&mut stream, 404, "Not Found", &[], b""),
        }
        return;
    }
    drop(guard);
    respond(&mut stream, 404, "Not Found", &[], b"");
}

/// `public/**` repositories are served without a challenge, like a public mirror.
fn is_public(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("/v2/") else {
        return false;
    };
    let repository = rest
        .split_once("/manifests/")
        .map(|(repository, _)| repository)
        .or_else(|| rest.split_once("/blobs/").map(|(repository, _)| repository));
    repository.is_some_and(|repository| repository.starts_with("public/"))
}

fn respond(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    headers: &[(&str, String)],
    body: &[u8],
) {
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

// ---------------------------------------------------------------------------------------------
// Image fixtures
// ---------------------------------------------------------------------------------------------

fn seed_images(state: &mut State) {
    seed_demo(state);
    seed_public(state);
    seed_device(state);
    seed_corrupt(state);
    seed_escape(state);
    seed_symlink(state);
}

fn seed_demo(state: &mut State) {
    let mut base = TarBuilder::new();
    base.dir("app", 0o755);
    base.file("app/old.txt", 0o644, b"old");
    base.file("app/keep.txt", 0o644, b"keep");
    base.dir("app/dir", 0o755);
    base.file("app/dir/a.txt", 0o644, b"a");
    base.file("app/dir/b.txt", 0o644, b"b");
    base.dir("bin", 0o755);
    base.file("bin/tool", 0o755, b"#!/bin/sh\n");
    let base_bytes = gzip(&base.finish());

    let mut patch = TarBuilder::new();
    patch.file("app/.wh.old.txt", 0o644, b"");
    patch.file("app/dir/.wh..wh..opq", 0o644, b"");
    patch.file("app/dir/new.txt", 0o644, b"new");
    let patch_bytes = zstd(&patch.finish());

    let layers = [
        (sha256_hex(&base_bytes), base_bytes.len() as u64, GZIP),
        (sha256_hex(&patch_bytes), patch_bytes.len() as u64, ZSTD),
    ];
    let mut served = Served::default();
    served.blobs.insert(layers[0].0.clone(), base_bytes);
    served.blobs.insert(layers[1].0.clone(), patch_bytes);

    let mut entries = Vec::new();
    for arch in ["amd64", "arm64"] {
        let manifest = arch_manifest(&layers, arch);
        let digest = sha256_hex(&manifest);
        served.manifests.insert(
            digest.clone(),
            ServedManifest {
                media_type: OCI_MANIFEST.to_owned(),
                body: manifest.clone(),
            },
        );
        state
            .arch_digests
            .insert(("library/demo".to_owned(), arch.to_owned()), digest.clone());
        entries.push((digest, manifest.len() as u64, arch));
    }
    let index = index_json(&entries);
    served.manifests.insert(
        "latest".to_owned(),
        ServedManifest {
            media_type: OCI_INDEX.to_owned(),
            body: index,
        },
    );
    state.images.insert("library/demo".to_owned(), served);
}

fn seed_corrupt(state: &mut State) {
    let advertised = sha256_hex(b"bytes-the-server-never-sends");
    let mut tar = TarBuilder::new();
    tar.file("app/broken.txt", 0o644, b"payload");
    let served_bytes = gzip(&tar.finish());
    let manifest = arch_manifest(
        &[(advertised.clone(), served_bytes.len() as u64, GZIP)],
        "amd64",
    );
    let mut image = Served::default();
    image.blobs.insert(advertised, served_bytes);
    image.manifests.insert(
        "latest".to_owned(),
        ServedManifest {
            media_type: OCI_MANIFEST.to_owned(),
            body: manifest,
        },
    );
    state.images.insert("library/corrupt".to_owned(), image);
}

fn seed_escape(state: &mut State) {
    let mut tar = TarBuilder::new();
    tar.raw_file("../escape.txt", b"pwned");
    let bytes = gzip(&tar.finish());
    let digest = sha256_hex(&bytes);
    let manifest = arch_manifest(&[(digest.clone(), bytes.len() as u64, GZIP)], "amd64");
    let mut image = Served::default();
    image.blobs.insert(digest, bytes);
    image.manifests.insert(
        "latest".to_owned(),
        ServedManifest {
            media_type: OCI_MANIFEST.to_owned(),
            body: manifest,
        },
    );
    state.images.insert("library/escape".to_owned(), image);
}

fn seed_symlink(state: &mut State) {
    let mut tar = TarBuilder::new();
    tar.raw_symlink("app/link", "../../etc");
    let bytes = gzip(&tar.finish());
    let digest = sha256_hex(&bytes);
    let manifest = arch_manifest(&[(digest.clone(), bytes.len() as u64, GZIP)], "amd64");
    let mut image = Served::default();
    image.blobs.insert(digest, bytes);
    image.manifests.insert(
        "latest".to_owned(),
        ServedManifest {
            media_type: OCI_MANIFEST.to_owned(),
            body: manifest,
        },
    );
    state.images.insert("library/symlink".to_owned(), image);
}

/// Unauthenticated mirror image: `public/demo` serves direct 200s with no challenge.
fn seed_public(state: &mut State) {
    let mut tar = TarBuilder::new();
    tar.dir("usr", 0o755);
    tar.dir("usr/share", 0o755);
    tar.file("usr/share/demo.txt", 0o644, b"public");
    let bytes = gzip(&tar.finish());
    let digest = sha256_hex(&bytes);
    let manifest = arch_manifest(&[(digest.clone(), bytes.len() as u64, GZIP)], "amd64");
    let mut image = Served::default();
    image.blobs.insert(digest, bytes);
    image.manifests.insert(
        "latest".to_owned(),
        ServedManifest {
            media_type: OCI_MANIFEST.to_owned(),
            body: manifest,
        },
    );
    state.images.insert("public/demo".to_owned(), image);
}

/// Incomplete rootfs: a character-device entry has no representation on disk.
fn seed_device(state: &mut State) {
    let mut tar = TarBuilder::new();
    tar.dir("dev", 0o755);
    tar.raw_special("dev/null", tar::EntryType::Char);
    let bytes = gzip(&tar.finish());
    let digest = sha256_hex(&bytes);
    let manifest = arch_manifest(&[(digest.clone(), bytes.len() as u64, GZIP)], "amd64");
    let mut image = Served::default();
    image.blobs.insert(digest, bytes);
    image.manifests.insert(
        "latest".to_owned(),
        ServedManifest {
            media_type: OCI_MANIFEST.to_owned(),
            body: manifest,
        },
    );
    state.images.insert("library/device".to_owned(), image);
}

fn arch_manifest(layers: &[(String, u64, &str)], arch: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "schemaVersion": 2,
        "mediaType": OCI_MANIFEST,
        "annotations": { "org.example.arch": arch },
        "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "digest": sha256_hex(b"{}"),
            "size": 2
        },
        "layers": layers
            .iter()
            .map(|(digest, size, media_type)| serde_json::json!({
                "mediaType": media_type,
                "digest": digest,
                "size": size
            }))
            .collect::<Vec<_>>()
    }))
    .expect("manifest json")
}

fn index_json(entries: &[(String, u64, &str)]) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "schemaVersion": 2,
        "mediaType": OCI_INDEX,
        "manifests": entries
            .iter()
            .map(|(digest, size, arch)| serde_json::json!({
                "mediaType": OCI_MANIFEST,
                "digest": digest,
                "size": size,
                "platform": { "architecture": arch, "os": "linux" }
            }))
            .collect::<Vec<_>>()
    }))
    .expect("index json")
}

struct TarBuilder {
    builder: tar::Builder<Vec<u8>>,
}

impl TarBuilder {
    fn new() -> Self {
        Self {
            builder: tar::Builder::new(Vec::new()),
        }
    }

    fn dir(&mut self, path: &str, mode: u32) {
        let mut header = tar::Header::new_gnu();
        header.set_path(format!("{path}/")).expect("directory path");
        header.set_entry_type(tar::EntryType::Directory);
        header.set_size(0);
        header.set_mode(mode);
        header.set_cksum();
        self.builder
            .append(&header, io::empty())
            .expect("append a directory");
    }

    fn file(&mut self, path: &str, mode: u32, data: &[u8]) {
        let mut header = tar::Header::new_gnu();
        header.set_path(path).expect("file path");
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(data.len() as u64);
        header.set_mode(mode);
        header.set_cksum();
        self.builder.append(&header, data).expect("append a file");
    }

    fn raw_file(&mut self, name: &str, data: &[u8]) {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.as_mut_bytes()[..name.len()].copy_from_slice(name.as_bytes());
        header.set_cksum();
        self.builder
            .append(&header, data)
            .expect("append a raw file entry");
    }

    fn raw_symlink(&mut self, name: &str, target: &str) {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        let bytes = header.as_mut_bytes();
        bytes[..name.len()].copy_from_slice(name.as_bytes());
        bytes[157..157 + target.len()].copy_from_slice(target.as_bytes());
        header.set_cksum();
        self.builder
            .append(&header, io::empty())
            .expect("append a raw symlink entry");
    }

    fn raw_special(&mut self, name: &str, entry_type: tar::EntryType) {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(entry_type);
        header.set_size(0);
        header.set_mode(0o644);
        header.as_mut_bytes()[..name.len()].copy_from_slice(name.as_bytes());
        header.set_cksum();
        self.builder
            .append(&header, io::empty())
            .expect("append a special entry");
    }

    fn finish(self) -> Vec<u8> {
        self.builder.into_inner().expect("tar bytes")
    }
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes).expect("gzip the layer");
    encoder.finish().expect("finish the gzip layer")
}

fn zstd(bytes: &[u8]) -> Vec<u8> {
    zstd::stream::encode_all(bytes, 3).expect("zstd the layer")
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{}", hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}
