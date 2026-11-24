//! Protected live journey: a private loopback `Distribution v2` registry behind a `401` Bearer
//! challenge, the shipped `slipfork` binary, and a real Btrfs image store.
//!
//! `private/base` and `private/tools` are tiny real OCI images (config plus one gzip tar layer).
//! The journey logs in with a Basic-checked `/token`, pulls the base, then builds an image whose
//! `Dockerfile` is `FROM` the base plus `COPY --from` the tools image. Every `/v2/private/...`
//! request must carry the minted bearer token; the credential never appears in any stream, the
//! build report, or the sealed rootfs. A non-Btrfs host or a missing `crun` FAILS CLOSED.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde_json::Value;
use slipfork_image::ImageLayout;
use slipfork_world::{BtrfsEngine, Dirtiness, StoreLayout, WorldEngine, is_btrfs_path};

#[path = "support/registry.rs"]
mod registry;

const USER: &str = "octo";
const SECRET: &str = "s3cret";
const TOKEN: &str = "lab-token";
const OCI_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
const OCI_CONFIG: &str = "application/vnd.oci.image.config.v1+json";
const GZIP_LAYER: &str = "application/vnd.oci.image.layer.v1.tar+gzip";
const MOUNT_REMEDY: &str = "mount -o user_subvol_rm_allowed,noatime <device> <mount>";

// ---------------------------------------------------------------------------------------------
// Fail-closed Btrfs scratch root (copied from the image suite's private support module).
// ---------------------------------------------------------------------------------------------

fn scratch_root() -> PathBuf {
    if let Some(root) = std::env::var_os("SLIPFORK_TEST_ROOT") {
        return PathBuf::from(root);
    }
    let home = std::env::var_os("HOME")
        .expect("fail closed: HOME is unset; export SLIPFORK_TEST_ROOT=<btrfs dir>");
    PathBuf::from(home).join(".cache/slipfork/tests")
}

fn live_run_root(prefix: &str) -> PathBuf {
    let scratch = scratch_root();
    assert!(
        !scratch.starts_with("/tmp"),
        "fail closed: `{}` is RAM-backed tmpfs and has no Btrfs ioctls.\nnext: set SLIPFORK_TEST_ROOT to a Btrfs directory such as ~/.cache/slipfork/tests or /var/tmp/slipfork",
        scratch.display()
    );
    fs::create_dir_all(&scratch).expect("create the Btrfs scratch root");
    match is_btrfs_path(&scratch) {
        Ok(true) => {}
        Ok(false) => panic!(
            "fail closed: `{}` is not on Btrfs (BTRFS_SUPER_MAGIC 0x9123683E missing).\nnext: run on a Btrfs host or export SLIPFORK_TEST_ROOT=<btrfs dir>; {MOUNT_REMEDY}",
            scratch.display()
        ),
        Err(error) => panic!(
            "fail closed: probing `{}` failed: {error}\nnext: {MOUNT_REMEDY}",
            scratch.display()
        ),
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock reads before the epoch")
        .as_nanos();
    let run = scratch.join(format!("{prefix}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&run).expect("create the registry run root");
    run
}

fn require_crun() {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let found = std::env::split_paths(&path)
        .map(|directory| directory.join("crun"))
        .find(|candidate| candidate.is_file());
    assert!(
        found.is_some(),
        "fail closed: `crun` is missing, so `ImageLab::step_host` cannot resolve the live step host.\nnext: install crun (for example `apt install crun`) and rerun the live gate"
    );
}

fn unique_nonce() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    format!("{}-{nanos}", std::process::id())
}

// ---------------------------------------------------------------------------------------------
// Loopback private registry: 401 Bearer challenge, Basic-checked token, bearer-only blobs.
// ---------------------------------------------------------------------------------------------

struct Registry {
    addr: String,
    stop: Arc<AtomicBool>,
    state: Arc<Mutex<RegistryState>>,
    accept: Option<JoinHandle<()>>,
}

#[derive(Default)]
struct RegistryState {
    blobs: BTreeMap<(String, String), Vec<u8>>,
    manifests: BTreeMap<(String, String), Vec<u8>>,
    token_scopes: Vec<Option<String>>,
    bearer_paths: Vec<String>,
    unauthorized: Vec<String>,
}

impl Registry {
    fn start(nonce: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind the private registry");
        let addr = listener
            .local_addr()
            .expect("read the private registry address")
            .to_string();
        listener
            .set_nonblocking(true)
            .expect("make the private registry non-blocking");

        let config = b"{}".to_vec();
        let config_digest = registry::sha256_hex(&config);
        let os_release = format!("ID=slipfork-lab\nLAB_NONCE={nonce}\n").into_bytes();
        let marker = format!("marker-{nonce}\n").into_bytes();
        let mut state = RegistryState::default();
        seed(
            &mut state,
            "private/base",
            &["etc"],
            &[("etc/os-release", os_release.as_slice())],
            &config,
            &config_digest,
        );
        seed(
            &mut state,
            "private/tools",
            &[],
            &[("marker", marker.as_slice())],
            &config,
            &config_digest,
        );

        let state = Arc::new(Mutex::new(state));
        let stop = Arc::new(AtomicBool::new(false));
        let accept = {
            let stop = Arc::clone(&stop);
            let state = Arc::clone(&state);
            let realm = format!("http://{addr}/token");
            thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => serve(stream, &realm, &state),
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => break,
                    }
                }
            })
        };
        Self {
            addr,
            stop,
            state,
            accept: Some(accept),
        }
    }

    fn reference(&self, repository: &str) -> String {
        format!("{}/{repository}:latest", self.addr)
    }
}

