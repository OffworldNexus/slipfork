//! Shared loopback Distribution v2 registry fixture for the protected `--features live` suites.
//!
//! Both live test binaries compile this module from a private `mod support;`, so a helper that
//! only one journey happens to call is still reachable from the other crate root. Silencing
//! `dead_code` once here is cheaper than duplicating a 600-line reference registry per suite.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use slipfork_image::{Architecture, ImageReference};
use slipfork_world::is_btrfs_path;

pub const GZIP: &str = "application/vnd.oci.image.layer.v1.tar+gzip";
pub const ZSTD: &str = "application/vnd.oci.image.layer.v1.tar+zstd";
pub const OCI_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
pub const OCI_INDEX: &str = "application/vnd.oci.image.index.v1+json";
pub const MOUNT_REMEDY: &str = "mount -o user_subvol_rm_allowed,noatime <device> <mount>";

/// `SLIPFORK_TEST_ROOT` or `~/.cache/slipfork/tests`; never RAM-backed `/tmp`.
pub fn scratch_root() -> PathBuf {
    if let Some(root) = std::env::var_os("SLIPFORK_TEST_ROOT") {
        return PathBuf::from(root);
    }
    let home = std::env::var_os("HOME")
        .expect("fail closed: HOME is unset; export SLIPFORK_TEST_ROOT=<btrfs dir>");
    PathBuf::from(home).join(".cache/slipfork/tests")
}

