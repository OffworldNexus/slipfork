//! Protected `slipforkd` OSPP v1 journey: pairing store, encrypted subscription with local-vault
//! `claims`, live `env_updated`, and the streaming HPKE `dump_ready` restore (`M3.29`,
//! `docs/design/ospp.md`, `ADR-0008` §1–§3).
//!
//! One rich journey drives the real `slipfork` + `slipforkd` binaries against the in-tree `ospp-v1`
//! reference server from the unpublished `slipfork-ospp-testkit` crate (a loopback test double,
//! never a production fallback): `slipfork auth` `#handle=` pairing → `<state_dir>/ospp.json`
//! written `0600` → `slipforkd` reboot with **no** `--ospp-endpoint` flag survives via the store →
//! encrypted `subscribe` with local-vault `claims` → zero secret bytes on disk → a live
//! `env_updated` restarts only `api` → the streaming sealed `dump_ready` archive restores into a
//! real `postgres:18` with no plaintext dump file. The currently separate fail-closed cases (an
//! unpaired remote strategy, an unpaired `--refresh-dump`, an optional and a malformed
//! `slipfork.toml`) are folded into the first and the last boot. `postgres:18` and the pinned
//! static `crun` are real; a missing Docker/crun dependency fails loudly.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use slipfork_edge::{DaemonControl, DaemonControlClient};
use slipfork_ospp_testkit::{ReferenceOsppServer, ReferenceServerConfig};
use slipfork_wire::control::{ExecTargetRequest, WorldSelector};

pub mod support;
use support::{Daemon, assert_success, repo_with_manifest, run_git, stderr, stdout};

/// Two components, a real `postgres:18`, and a local-vault `vars::DEBUG` override. The server
/// resolves the unclaimed remote values, while the claimed local ones stay byte-verbatim
/// templates for the daemon. The `pg_databases.main` dump is the streaming-restore subject.
const TWO_TIER_SNOW: &str = r#"
images:
  - id: app
    context: .
    dockerfile: Dockerfile

components:
  - id: api
    image: app
    envs: [api]
    run_command: "exec sleep 300"
  - id: worker
    image: app
    envs: [worker]
    run_command: "exec sleep 300"

ingresses:
  - match:
      prefix: "/"
    target:
      component: api
      port: 8000

resources:
  pg_clusters:
    - id: main
      version: "18"
  pg_databases:
    - id: main
      cluster: main

modes:
  local:
    envs:
      api:
        DEBUG: '${{ vars::DEBUG }}'
        REMOTE_VAR: '${{ vars::REMOTE_VAR }}'
        REMOTE_TOKEN: '${{ secrets::REMOTE_TOKEN }}'
        LOCAL_PASSWORD: '${{ secrets::LOCAL_PASSWORD }}'
      worker:
        WORKER_VAR: '${{ vars::WORKER_VAR }}'
"#;

const PLAIN_SNOW: &str = r#"
images:
  - id: app
    context: .
    dockerfile: Dockerfile

components:
  - id: api
    image: app
    run_command: "exec sleep 300"

ingresses:
  - match:
      prefix: "/"
    target:
      component: api
      port: 8000

modes:
  local:
    envs:
      app:
        BASE_URL: '${{ resources::app.url }}'
"#;

/// A known remote-secret plaintext with non-ASCII bytes, so the byte scan proves the real UTF-8
/// value never reaches disk.
const REMOTE_TOKEN: &str = "remote-token-\u{1f510}";

fn scratch_dir() -> tempfile::TempDir {
    support::disk_tempdir("ospp-")
}

/// Read the daemon's machine key id from a real `slipfork auth` pairing.
fn pair_and_read_key(daemon: &Daemon, repo: &Path) -> String {
    let output = daemon.cli(repo, &["auth"]);
    assert_success(&output, "slipfork auth");
    let text = stdout(&output);
    text.split_whitespace()
        .find(|token| token.starts_with("p256:"))
        .unwrap_or_else(|| panic!("auth must print the paired key id: {text}"))
        .trim_start_matches("p256:")
        .to_owned()
}

/// The server-supplied `#handle=` pairing URL `auth` printed and handed to the real opener.
fn pairing_url(output: &Output) -> String {
    let printed = stdout(output);
    let line = printed
        .lines()
        .find(|line| line.contains("#handle="))
        .unwrap_or_else(|| panic!("auth must print the fragment URL: {printed}"));
    line.trim().to_owned()
}

