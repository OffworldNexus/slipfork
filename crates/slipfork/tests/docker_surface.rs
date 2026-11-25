//! Lab E2E for `slipfork docker` credentials: a real loopback bearer registry, a real 0600
//! store, and the shipped binary.
//!
//! The listener speaks the exact `Distribution v2` shape: `/v2/` answers a `401` Bearer
//! challenge, `/token` only mints a token for the one Basic credential, and a wrong password is
//! refused. Every invocation uses a disk-backed scratch root under `/var/tmp`; there is no mock,
//! fake clock, or in-memory vault.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde_json::Value;

#[path = "support/registry.rs"]
mod registry;

const USER: &str = "octo";
const SECRET: &str = "s3cret";
const TOKEN: &str = "lab-token";
const LONG_HOST: &str = "registry.eu-west-1.example.internal:5000";
const LONG_USER: &str = "builder";
const LONG_SECRET: &str = "west-secret";

struct Lab {
    root: PathBuf,
}

impl Lab {
    fn new(prefix: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock reads after the epoch")
            .as_nanos();
        let root = PathBuf::from("/var/tmp").join(format!(
            "slipfork-docker-{prefix}-{}-{nanos}",
            std::process::id()
        ));
        for dir in ["home", "run", "state", "config", "data"] {
            fs::create_dir_all(root.join(dir)).expect("create the docker lab root");
        }
        Self { root }
    }

    fn store(&self) -> PathBuf {
        self.root.join("config/slipfork/registries.json")
    }

    fn run(&self, args: &[&str], stdin_line: Option<&str>) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_slipfork"));
        command
            .args(args)
            .current_dir(&self.root)
            .env("NO_COLOR", "1")
            .env("TERM", "dumb")
            .env("HOME", self.root.join("home"))
            .env("XDG_RUNTIME_DIR", self.root.join("run"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_DATA_HOME", self.root.join("data"))
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

impl Drop for Lab {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[derive(Default)]
struct RegistryState {
    token_grants: u64,
    token_refusals: u64,
    scopes: Vec<Option<String>>,
}

/// A loopback registry that challenges `/v2/` and only mints `TOKEN` for `USER:SECRET`.
struct BearerRegistry {
    addr: String,
    stop: Arc<AtomicBool>,
    state: Arc<Mutex<RegistryState>>,
    accept: Option<JoinHandle<()>>,
}

impl BearerRegistry {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback registry");
        let addr = listener
            .local_addr()
            .expect("read the loopback address")
            .to_string();
        listener
            .set_nonblocking(true)
            .expect("make the loopback listener non-blocking");
        let stop = Arc::new(AtomicBool::new(false));
        let state = Arc::new(Mutex::new(RegistryState::default()));
        let accept = {
            let stop = Arc::clone(&stop);
            let state = Arc::clone(&state);
            let realm = format!("http://{addr}/token");
            thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => serve(stream, &realm, &state),
                        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
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

    fn grants(&self) -> u64 {
        self.state
            .lock()
            .expect("lock the registry state")
            .token_grants
    }

    fn refusals(&self) -> u64 {
        self.state
            .lock()
            .expect("lock the registry state")
            .token_refusals
    }

    fn scopes(&self) -> Vec<Option<String>> {
        self.state
            .lock()
            .expect("lock the registry state")
            .scopes
            .clone()
    }
}

impl Drop for BearerRegistry {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr.as_str());
        if let Some(handle) = self.accept.take() {
            let _ = handle.join();
        }
    }
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
    match path {
        "/v2/" => registry::respond(
            &mut stream,
            401,
            "Unauthorized",
            &[(
                "WWW-Authenticate",
                registry::challenge(realm, "lab", "repository:private/app:pull"),
            )],
            b"",
        ),
        "/token" => {
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
            let mut guard = state.lock().expect("lock the registry state");
            guard.scopes.push(scope);
            if headers.get("authorization").map(String::as_str) == Some(expected.as_str()) {
                guard.token_grants += 1;
                drop(guard);
                registry::respond(
                    &mut stream,
                    200,
                    "OK",
                    &[("Content-Type", "application/json".to_owned())],
                    format!("{{\"token\":\"{TOKEN}\"}}").as_bytes(),
                );
            } else {
                guard.token_refusals += 1;
                drop(guard);
                registry::respond(&mut stream, 401, "Unauthorized", &[], b"");
            }
        }
        _ => registry::respond(&mut stream, 404, "Not Found", &[], b""),
    }
}

fn text(output: &Output) -> String {
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    text
}

fn assert_no_secret(label: &str, text: &str) {
    assert!(
        !text.contains(SECRET),
        "the registry password leaked into {label}: {text}"
    );
}

fn assert_no_ansi(label: &str, text: &str) {
    assert!(
        !text.contains('\u{1b}'),
        "{label} must emit zero ANSI under --json or NO_COLOR: {text:?}"
    );
}

/// Visible text with every CSI sequence removed, so column starts are measured like a terminal.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            out.push(ch);
            continue;
        }
        if chars.peek() == Some(&'[') {
            chars.next();
            for ch in chars.by_ref() {
                if ('@'..='~').contains(&ch) {
                    break;
                }
            }
        } else {
            chars.next();
        }
    }
    out
}