/// Creates a fresh, Btrfs-backed run directory. Panics closed when the scratch root cannot carry
/// real subvolumes, because a skipped CoW suite hides broken snapshots forever.
pub fn live_run_root(prefix: &str) -> PathBuf {
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
    let run = scratch.join(format!("{prefix}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&run).expect("create run root");
    run
}

pub fn host_architecture() -> Architecture {
    match std::env::consts::ARCH {
        "x86_64" => Architecture::X86_64,
        "aarch64" => Architecture::Aarch64,
        other => panic!("fail closed: unsupported test architecture `{other}`"),
    }
}

pub fn read_text(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|err| panic!("read `{}`: {err}", path.display()))
}

// ---------------------------------------------------------------------------------------------
// Loopback Distribution v2 reference registry
// ---------------------------------------------------------------------------------------------

pub struct Fixture {
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
    pub fn start() -> Self {
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

    /// `127.0.0.1:<port>/<repo>:latest`, ready for `ImageReference::parse` or a `FROM` line.
    pub fn reference_text(&self, repo: &str) -> String {
        format!("{}/{repo}:latest", self.addr)
    }

    pub fn reference(&self, repo: &str) -> ImageReference {
        ImageReference::parse(&self.reference_text(repo)).expect("fixture reference parses")
    }

    pub fn demo_digest(&self, arch: &str) -> String {
        self.state
            .lock()
            .expect("lock the registry state")
            .arch_digests
            .get(&("library/demo".to_owned(), arch.to_owned()))
            .cloned()
            .expect("demo manifest for the host architecture")
    }

    pub fn blob_requests(&self) -> u64 {
        self.state
            .lock()
            .expect("lock the registry state")
            .blob_requests
    }

    pub fn token_requests(&self) -> u64 {
        self.state
            .lock()
            .expect("lock the registry state")
            .token_requests
    }

    pub fn first_v2_auth(&self) -> Option<String> {
        self.state
            .lock()
            .expect("lock the registry state")
            .v2_auth
            .first()
            .cloned()
            .flatten()
    }

    pub fn bearer_used(&self) -> bool {
        self.state
            .lock()
            .expect("lock the registry state")
            .v2_auth
            .iter()
            .flatten()
            .any(|value| value.starts_with("Bearer "))
    }

    pub fn manifest_refs(&self) -> Vec<String> {
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
            .cloned()
            // Every fixture manifest names the same `{}` image config; serve it on demand.
            .or_else(|| (digest == sha256_hex(b"{}")).then(|| b"{}".to_vec()));
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
    seed_shell(state);
    seed_device(state);
    seed_corrupt(state);
    seed_escape(state);
    seed_symlink(state);
    seed_links(state);
    seed_hardlink_escape(state);
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

/// Container-absolute links: a hardlink to `/bin/busybox` and a symlink to the same path.
fn seed_links(state: &mut State) {
    let mut tar = TarBuilder::new();
    // No `bin/` directory entry: the symlink comes first and pins on-demand parent creation.
    // A hardlink can never precede its target, so the file lands before `bin/arch`.
    tar.raw_symlink("bin/sh", "/bin/busybox");
    tar.file("bin/busybox", 0o755, b"#!/bin/sh\n");
    tar.raw_hardlink("bin/arch", "/bin/busybox");
    serve_single_layer(state, "library/links", gzip(&tar.finish()));
}

/// A hardlink that climbs out of the image root must still be refused.
fn seed_hardlink_escape(state: &mut State) {
    let mut tar = TarBuilder::new();
    tar.file("bin/busybox", 0o755, b"#!/bin/sh\n");
    tar.raw_hardlink("bin/evil", "../../etc/shadow");
    serve_single_layer(state, "library/link-escape", gzip(&tar.finish()));
}

/// Unauthenticated mirror image: `public/demo` serves direct 200s with no challenge.
fn seed_public(state: &mut State) {
    let mut tar = TarBuilder::new();
    tar.dir("usr", 0o755);
    tar.dir("usr/share", 0o755);
    tar.file("usr/share/demo.txt", 0o644, b"public");
    serve_single_layer(state, "public/demo", gzip(&tar.finish()));
}

/// Incomplete rootfs: a character-device entry has no representation on disk.
fn seed_device(state: &mut State) {
    let mut tar = TarBuilder::new();
    tar.dir("dev", 0o755);
    tar.raw_special("dev/null", tar::EntryType::Char);
    serve_single_layer(state, "library/device", gzip(&tar.finish()));
}

/// A runnable base image: the host's `/bin/sh` plus its dynamic-loader closure.
///
/// `RUN` always executes `/bin/sh -c`, so the loopback base image must carry a real shell.
/// Copying the host shell keeps the fixture hermetic across distros and architectures without
/// embedding a prebuilt binary in the repository.
fn seed_shell(state: &mut State) {
    let shell = std::fs::canonicalize("/bin/sh")
        .expect("fail closed: resolve the host /bin/sh for the base image");
    let mut files: Vec<(String, Vec<u8>, u32)> = vec![(
        "bin/sh".to_owned(),
        std::fs::read(&shell).expect("read the host shell"),
        0o755,
    )];
    for library in shared_libraries(&shell) {
        let relative = library
            .strip_prefix("/")
            .expect("ldd only reports absolute library paths");
        files.push((
            relative.to_string_lossy().into_owned(),
            std::fs::read(&library).expect("read a shell dependency"),
            0o755,
        ));
    }
    let mut directories: BTreeSet<String> = ["bin", "etc", "root", "root/.cache", "root/.cache/x"]
        .iter()
        .map(|path| (*path).to_owned())
        .collect();
    for (path, ..) in &files {
        let mut parent = Path::new(path).parent();
        while let Some(directory) = parent {
            if directory.as_os_str().is_empty() {
                break;
            }
            directories.insert(directory.to_string_lossy().into_owned());
            parent = directory.parent();
        }
    }
    let mut tar = TarBuilder::new();
    for directory in &directories {
        tar.dir(directory, 0o755);
    }
    for (path, bytes, mode) in &files {
        tar.file(path, *mode, bytes);
    }
    serve_single_layer(state, "library/shell", gzip(&tar.finish()));
}

/// Absolute shared libraries `ldd` reports for `binary` (empty for a static binary).
fn shared_libraries(binary: &Path) -> Vec<PathBuf> {
    let Ok(output) = std::process::Command::new("ldd").arg(binary).output() else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let mut libraries = Vec::new();
    for line in text.lines() {
        for token in line.split_whitespace() {
            if token.starts_with('/') && Path::new(token).is_file() {
                libraries.push(PathBuf::from(token));
            }
        }
    }
    libraries
}

fn serve_single_layer(state: &mut State, repo: &str, layer: Vec<u8>) {
    let digest = sha256_hex(&layer);
    let manifest = arch_manifest(&[(digest.clone(), layer.len() as u64, GZIP)], "amd64");
    let mut image = Served::default();
    image.blobs.insert(digest, layer);
    image.manifests.insert(
        "latest".to_owned(),
        ServedManifest {
            media_type: OCI_MANIFEST.to_owned(),
            body: manifest,
        },
    );
    state.images.insert(repo.to_owned(), image);
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

    fn raw_hardlink(&mut self, name: &str, target: &str) {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Link);
        header.set_size(0);
        header.set_mode(0o644);
        let bytes = header.as_mut_bytes();
        bytes[..name.len()].copy_from_slice(name.as_bytes());
        bytes[157..157 + target.len()].copy_from_slice(target.as_bytes());
        header.set_cksum();
        self.builder
            .append(&header, io::empty())
            .expect("append a raw hardlink entry");
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

pub fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes).expect("gzip the layer");
    encoder.finish().expect("finish the gzip layer")
}

fn zstd(bytes: &[u8]) -> Vec<u8> {
    zstd::stream::encode_all(bytes, 3).expect("zstd the layer")
}

pub fn sha256_hex(bytes: &[u8]) -> String {
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