impl Drop for Registry {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr.as_str());
        if let Some(handle) = self.accept.take() {
            let _ = handle.join();
        }
    }
}

fn seed(
    state: &mut RegistryState,
    repository: &str,
    directories: &[&str],
    files: &[(&str, &[u8])],
    config: &[u8],
    config_digest: &str,
) {
    let layer = gzip(&tar_layer(directories, files));
    let layer_digest = registry::sha256_hex(&layer);
    let manifest = serde_json::to_vec(&serde_json::json!({
        "schemaVersion": 2,
        "mediaType": OCI_MANIFEST,
        "config": {
            "mediaType": OCI_CONFIG,
            "digest": config_digest,
            "size": config.len(),
        },
        "layers": [{
            "mediaType": GZIP_LAYER,
            "digest": layer_digest,
            "size": layer.len(),
        }],
    }))
    .expect("encode the manifest");
    state
        .blobs
        .insert((repository.to_owned(), layer_digest), layer);
    state.blobs.insert(
        (repository.to_owned(), config_digest.to_owned()),
        config.to_vec(),
    );
    state
        .manifests
        .insert((repository.to_owned(), "latest".to_owned()), manifest);
}

fn serve(mut stream: TcpStream, realm: &str, state: &Mutex<RegistryState>) {
    let Some((line, headers)) = registry::read_head(&mut stream) else {
        return;
    };
    let target = line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    let path = target.split('?').next().unwrap_or(&target);
    if path == "/token" {
        let expected = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("{USER}:{SECRET}"))
        );
        let scope = target
            .split_once('?')
            .and_then(|(_, query)| {
                query
                    .split('&')
                    .find_map(|part| part.strip_prefix("scope="))
            })
            .map(str::to_owned);
        let mut guard = state.lock().expect("lock the private registry");
        guard.token_scopes.push(scope);
        if headers.get("authorization").map(String::as_str) == Some(expected.as_str()) {
            drop(guard);
            registry::respond(
                &mut stream,
                200,
                "OK",
                &[("Content-Type", "application/json".to_owned())],
                format!("{{\"token\":\"{TOKEN}\"}}").as_bytes(),
            );
        } else {
            drop(guard);
            registry::respond(&mut stream, 401, "Unauthorized", &[], b"");
        }
        return;
    }
    if path == "/v2/" {
        registry::respond(
            &mut stream,
            401,
            "Unauthorized",
            &[(
                "WWW-Authenticate",
                registry::challenge(realm, "lab", "repository:private/base:pull"),
            )],
            b"",
        );
        return;
    }
    let Some(rest) = path.strip_prefix("/v2/") else {
        registry::respond(&mut stream, 404, "Not Found", &[], b"");
        return;
    };
    let Some(repository) = rest
        .split_once("/manifests/")
        .map(|(repository, _)| repository)
        .or_else(|| rest.split_once("/blobs/").map(|(repository, _)| repository))
    else {
        registry::respond(&mut stream, 404, "Not Found", &[], b"");
        return;
    };
    let expected = format!("Bearer {TOKEN}");
    if headers.get("authorization").map(String::as_str) != Some(expected.as_str()) {
        let scope = format!("repository:{repository}:pull");
        state
            .lock()
            .expect("lock the private registry")
            .unauthorized
            .push(path.to_owned());
        registry::respond(
            &mut stream,
            401,
            "Unauthorized",
            &[(
                "WWW-Authenticate",
                registry::challenge(realm, "lab", &scope),
            )],
            b"",
        );
        return;
    }
    state
        .lock()
        .expect("lock the private registry")
        .bearer_paths
        .push(path.to_owned());
    if let Some((repository, reference)) = rest.split_once("/manifests/") {
        let body = state
            .lock()
            .expect("lock the private registry")
            .manifests
            .get(&(repository.to_owned(), reference.to_owned()))
            .cloned();
        match body {
            Some(body) => registry::respond(
                &mut stream,
                200,
                "OK",
                &[
                    ("Content-Type", OCI_MANIFEST.to_owned()),
                    ("Docker-Content-Digest", registry::sha256_hex(&body)),
                ],
                &body,
            ),
            None => registry::respond(&mut stream, 404, "Not Found", &[], b""),
        }
        return;
    }
    if let Some((repository, digest)) = rest.split_once("/blobs/") {
        let blob = state
            .lock()
            .expect("lock the private registry")
            .blobs
            .get(&(repository.to_owned(), digest.to_owned()))
            .cloned();
        match blob {
            Some(blob) => registry::respond(
                &mut stream,
                200,
                "OK",
                &[("Content-Type", "application/octet-stream".to_owned())],
                &blob,
            ),
            None => registry::respond(&mut stream, 404, "Not Found", &[], b""),
        }
        return;
    }
    registry::respond(&mut stream, 404, "Not Found", &[], b"");
}