/// A `0600` local vault holding the local `vars::DEBUG` override and a local credential.
fn seed_local_vault(state_dir: &Path, project: &str, debug: &str, password: &str) {
    let path = state_dir
        .join("xdg-data/slipfork/vaults")
        .join(format!("{project}.json"));
    fs::create_dir_all(path.parent().expect("vault parent")).expect("create vault dir");
    fs::write(
        &path,
        format!(
            "{{\n  \"vars\": {{ \"DEBUG\": \"{debug}\" }},\n  \"secrets\": {{ \"LOCAL_PASSWORD\": \"{password}\" }}\n}}\n"
        ),
    )
    .expect("write local vault");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("chmod vault");
}

/// The `<state_dir>/ospp.json` pairing record, its `0600` mode, and its decoded public fields.
fn read_pairing_record(state_dir: &Path) -> (String, serde_json::Value) {
    let path = state_dir.join("ospp.json");
    assert!(
        path.is_file(),
        "`slipfork auth` must write {}",
        path.display()
    );
    let mode = fs::metadata(&path)
        .expect("ospp.json metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "ospp.json must be 0600");
    let text = fs::read_to_string(&path).expect("read ospp.json");
    let record: serde_json::Value = serde_json::from_str(&text).expect("ospp.json is JSON");
    (text, record)
}

/// Every regular file under `root` whose **first** bytes match one of `magics`.
fn scan_tree_headers(root: &Path, magics: &[&[u8]]) -> Vec<PathBuf> {
    fn walk(directory: &Path, magics: &[&[u8]], hits: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(directory) else {
            return;
        };
        let longest = magics.iter().map(|magic| magic.len()).max().unwrap_or(0);
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, magics, hits);
            } else if path.is_file() {
                let mut head = vec![0u8; longest];
                let read = fs::File::open(&path)
                    .and_then(|mut file| file.read(&mut head))
                    .unwrap_or(0);
                if magics
                    .iter()
                    .any(|magic| read >= magic.len() && &head[..magic.len()] == *magic)
                {
                    hits.push(path);
                }
            }
        }
    }
    let mut hits = Vec::new();
    walk(root, magics, &mut hits);
    hits
}

/// Every regular file under `root` whose bytes contain `needle`.
fn scan_tree(root: &Path, needle: &[u8]) -> Vec<PathBuf> {
    fn walk(directory: &Path, needle: &[u8], hits: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(directory) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, needle, hits);
            } else if path.is_file() {
                let mut bytes = Vec::new();
                if fs::File::open(&path)
                    .and_then(|mut file| file.read_to_end(&mut bytes))
                    .is_ok()
                    && bytes.windows(needle.len()).any(|window| window == needle)
                {
                    hits.push(path);
                }
            }
        }
    }
    let mut hits = Vec::new();
    walk(root, needle, &mut hits);
    hits
}