fn login_args(host: &str, json: bool) -> Vec<&str> {
    let mut args = vec![
        "docker",
        "login",
        host,
        "--username",
        USER,
        "--password-stdin",
    ];
    if json {
        args.push("--json");
    }
    args
}

#[test]
fn a_malformed_credential_store_fails_closed_before_any_verb() {
    let lab = Lab::new("malformed");
    fs::create_dir_all(lab.root.join("config/slipfork")).expect("create the config directory");
    fs::write(lab.store(), "{not json").expect("write a malformed store");

    let list = lab.run(&["docker", "list"], None);
    assert!(!list.status.success(), "{}", text(&list));
    let message = text(&list);
    assert!(message.contains("error:"), "{message}");
    assert_eq!(message.matches("next:").count(), 1, "{message}");
    assert!(!message.contains("No stored registries"), "{message}");

    let login = lab.run(&login_args("127.0.0.1:1", false), Some("never-stored"));
    assert!(!login.status.success(), "{}", text(&login));
    let message = text(&login);
    assert_eq!(message.matches("next:").count(), 1, "{message}");
    assert!(!message.contains("Logged in"), "{message}");
    assert!(!message.contains("never-stored"), "{message}");
}

#[test]
fn a_corrupt_built_index_fails_closed_instead_of_listing_nothing() {
    let lab = Lab::new("corrupt-built");
    let built = lab.root.join("data/slipfork/store/images/built.json");
    fs::create_dir_all(built.parent().expect("built index parent"))
        .expect("create the image store");
    fs::write(&built, "{not json").expect("write a corrupt built index");

    let images = lab.run(&["docker", "images"], None);
    assert!(!images.status.success(), "{}", text(&images));
    let message = text(&images);
    assert_eq!(
        message.lines().count(),
        2,
        "a corrupt inventory is one line plus one next: {message:?}"
    );
    assert!(
        message
            .lines()
            .next()
            .is_some_and(|line| line.starts_with("error:")),
        "{message}"
    );
    assert_eq!(message.matches("next:").count(), 1, "{message}");
    assert!(message.contains("built.json"), "{message}");
    assert!(
        !message.contains("No OCI images cached"),
        "a corrupt index must never degrade to an empty inventory: {message}"
    );
}