fn tar_layer(directories: &[&str], files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for directory in directories {
        let mut header = tar::Header::new_gnu();
        header
            .set_path(format!("{directory}/"))
            .expect("directory path");
        header.set_entry_type(tar::EntryType::Directory);
        header.set_size(0);
        header.set_mode(0o755);
        header.set_mtime(0);
        header.set_cksum();
        builder
            .append(&header, std::io::empty())
            .expect("append a directory");
    }
    for (path, data) in files {
        let mut header = tar::Header::new_gnu();
        header.set_path(path).expect("file path");
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_cksum();
        builder.append(&header, *data).expect("append a file");
    }
    builder.into_inner().expect("tar bytes")
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes).expect("gzip the layer");
    encoder.finish().expect("finish the gzip layer")
}

// ---------------------------------------------------------------------------------------------
// The shipped binary in an isolated root.
// ---------------------------------------------------------------------------------------------

struct Lab {
    root: PathBuf,
    project: PathBuf,
    home: PathBuf,
    config: PathBuf,
    state: PathBuf,
    data: PathBuf,
    runtime: PathBuf,
    sealed: Mutex<Vec<PathBuf>>,
    references: Mutex<Vec<String>>,
    images_root: Mutex<Option<PathBuf>>,
}

impl Lab {
    fn new(prefix: &str) -> Self {
        let root = live_run_root(prefix);
        let project = root.join("project");
        let home = root.join("home");
        let config = root.join("config");
        let state = root.join("state");
        let data = root.join("data");
        let runtime = root.join("runtime");
        for directory in [&project, &home, &config, &state, &data, &runtime] {
            fs::create_dir_all(directory).expect("create an isolated lab directory");
        }
        Self {
            root,
            project,
            home,
            config,
            state,
            data,
            runtime,
            sealed: Mutex::new(Vec::new()),
            references: Mutex::new(Vec::new()),
            images_root: Mutex::new(None),
        }
    }

    /// Remembers one `refs.json` key so Drop destroys exactly this run's sealed image.
    fn track_reference(&self, reference: &str) {
        self.references
            .lock()
            .expect("lock the tracked references")
            .push(reference.to_owned());
    }