/// Run one service entry, retrying while the postmaster is still in crash recovery.
fn run_service_entry_when_ready(
    client: &DaemonControlClient,
    repo: &Path,
    service: &str,
    argv: &[&str],
) -> Output {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let attempt = run_service_entry(client, repo, service, argv);
        if attempt.status.success()
            || !String::from_utf8_lossy(&attempt.stderr).contains("starting up")
            || Instant::now() >= deadline
        {
            return attempt;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Run one service entry exactly like the thin CLI does (`crun exec` inside the container).
fn run_service_entry(
    client: &DaemonControlClient,
    repo: &Path,
    service: &str,
    argv: &[&str],
) -> Output {
    let target = client
        .exec_target(&ExecTargetRequest {
            cwd: repo.to_path_buf(),
            world: None,
            service: Some(service.to_owned()),
            argv: argv.iter().map(|arg| (*arg).to_owned()).collect(),
        })
        .expect("a running service must be enterable through the typed control port");
    let mut args = target.entry.args_prefix.clone();
    if args.last().map(String::as_str) == Some("--") {
        args.pop();
    }
    let mut command = Command::new(&target.entry.program);
    command
        .args(&args)
        .args(&target.target_argv)
        .current_dir(&target.entry.cwd)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, value) in target.env.iter() {
        command.env(name, value.as_ref());
    }
    command.output().expect("run service entry")
}

/// `zstd`-compress a `pg_dump -Fc` archive (the OSPP dump stream is zstd-compressed).
fn zstd_compress(archive: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut compressor = Command::new("zstd")
        .args(["-q", "-c"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn zstd");
    compressor
        .stdin
        .take()
        .expect("zstd stdin")
        .write_all(archive)
        .expect("write zstd stdin");
    let compressed = compressor.wait_with_output().expect("zstd output");
    assert!(compressed.status.success(), "zstd compression failed");
    compressed.stdout
}

/// One `--json ps` row's uptime in whole seconds (`0` when the service is missing).
fn uptime(rows: &slipfork_wire::control::WorldProcessReport, id: &str) -> u64 {
    rows.services
        .iter()
        .find(|row| row.info.id == id)
        .and_then(|row| row.info.uptime_secs)
        .unwrap_or(0)
}

#[test]
fn ospp_pairing_store_reboot_subscribe_push_and_sealed_dump_journey() {
    let tmp = scratch_dir();
    let scratch = tmp.path();
    let repo = repo_with_manifest(scratch, "acme-two-tier", TWO_TIER_SNOW);
    run_git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "git@example.com:acme/two-tier.git",
        ],
    );

    // --- boot 1: the fail-closed unpaired cases, then the pairing itself ----------------
    // A permissive reference server mints the pairing; no `auth` has run yet, so this daemon has
    // no authenticated session and an OSPP-backed resource must refuse instead of degrading.
    let discovery = ReferenceOsppServer::start(ReferenceServerConfig::default());
    let key = {
        let mut daemon = Daemon::start_with_ospp(scratch, discovery.endpoint());

        fs::write(
            repo.join("slipfork.toml"),
            "project = \"acme-two-tier\"\n[resources.pg_databases.main]\nlocal = \"remote\"\n",
        )
        .expect("write remote strategy");
        let remote = daemon.cli(&repo, &["up"]);
        assert!(
            !remote.status.success(),
            "an unpaired `local = \"remote\"` must fail closed"
        );
        assert!(
            stderr(&remote).contains("requires an authenticated OSPP session"),
            "the refusal must name the missing session: {}",
            stderr(&remote)
        );

        fs::write(repo.join("slipfork.toml"), "project = \"acme-two-tier\"\n")
            .expect("restore default strategy");
        let refresh = daemon.cli(&repo, &["up", "--refresh-dump"]);
        assert!(
            !refresh.status.success(),
            "an unpaired --refresh-dump must fail closed"
        );
        assert!(
            stderr(&refresh).contains("requires an authenticated OSPP session"),
            "the refusal must name the missing session: {}",
            stderr(&refresh)
        );

        // The real pairing: the CLI prints exactly the server-supplied fragment URL, hands the
        // same URL to the real opener, and persists only the public coordinates at `0600`.
        let auth = daemon.cli(&repo, &["auth"]);
        let url = pairing_url(&auth);
        let expected_prefix = format!("http://{}/pair#handle=", discovery.dump_addr());
        assert!(
            url.starts_with(&expected_prefix),
            "the daemon must print the server-supplied URL, never one it computes: {url}"
        );
        let handle = url.strip_prefix(&expected_prefix).expect("fragment handle");
        assert_eq!(
            handle.len(),
            64,
            "the handle is the server's 32-byte hex value"
        );
        assert!(
            handle.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "the fragment handle must be hex: {handle}"
        );
        let argv = match fs::read_to_string(&daemon.browser_argv) {
            Ok(argv) => argv,
            Err(error) => {
                let record_dir = daemon
                    .browser_argv
                    .parent()
                    .expect("the browser argv record has a parent");
                let listing = match fs::read_dir(record_dir) {
                    Ok(entries) => entries
                        .map(|entry| match entry {
                            Ok(entry) => entry.file_name().to_string_lossy().into_owned(),
                            Err(entry_error) => format!("<entry error: {entry_error}>"),
                        })
                        .collect::<Vec<_>>()
                        .join(", "),
                    Err(listing_error) => format!("<unreadable: {listing_error}>"),
                };
                let fixture = scratch.join("browser-bin").join("xdg-open");
                let executable = fs::metadata(&fixture)
                    .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
                    .unwrap_or(false);
                let log_path = scratch.join("daemon.log");
                panic!(
                    "the injected real opener did not record its argv ({error})\n\
                     record file: {}\n\
                     record dir ({}) listing: {listing}\n\
                     fixture {}: exists={} executable={executable}\n\
                     test process PATH={:?} DISPLAY={:?}\n\
                     daemon log ({}):\n{}",
                    daemon.browser_argv.display(),
                    record_dir.display(),
                    fixture.display(),
                    fixture.exists(),
                    std::env::var("PATH").ok(),
                    std::env::var("DISPLAY").ok(),
                    log_path.display(),
                    fs::read_to_string(&log_path)
                        .unwrap_or_else(|log_error| format!("<unreadable: {log_error}>")),
                )
            }
        };
        assert_eq!(
            argv.trim(),
            url.trim(),
            "the real opener receives exactly the printed fragment URL"
        );
        assert_eq!(argv.lines().count(), 1, "the opener writes one URL only");
        assert!(
            !stdout(&auth).contains(REMOTE_TOKEN),
            "auth must never echo a stored secret"
        );

        let (body, record) = read_pairing_record(&daemon.state_dir);
        assert_eq!(
            record["endpoint"],
            discovery.endpoint().as_str(),
            "the store records the paired endpoint: {record}"
        );
        assert_eq!(record["account"], "alice", "the store records the account");
        assert_eq!(
            record["machine"], "dev-laptop",
            "the store records the machine"
        );
        assert!(
            !body.contains(REMOTE_TOKEN) && !body.contains("PRIVATE"),
            "the pairing record carries public coordinates only: {body}"
        );

        let key = pair_and_read_key(&daemon, &repo);
        daemon.shutdown();
        key
    };
    drop(discovery);

    // --- boot 2: re-pair against the enrolled server so the store names the live endpoint ----
    let server = ReferenceOsppServer::start(ReferenceServerConfig {
        accepted_keys: vec![key],
        mode: "local".to_owned(),
        vars: BTreeMap::from([
            ("REMOTE_VAR".to_owned(), "from-remote".to_owned()),
            ("WORKER_VAR".to_owned(), "worker-remote".to_owned()),
        ]),
        secrets: BTreeMap::from([("REMOTE_TOKEN".to_owned(), REMOTE_TOKEN.to_owned())]),
        ..ReferenceServerConfig::default()
    });
    {
        let mut daemon = Daemon::start_with_ospp(scratch, server.endpoint());
        // Seed the local vault before the reboot and before re-pairing: it must survive both, and
        // `auth` must never echo its value.
        seed_local_vault(
            &daemon.state_dir,
            "acme-two-tier",
            "local-debug",
            "local-password",
        );
        let auth = daemon.cli(&repo, &["auth"]);
        assert_success(&auth, "slipfork auth against the enrolled server");
        let expected_pair = format!("http://{}/pair", server.dump_addr());
        assert_eq!(
            pairing_url(&auth).split("#handle=").next(),
            Some(expected_pair.as_str()),
            "the pairing URL is the enrolled server's"
        );
        assert!(
            !stdout(&auth).contains("local-password") && !stderr(&auth).contains("local-password"),
            "no command may echo a stored secret value: {}",
            stdout(&auth)
        );
        let (_, record) = read_pairing_record(&daemon.state_dir);
        assert_eq!(
            record["endpoint"],
            server.endpoint().as_str(),
            "the store follows the re-pairing: {record}"
        );
        daemon.shutdown();
    }

    // --- boot 3: no `--ospp-endpoint` flag; the daemon reconnects through the store ---------
    let mut daemon = Daemon::start(scratch);
    let client = DaemonControlClient::bind(&daemon.runtime_dir);
    let listed = daemon.cli(&repo, &["secret", "list"]);
    assert_success(&listed, "secret list after the store-driven reboot");
    assert!(
        stdout(&listed).contains("LOCAL_PASSWORD"),
        "the local vault must survive a daemon reboot: {}",
        stdout(&listed)
    );

    assert_success(
        &daemon.cli(&repo, &["up", "--no-dump"]),
        "up through the store-resolved subscription",
    );
    let env = daemon.cli(
        &repo,
        &[
            "exec",
            "--",
            "sh",
            "-c",
            "printenv DEBUG; printenv REMOTE_VAR; printenv REMOTE_TOKEN; printenv LOCAL_PASSWORD",
        ],
    );
    assert_success(&env, "exec the resolved environment");
    assert!(
        !stderr(&env).contains("getcwd"),
        "world entry must start on a directory the world carries, never a host pathname: {}",
        stderr(&env)
    );
    let values = stdout(&env);
    assert!(
        values.contains("local-debug"),
        "claimed vars::DEBUG must be substituted locally: {values}"
    );
    assert!(
        values.contains("from-remote"),
        "unclaimed vars:: must arrive from the server: {values}"
    );
    assert!(
        values.contains(REMOTE_TOKEN),
        "remote secrets:: must be injected in memory: {values}"
    );
    assert!(
        values.contains("local-password"),
        "vault-backed local credentials must still resolve: {values}"
    );

    // The known remote plaintext appears nowhere under `<state_dir>` (world tree included).
    let hits = scan_tree(&daemon.state_dir, REMOTE_TOKEN.as_bytes());
    assert!(
        hits.is_empty(),
        "remote secret plaintext leaked onto disk: {hits:?}"
    );

    // --- a live `env_updated` restarts only the affected component --------------------------
    let selector = WorldSelector {
        cwd: repo.clone(),
        world: None,
    };
    // Let both components accrue whole seconds of uptime before the push.
    std::thread::sleep(Duration::from_millis(2_500));
    server.set_var("REMOTE_VAR", "pushed-remote");
    server.push_update().expect("push env_updated");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let rows = client
            .world_processes(&selector)
            .expect("world_processes after push");
        let api = uptime(&rows, "api");
        let worker = uptime(&rows, "worker");
        if api <= 1 && worker >= 2 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "env_updated must restart api only (api uptime {api}s, worker uptime {worker}s)"
        );
        std::thread::sleep(Duration::from_millis(200));
    }

    // --- the streaming sealed dump restores into the real postgres:18 -----------------------
    let created = run_service_entry_when_ready(
        &client,
        &repo,
        "pg_databases.main",
        &[
            "psql",
            "-U",
            "postgres",
            "-h",
            "127.0.0.1",
            "-c",
            "CREATE TABLE seed_rows(id int); INSERT INTO seed_rows VALUES (1),(2),(3);",
        ],
    );
    assert!(
        created.status.success(),
        "creating the seed table failed: {}",
        String::from_utf8_lossy(&created.stderr)
    );
    let dumped = run_service_entry(
        &client,
        &repo,
        "pg_databases.main",
        &[
            "pg_dump",
            "-Fc",
            "-U",
            "postgres",
            "-h",
            "127.0.0.1",
            "-d",
            "main",
        ],
    );
    assert!(
        dumped.status.success(),
        "pg_dump -Fc failed: {}",
        String::from_utf8_lossy(&dumped.stderr)
    );
    // The archive is produced by the world itself; the reference server then serves it sealed.
    server.set_dump("pg_databases.main", zstd_compress(&dumped.stdout));
    assert_success(&daemon.cli(&repo, &["down"]), "down after the dump");
    assert_success(&daemon.cli(&repo, &["up"]), "up with a streamed dump");
    assert_eq!(
        server.state().dump_urls,
        1,
        "up must stream exactly one dump"
    );

    let rows = run_service_entry_when_ready(
        &client,
        &repo,
        "pg_databases.main",
        &[
            "psql",
            "-U",
            "postgres",
            "-h",
            "127.0.0.1",
            "-tAc",
            "SELECT count(*) FROM seed_rows",
        ],
    );
    assert!(
        rows.status.success(),
        "querying the restored database failed: {}",
        String::from_utf8_lossy(&rows.stderr)
    );
    assert_eq!(stdout(&rows).trim(), "3", "the dump rows must be restored");

    // No dump stream byte lands on disk. A materialized archive **begins** with either the
    // uncompressed `PGDMP` magic or the zstd frame magic; binaries that merely embed those bytes
    // (the image's own `pg_dump`) are not dump files.
    let hits = scan_tree_headers(
        &daemon.state_dir,
        &[b"PGDMP".as_slice(), b"\x28\xb5\x2f\xfd".as_slice()],
    );
    assert!(
        hits.is_empty(),
        "a dump stream was materialized on disk: {hits:?}"
    );

    // --- an optional and a malformed `slipfork.toml` on the same daemon ---------------------
    let optional = repo_with_manifest(scratch, "acme-optional", PLAIN_SNOW);
    fs::remove_file(optional.join("slipfork.toml")).expect("remove optional slipfork.toml");
    assert_success(
        &daemon.cli(&optional, &["up"]),
        "up in a repository without slipfork.toml",
    );
    assert_success(
        &daemon.cli(&optional, &["down"]),
        "down before the malformed retry",
    );
    fs::write(optional.join("slipfork.toml"), "project = \"Bad_Slug\"\n")
        .expect("write malformed slipfork.toml");
    let malformed = daemon.cli(&optional, &["up"]);
    assert!(
        !malformed.status.success(),
        "a malformed slipfork.toml must fail closed"
    );
    assert!(
        stderr(&malformed).contains("slipfork.toml"),
        "the diagnostic must name the malformed manifest: {}",
        stderr(&malformed)
    );

    assert_success(&daemon.cli(&repo, &["down"]), "final down");
    daemon.shutdown();
    assert!(
        !support::daemon_is_served(&daemon.runtime_dir),
        "teardown unlinks the control socket"
    );
    drop(daemon);
}