#[test]
fn docker_images_shortens_digests_to_twelve_dimmed_cells_with_latched_columns() {
    let lab = Lab::new("digest-cells");
    let images = lab.root.join("data/slipfork/store/images");
    fs::create_dir_all(&images).expect("create the image cache");
    let long_hex = "1".repeat(64);
    let other_hex = "2".repeat(64);
    let short = "abc123";
    let reference = "registry.eu-west-1.example.internal:5000/team/service:latest";
    assert!(
        reference.chars().count() > 40,
        "the long name proves widths latch"
    );
    fs::create_dir_all(images.join(format!("oci/sha256-{long_hex}")))
        .expect("create the pulled image directory");
    let mut refs = BTreeMap::new();
    refs.insert(reference.to_owned(), format!("sha256:{long_hex}"));
    fs::write(
        images.join("oci/refs.json"),
        serde_json::to_vec(&refs).expect("encode refs"),
    )
    .expect("write refs");
    fs::write(
        images.join("built.json"),
        serde_json::to_vec(&serde_json::json!({
            "tiny": { "image_id": short },
            "long": { "image_id": format!("sha256:{other_hex}") },
        }))
        .expect("encode built images"),
    )
    .expect("write built images");

    let output = lab.run(&["docker", "images"], None);
    assert!(output.status.success(), "{}", text(&output));
    let shown = text(&output);
    assert_no_ansi("docker images", &shown);
    assert!(!shown.contains('\r'), "a pipe carries zero CR: {shown:?}");
    let lines: Vec<&str> = shown.lines().collect();
    assert!(lines[0].starts_with("SOURCE"), "{shown}");
    let body = &lines[1..];
    assert_eq!(body.len(), 3, "one pull plus two built rows: {shown}");

    let cells: Vec<&str> = ["sha256:111111111111", "sha256:222222222222", short]
        .into_iter()
        .collect();
    let starts: Vec<usize> = body
        .iter()
        .zip(&cells)
        .map(|(line, cell)| {
            let at = line
                .find(cell)
                .unwrap_or_else(|| panic!("`{cell}` must appear untruncated: {line:?}"));
            assert!(
                cell.len() <= 19,
                "a digest cell is the optional prefix plus twelve characters"
            );
            at
        })
        .collect();
    assert!(
        starts.windows(2).all(|pair| pair[0] == pair[1]),
        "the IMAGE column starts at one index: {starts:?}\n{shown}"
    );
    assert!(
        !shown.contains(&long_hex) && !shown.contains(&other_hex),
        "a full digest never reaches the table: {shown}"
    );
}

#[test]
fn docker_build_refusal_is_one_failure_line_and_one_next_with_zero_escapes() {
    let lab = Lab::new("build-refusal");
    // A file where the runtime directory should be fails the connect before any daemon starts.
    let runtime = lab.root.join("run");
    fs::remove_dir_all(&runtime).expect("remove the empty runtime directory");
    fs::write(&runtime, b"not a directory").expect("write the runtime file");

    let output = lab.run(&["docker", "build", "app"], None);
    assert!(!output.status.success(), "{}", text(&output));
    let shown = text(&output);
    assert_no_ansi("docker build refusal", &shown);
    assert_eq!(
        shown.lines().count(),
        2,
        "one line plus one next: {shown:?}"
    );
    assert!(
        shown
            .lines()
            .next()
            .is_some_and(|line| line.starts_with("error:")),
        "{shown}"
    );
    assert_eq!(shown.matches("next:").count(), 1, "{shown}");
    assert_eq!(shown.matches("error:").count(), 1, "{shown}");
}

#[test]
fn exec_without_a_terminal_uses_the_same_one_failure_surface() {
    let lab = Lab::new("exec-refusal");
    let output = lab.run(&["exec", "--interactive", "--", "true"], None);
    assert!(!output.status.success(), "{}", text(&output));
    let shown = text(&output);
    assert_no_ansi("exec refusal", &shown);
    assert_eq!(
        shown.lines().count(),
        2,
        "one line plus one next: {shown:?}"
    );
    assert!(
        shown
            .lines()
            .next()
            .is_some_and(|line| line.starts_with("error:")),
        "{shown}"
    );
    assert_eq!(shown.matches("next:").count(), 1, "{shown}");
}