    /// Remembers one sealed subvolume and the shared images root its siblings live in.
    fn track_sealed(&self, rootfs: &Path) {
        if let Some(images_root) = rootfs.parent() {
            *self.images_root.lock().expect("lock the images root") =
                Some(images_root.to_path_buf());
        }
        self.sealed
            .lock()
            .expect("lock the sealed rootfs list")
            .push(rootfs.to_path_buf());
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        let images_root = self.images_root.lock().ok().and_then(|slot| slot.clone());
        if let Some(images_root) = images_root
            && let Some(store_root) = images_root.parent()
        {
            let engine = BtrfsEngine::new(StoreLayout::at(store_root));
            let refs: BTreeMap<String, String> = fs::read(images_root.join("refs.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_default();
            let layout = ImageLayout::new(images_root.clone());
            let references = self
                .references
                .lock()
                .map(|references| references.clone())
                .unwrap_or_default();
            for reference in references {
                if let Some(digest) = refs.get(&reference) {
                    let _ = engine.destroy(&layout.image(digest), Dirtiness::Dirty, true);
                }
            }
            let sealed = self
                .sealed
                .lock()
                .map(|sealed| sealed.clone())
                .unwrap_or_default();
            for rootfs in sealed {
                let _ = engine.destroy(&rootfs, Dirtiness::Dirty, true);
            }
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl Lab {
    fn run(&self, args: &[&str], stdin_line: Option<&str>) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_slipfork"));
        command
            .args(args)
            .current_dir(&self.project)
            .env("NO_COLOR", "1")
            .env("TERM", "dumb")
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.config)
            .env("XDG_STATE_HOME", &self.state)
            .env("XDG_DATA_HOME", &self.data)
            .env("XDG_RUNTIME_DIR", &self.runtime)
            .stdin(if stdin_line.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("the slipfork binary must run");
        if let Some(secret) = stdin_line {
            let mut stdin = child.stdin.take().expect("login stdin");
            stdin
                .write_all(format!("{secret}\n").as_bytes())
                .expect("write one password line");
        }
        child.wait_with_output().expect("wait for slipfork")
    }
}

fn transcript(output: &Output) -> String {
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    text
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn assert_no_secret_under(root: &Path, needle: &[u8]) {
    let entries = fs::read_dir(root)
        .unwrap_or_else(|error| panic!("read the sealed rootfs `{}`: {error}", root.display()));
    for entry in entries {
        let path = entry.expect("sealed rootfs entry").path();
        let metadata = fs::symlink_metadata(&path).expect("read rootfs metadata");
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            assert_no_secret_under(&path, needle);
        } else if metadata.is_file() {
            let bytes = fs::read(&path).expect("read a sealed rootfs file");
            assert!(
                !contains(&bytes, needle),
                "the registry credential leaked into the built rootfs `{}`",
                path.display()
            );
        }
    }
}

#[test]
fn private_registry_login_pull_and_copy_from_build_never_leak_the_credential() {
    require_crun();
    let nonce = unique_nonce();
    let registry = Registry::start(&nonce);
    let lab = Lab::new("registry-live");
    let base = registry.reference("private/base");
    let tools = registry.reference("private/tools");
    lab.track_reference(&base);
    lab.track_reference(&tools);

    fs::write(
        lab.project.join("Forkfile"),
        "# forkfile: 0.1\nimage app . {\n  dockerfile Dockerfile\n}\n",
    )
    .expect("write the Forkfile");
    fs::write(
        lab.project.join("Dockerfile"),
        format!("FROM {base}\nCOPY --from={tools} /marker /marker\n"),
    )
    .expect("write the Dockerfile");

    let mut transcripts: Vec<(&str, String)> = Vec::new();

    let login = lab.run(
        &[
            "docker",
            "login",
            &registry.addr,
            "--username",
            USER,
            "--password-stdin",
        ],
        Some(SECRET),
    );
    assert!(
        login.status.success(),
        "docker login failed: {}",
        transcript(&login)
    );
    transcripts.push(("docker login", transcript(&login)));

    let store = lab.config.join("slipfork/registries.json");
    let mode = fs::metadata(&store)
        .expect("login writes the credential store")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "the live credential store is owner-only");
    let stored = fs::read(&store).expect("read the credential store");
    assert!(
        contains(&stored, SECRET.as_bytes()),
        "the owner-only store must contain the credential it owns"
    );

    let pull = lab.run(&["docker", "pull", &base, "--json"], None);
    assert!(
        pull.status.success(),
        "docker pull failed: {}",
        transcript(&pull)
    );
    let pull_text = String::from_utf8_lossy(&pull.stdout).into_owned();
    let pulled = pull_text
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|value| value.get("rootfs").is_some())
        .unwrap_or_else(|| panic!("docker pull --json must end with its report: {pull_text}"));
    lab.track_sealed(Path::new(
        pulled["rootfs"]
            .as_str()
            .expect("the pull report carries its sealed rootfs"),
    ));
    transcripts.push(("docker pull", transcript(&pull)));

    let build = lab.run(&["docker", "build", "app", "--json"], None);
    assert!(
        build.status.success(),
        "docker build failed: {}",
        transcript(&build)
    );
    let build_text = String::from_utf8_lossy(&build.stdout).into_owned();
    assert!(
        !build_text.contains('\u{1b}'),
        "docker build --json must emit zero ANSI: {build_text:?}"
    );
    let report = build_text
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|value| value.get("rootfs").is_some())
        .unwrap_or_else(|| {
            panic!("docker build --json must end with its report object: {build_text}")
        });
    let image_id = report["image_id"]
        .as_str()
        .expect("the build report carries an image id");
    assert!(
        image_id.starts_with("sha256:") && image_id.len() > "sha256:".len(),
        "the build must print a real image id: {image_id}"
    );
    let rootfs = PathBuf::from(
        report["rootfs"]
            .as_str()
            .expect("the build report carries its sealed rootfs"),
    );
    assert!(
        rootfs.is_dir(),
        "the sealed rootfs exists: {}",
        rootfs.display()
    );
    lab.track_sealed(&rootfs);
    let marker = fs::read(rootfs.join("marker")).expect("COPY --from carries /marker");
    assert_eq!(marker, format!("marker-{nonce}\n").as_bytes());
    let os_release = fs::read_to_string(rootfs.join("etc/os-release"))
        .expect("the private base layer carries /etc/os-release");
    assert!(
        os_release.contains(&nonce),
        "the built rootfs must come from this run's base image: {os_release}"
    );
    transcripts.push(("docker build", transcript(&build)));

    let images = lab.run(&["docker", "images", "--json"], None);
    assert!(
        images.status.success(),
        "docker images --json failed: {}",
        transcript(&images)
    );
    let rows: Value =
        serde_json::from_slice(&images.stdout).expect("docker images --json is one array");
    let rows = rows.as_array().expect("docker images --json is an array");
    assert!(
        rows.iter().any(|row| {
            row["source"] == "pull"
                && row["reference"]
                    .as_str()
                    .is_some_and(|reference| reference.ends_with("/private/base:latest"))
        }),
        "docker images must list the pulled private base: {rows:?}"
    );
    assert!(
        rows.iter().any(|row| {
            row["source"] == "build" && row["component"] == "app" && row["image_id"] == image_id
        }),
        "docker images must list the built component: {rows:?}"
    );
    transcripts.push(("docker images", transcript(&images)));

    {
        let state = registry.state.lock().expect("lock the private registry");
        for scope in [
            "repository:private/base:pull",
            "repository:private/tools:pull",
        ] {
            assert!(
                state
                    .token_scopes
                    .iter()
                    .flatten()
                    .any(|minted| minted == scope),
                "the token endpoint must mint `{scope}`; saw {:?}",
                state.token_scopes
            );
        }
        assert!(
            state
                .bearer_paths
                .iter()
                .any(|path| path.starts_with("/v2/private/base/")),
            "base blobs/manifests require the bearer token: {:?}",
            state.bearer_paths
        );
        assert!(
            state
                .bearer_paths
                .iter()
                .any(|path| path.starts_with("/v2/private/tools/")),
            "tools blobs/manifests require the bearer token: {:?}",
            state.bearer_paths
        );
        assert!(
            state
                .unauthorized
                .iter()
                .any(|path| path.starts_with("/v2/private/")),
            "the first private request is challenged before the token is minted"
        );
    }

    for (label, text) in &transcripts {
        assert!(
            !text.contains(SECRET),
            "the registry credential leaked into {label}: {text}"
        );
    }
    assert_no_secret_under(&rootfs, SECRET.as_bytes());
}