#[test]
fn bearer_login_is_0600_and_a_wrong_password_never_reaches_disk_or_a_stream() {
    let lab = Lab::new("bearer");
    let registry = BearerRegistry::start();
    let host = registry.addr.clone();
    let store = lab.store();
    let mut transcripts: Vec<(&str, String)> = Vec::new();

    // A wrong password is refused by the Basic-checked token endpoint; nothing is persisted.
    let refused = lab.run(&login_args(&host, false), Some("wrong-pass"));
    assert!(!refused.status.success(), "{}", text(&refused));
    let message = text(&refused);
    assert_eq!(
        message.lines().count(),
        2,
        "a refusal is exactly one line plus one next: {message:?}"
    );
    assert!(
        message
            .lines()
            .next()
            .is_some_and(|line| line.starts_with("error:")),
        "{message}"
    );
    assert_eq!(message.matches("next:").count(), 1, "{message}");
    assert!(!message.contains("wrong-pass"), "{message}");
    assert!(
        !store.exists(),
        "a refused login must leave no credential store: {}",
        store.display()
    );
    assert_eq!(registry.refusals(), 1);
    transcripts.push(("refused login", message));

    // The real Basic credential mints a token and lands in the owner-only store.
    let login = lab.run(&login_args(&host, false), Some(SECRET));
    assert!(login.status.success(), "{}", text(&login));
    let login_text = text(&login);
    assert!(login_text.contains("Logged in"), "{login_text}");
    assert_no_secret("login stdout/stderr", &login_text);
    assert_eq!(registry.grants(), 1);
    assert!(
        registry.scopes().iter().any(Option::is_none),
        "the login probe mints a scope-less token"
    );
    let mode = fs::metadata(&store)
        .expect("login writes the store")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "the credential store is owner-only");
    let stored = fs::read(&store).expect("read the credential store");
    let stored_text = String::from_utf8_lossy(&stored).into_owned();
    assert!(stored_text.contains(USER), "{stored_text}");
    assert!(stored_text.contains(SECRET), "{stored_text}");
    transcripts.push(("successful login", login_text));

    // The same wrong password after a successful login leaves the bytes untouched.
    let wrong_again = lab.run(&login_args(&host, false), Some("wrong-pass"));
    assert!(!wrong_again.status.success(), "{}", text(&wrong_again));
    let message = text(&wrong_again);
    assert_eq!(message.matches("next:").count(), 1, "{message}");
    assert_eq!(
        fs::read(&store).expect("the store still exists"),
        stored,
        "a refused login never rewrites the store"
    );
    assert_eq!(registry.refusals(), 2);
    transcripts.push(("second refusal", message));

    // A second owner-only entry with a much longer host proves dynamic column widths.
    let mut entries: BTreeMap<String, Value> =
        serde_json::from_slice(&stored).expect("the store is one host map");
    entries.insert(
        LONG_HOST.to_owned(),
        serde_json::json!({ "username": LONG_USER, "secret": LONG_SECRET }),
    );
    fs::write(
        &store,
        serde_json::to_vec_pretty(&entries).expect("encode the two-host store"),
    )
    .expect("seed the long host");
    fs::set_permissions(&store, fs::Permissions::from_mode(0o600))
        .expect("keep the seeded store owner-only");
    let stored_with_second = fs::read(&store).expect("read the seeded store");

    let list = lab.run(&["docker", "list"], None);
    assert!(list.status.success(), "{}", text(&list));
    let list_text = text(&list);
    assert_no_ansi("docker list", &list_text);
    assert!(
        !list_text.contains('\r'),
        "a pipe carries zero CR: {list_text:?}"
    );
    transcripts.push(("docker list", list_text.clone()));

    let host_width = [
        host.chars().count(),
        LONG_HOST.chars().count(),
        "HOST".len(),
    ]
    .into_iter()
    .max()
    .expect("one host width");
    let username_column = host_width + 2;
    let stripped: Vec<String> = list_text.lines().map(strip_ansi).collect();
    let header = stripped
        .iter()
        .find(|line| line.trim_start().starts_with("HOST"))
        .unwrap_or_else(|| panic!("docker list must render a HOST header: {list_text:?}"));
    assert!(
        header
            .chars()
            .skip(username_column)
            .collect::<String>()
            .starts_with("USERNAME"),
        "USERNAME must start at visible column {username_column}: {header:?}"
    );
    for (entry_host, entry_user) in [(host.as_str(), USER), (LONG_HOST, LONG_USER)] {
        let line = stripped
            .iter()
            .find(|line| line.contains(entry_host))
            .unwrap_or_else(|| panic!("`{entry_host}` must appear untruncated: {list_text:?}"));
        let padding = host_width - entry_host.chars().count() + 2;
        let expected_prefix = format!("{entry_host}{}", " ".repeat(padding));
        assert!(
            line.starts_with(&expected_prefix),
            "host column must pad to {host_width} then leave exactly two spaces: {line:?}"
        );
        let after = line.chars().skip(username_column).collect::<String>();
        assert!(
            after.starts_with(entry_user),
            "`{entry_user}` must start at visible column {username_column}: {line:?}"
        );
    }

    let list_json = lab.run(&["docker", "list", "--json"], None);
    assert!(list_json.status.success(), "{}", text(&list_json));
    let rows: Value = serde_json::from_slice(&list_json.stdout).expect("list --json is one array");
    let rows = rows.as_array().expect("list --json is an array");
    assert_eq!(rows.len(), 2, "{rows:?}");
    for (entry_host, entry_user) in [(host.as_str(), USER), (LONG_HOST, LONG_USER)] {
        assert!(
            rows.iter()
                .any(|row| row["host"] == entry_host && row["username"] == entry_user),
            "list --json must carry `{entry_host}` / `{entry_user}`: {rows:?}"
        );
    }
    assert_no_secret("docker list --json", &text(&list_json));
    assert_no_ansi("docker list --json", &text(&list_json));
    transcripts.push(("docker list --json", text(&list_json)));

    let images = lab.run(&["docker", "images", "--json"], None);
    assert!(images.status.success(), "{}", text(&images));
    let cached: Value = serde_json::from_slice(&images.stdout).expect("images --json is one array");
    assert_eq!(cached, Value::Array(Vec::new()), "{cached}");
    assert_no_ansi("docker images --json", &text(&images));
    transcripts.push(("docker images --json", text(&images)));

    // A JSON refusal is one typed object with no ANSI and no secret.
    let refused_json = lab.run(&login_args(&host, true), Some("wrong-pass"));
    assert!(!refused_json.status.success(), "{}", text(&refused_json));
    let error: Value =
        serde_json::from_slice(&refused_json.stdout).expect("login --json is one object");
    assert!(error.get("error").is_some(), "{error}");
    assert_no_secret("refused login --json", &text(&refused_json));
    assert_no_ansi("refused login --json", &text(&refused_json));
    assert_eq!(
        fs::read(&store).expect("the seeded store still exists"),
        stored_with_second,
        "a refused JSON login never rewrites the store"
    );
    transcripts.push(("refused login --json", text(&refused_json)));

    // A successful JSON login is exactly one object; logout removes only the named host.
    let login_json = lab.run(&login_args(&host, true), Some(SECRET));
    assert!(login_json.status.success(), "{}", text(&login_json));
    assert_eq!(
        String::from_utf8_lossy(&login_json.stdout).lines().count(),
        1,
        "login --json must be one object: {}",
        text(&login_json)
    );
    let value: Value =
        serde_json::from_slice(&login_json.stdout).expect("login --json is one object");
    assert_eq!(value["host"], host, "{value}");
    assert_eq!(value["stored"], true, "{value}");
    assert_no_secret("successful login --json", &text(&login_json));
    assert_no_ansi("successful login --json", &text(&login_json));
    transcripts.push(("successful login --json", text(&login_json)));

    let logout = lab.run(&["docker", "logout", &host, "--json"], None);
    assert!(logout.status.success(), "{}", text(&logout));
    let removed: Value =
        serde_json::from_slice(&logout.stdout).expect("logout --json is one object");
    assert_eq!(removed["removed"], true, "{removed}");
    assert_no_ansi("docker logout --json", &text(&logout));
    transcripts.push(("docker logout --json", text(&logout)));

    let emptied = fs::read_to_string(&store).expect("logout rewrites the store");
    assert!(!emptied.contains(USER), "{emptied}");
    assert!(!emptied.contains(SECRET), "{emptied}");
    assert!(
        emptied.contains(LONG_HOST) && emptied.contains(LONG_USER),
        "logout only forgets the named host: {emptied}"
    );

    for (label, transcript) in &transcripts {
        assert_no_secret(label, transcript);
    }
}

#[test]
fn help_advertises_the_no_cache_flag_on_up_and_docker_build() {
    let lab = Lab::new("no-cache-help");

    let up = lab.run(&["up", "--help"], None);
    assert!(up.status.success(), "{}", text(&up));
    let up_help = strip_ansi(&text(&up));
    assert!(
        up_help.contains("--no-cache"),
        "`up --help` must advertise the rebuild flag: {up_help}"
    );

    let build = lab.run(&["docker", "build", "--help"], None);
    assert!(build.status.success(), "{}", text(&build));
    let build_raw = text(&build);
    assert!(
        !build_raw.contains('\u{1b}') && !build_raw.contains('\r'),
        "help stays plain under NO_COLOR: {build_raw:?}"
    );
    let build_help = strip_ansi(&build_raw);
    assert!(
        build_help.contains("--no-cache"),
        "`docker build --help` must advertise the rebuild flag: {build_help}"
    );
}
