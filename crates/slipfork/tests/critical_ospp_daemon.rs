//! Protected `slipforkd` OSPP v1 two-tier secret, live-subscription, streaming-dump, and VCS
//! routing tests (`M3.29`, `docs/design/ospp.md`, `ADR-0008` §1–§3).
//!
//! Every test drives the real `slipfork` + `slipforkd` binaries (or the same in-process
//! `compose_daemon` graph on a non-Btrfs host). The OSPP host is the in-tree `ospp-v1` reference
//! server from the unpublished `slipfork-ospp-testkit` crate: a test double, never a production
//! fallback. `postgres:18` and the pinned static `crun` are real; a missing Docker/crun/jj
//! dependency fails loudly instead of weakening an assertion.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use slipfork_edge::daemon::{ExecTargetRequest, WorldSelector};
use slipfork_edge::{DaemonControl, DaemonControlClient};
use slipfork_ospp_testkit::{ReferenceOsppServer, ReferenceServerConfig};

pub mod support;
use support::{
    Daemon, assert_success, git_only_path, jj_binary, repo_with_manifest, run_git, stderr, stdout,
};

/// Two components and a local-vault `vars::DEBUG` override. The server resolves the unclaimed
/// remote values, while the claimed local ones stay byte-verbatim templates for the daemon.
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

const PG_SNOW: &str = r#"
images:
  - id: app
    context: .
    dockerfile: Dockerfile

components:
  - id: api
    image: app
    envs: [app]
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
      app:
        BASE_URL: '${{ resources::app.url }}'
"#;

/// A known remote-secret plaintext with non-ASCII bytes, so the byte scan proves the real UTF-8
/// value never reaches disk.
const REMOTE_TOKEN: &str = "remote-token-\u{1f510}";

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

/// A `0600` local vault holding a local `vars::DEBUG` override and a local credential.
fn seed_local_vault(daemon: &Daemon, project: &str, debug: &str, password: &str) {
    let path = daemon
        .state_dir
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

/// Every regular file under `root`, read as bytes (bounded by the caller's scan policy).
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

#[test]
fn remote_secrets_never_reach_disk_and_claimed_vars_resolve_from_the_local_vault() {
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

    // Pair once against an empty server to mint the machine key, then restart against a server
    // that accepts it and serves the remote values.
    let discovery = ReferenceOsppServer::start(ReferenceServerConfig::default());
    {
        let mut daemon = Daemon::start_with_ospp(scratch, discovery.endpoint());
        pair_and_read_key(&daemon, &repo);
        daemon.shutdown();
    }
    drop(discovery);

    // The device key from the first pairing persists under the shared data home. Start the real
    // server and learn the public key by pairing it once more against a permissive config.
    let permissive = ReferenceOsppServer::start(ReferenceServerConfig::default());
    let mut daemon = Daemon::start_with_ospp(scratch, permissive.endpoint());
    let key = pair_and_read_key(&daemon, &repo);
    daemon.shutdown();
    drop(permissive);

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
    let mut daemon = Daemon::start_with_ospp(scratch, server.endpoint());
    seed_local_vault(&daemon, "acme-two-tier", "local-debug", "local-password");

    assert_success(
        &daemon.cli(&repo, &["up"]),
        "up with the paired OSPP server",
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

    // (a) The known remote plaintext appears nowhere under `<state_dir>`.
    let hits = scan_tree(&daemon.state_dir, REMOTE_TOKEN.as_bytes());
    assert!(
        hits.is_empty(),
        "remote secret plaintext leaked onto disk: {hits:?}"
    );

    // (e) The vault-backed local credential survives a daemon restart.
    assert_success(&daemon.cli(&repo, &["down"]), "down before daemon restart");
    daemon.shutdown();
    drop(daemon);
    let restarted = Daemon::start_with_ospp(scratch, server.endpoint());
    let listed = restarted.cli(&repo, &["secret", "list"]);
    assert_success(&listed, "secret list after daemon restart");
    assert!(
        stdout(&listed).contains("LOCAL_PASSWORD"),
        "the local vault must survive a daemon restart: {}",
        stdout(&listed)
    );
}

#[test]
fn pushed_env_update_restarts_only_the_affected_component() {
    let tmp = scratch_dir();
    let scratch = tmp.path();
    let repo = repo_with_manifest(scratch, "acme-live", TWO_TIER_SNOW);
    run_git(
        &repo,
        &["remote", "add", "origin", "git@example.com:acme/live.git"],
    );

    let discovery = ReferenceOsppServer::start(ReferenceServerConfig::default());
    let key = {
        let mut daemon = Daemon::start_with_ospp(scratch, discovery.endpoint());
        let key = pair_and_read_key(&daemon, &repo);
        daemon.shutdown();
        key
    };
    drop(discovery);

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
    // An event-driven pairing verdict is not needed: `auth` auto-approves in the reference server.
    let daemon = Daemon::start_with_ospp(scratch, server.endpoint());
    seed_local_vault(&daemon, "acme-live", "local-debug", "local-password");
    assert_success(&daemon.cli(&repo, &["up"]), "up before a push");

    let client = DaemonControlClient::bind(&daemon.runtime_dir);
    let selector = WorldSelector {
        cwd: repo.clone(),
        world: None,
    };
    let uptime = |rows: &slipfork_edge::WorldProcessReport, id: &str| {
        rows.services
            .iter()
            .find(|row| row.info.id == id)
            .and_then(|row| row.info.uptime_secs)
            .unwrap_or(0)
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

#[test]
fn sealed_dump_restores_into_postgres_without_a_plaintext_file() {
    let tmp = scratch_dir();
    let scratch = tmp.path();
    let repo = repo_with_manifest(scratch, "acme-dump", PG_SNOW);
    run_git(
        &repo,
        &["remote", "add", "origin", "git@example.com:acme/dump.git"],
    );

    let discovery = ReferenceOsppServer::start(ReferenceServerConfig::default());
    let key = {
        let mut daemon = Daemon::start_with_ospp(scratch, discovery.endpoint());
        let key = pair_and_read_key(&daemon, &repo);
        daemon.shutdown();
        key
    };
    drop(discovery);

    let server = ReferenceOsppServer::start(ReferenceServerConfig {
        accepted_keys: vec![key.clone()],
        mode: "local".to_owned(),
        ..ReferenceServerConfig::default()
    });
    let mut daemon = Daemon::start_with_ospp(scratch, server.endpoint());

    // Seal a real clean initdb base, then produce a real `pg_dump -Fc` archive from the world.
    assert_success(&daemon.cli(&repo, &["up", "--no-dump"]), "up --no-dump");
    let client = DaemonControlClient::bind(&daemon.runtime_dir);
    // `--no-dump` seals a clean initdb without a restore, so the postmaster may still be in crash
    // recovery when the first client arrives. Wait for it, exactly as a developer would.
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
    // The OSPP dump stream is `zstd`-compressed; compress the archive on the host (the test owns
    // this file, never the daemon).
    let mut compressor = Command::new("zstd")
        .args(["-q", "-c"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn zstd");
    {
        use std::io::Write;
        compressor
            .stdin
            .take()
            .expect("zstd stdin")
            .write_all(&dumped.stdout)
            .expect("write zstd stdin");
    }
    let compressed = compressor.wait_with_output().expect("zstd output");
    assert!(compressed.status.success(), "zstd compression failed");
    assert_success(&daemon.cli(&repo, &["down"]), "down after the dump");
    daemon.shutdown();
    drop(daemon);
    drop(server);

    let server = ReferenceOsppServer::start(ReferenceServerConfig {
        accepted_keys: vec![key],
        mode: "local".to_owned(),
        dumps: BTreeMap::from([("pg_databases.main".to_owned(), compressed.stdout.clone())]),
        ..ReferenceServerConfig::default()
    });
    let daemon = Daemon::start_with_ospp(scratch, server.endpoint());
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

    // (c) No dump stream byte lands on disk. A materialized archive **begins** with either the
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
}

#[test]
fn unpaired_remote_strategy_and_refresh_dump_fail_closed() {
    let tmp = scratch_dir();
    let scratch = tmp.path();
    let repo = repo_with_manifest(scratch, "acme-unpaired", PG_SNOW);
    fs::write(
        repo.join("slipfork.toml"),
        "project = \"acme-unpaired\"\n[resources.pg_databases.main]\nlocal = \"remote\"\n",
    )
    .expect("write remote strategy");
    let daemon = Daemon::start(scratch);

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

    fs::write(repo.join("slipfork.toml"), "project = \"acme-unpaired\"\n")
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
}

#[test]
fn optional_slipfork_toml_boots_and_malformed_config_fails_closed() {
    let tmp = scratch_dir();
    let scratch = tmp.path();
    let repo = repo_with_manifest(scratch, "acme-optional", PLAIN_SNOW);
    fs::remove_file(repo.join("slipfork.toml")).expect("remove optional slipfork.toml");
    let daemon = Daemon::start(scratch);

    assert_success(
        &daemon.cli(&repo, &["up"]),
        "up in a repository without slipfork.toml",
    );
    assert_success(
        &daemon.cli(&repo, &["down"]),
        "down before the malformed retry",
    );

    fs::write(repo.join("slipfork.toml"), "project = \"Bad_Slug\"\n").expect("write malformed");
    let malformed = daemon.cli(&repo, &["up"]);
    assert!(
        !malformed.status.success(),
        "a malformed slipfork.toml must fail closed"
    );
    assert!(
        stderr(&malformed).contains("slipfork.toml"),
        "the diagnostic must name the malformed manifest: {}",
        stderr(&malformed)
    );
}

/// Start the real daemon with an explicit `PATH`, or assert its fail-closed non-Btrfs refusal.
fn daemon_with_path(scratch: &Path, path: &Path) -> Option<Daemon> {
    match Daemon::try_start_with_path(scratch, Some(path)) {
        Ok(daemon) => Some(daemon),
        Err(log) => {
            assert!(
                !support::host_is_btrfs(&scratch.join("state")),
                "the real daemon refused a Btrfs host: {log}"
            );
            support::assert_btrfs_refusal(&log);
            eprintln!("non-Btrfs host: `.jj` routing runs on the Btrfs VM");
            None
        }
    }
}

#[test]
fn dot_jj_repository_routes_to_jujutsu_and_never_falls_back_to_git() {
    let jj = jj_binary().unwrap_or_else(|| {
        panic!(
            "jj is required for the .jj routing test; install Jujutsu or place the pinned binary in ~/.cache/slipfork-tools/jj/jj"
        )
    });
    let jj_dir = jj.parent().expect("jj parent").to_path_buf();

    // Negative: a `.jj` repository with no `jj` on PATH fails closed with ToolMissing and never
    // creates a Git worktree.
    let tmp = scratch_dir();
    let scratch = tmp.path();
    let repo = repo_with_manifest(scratch, "acme-jj-absent", PLAIN_SNOW);
    fs::create_dir(repo.join(".jj")).expect("create .jj marker");
    let git_only = git_only_path(scratch);
    let Some(daemon) = daemon_with_path(scratch, &git_only) else {
        return;
    };
    let refused = daemon.cli(&repo, &["fork", "feat-jj"]);
    assert!(
        !refused.status.success(),
        "a .jj repository without jj must fail closed"
    );
    let diagnostic = stderr(&refused);
    assert!(
        diagnostic.contains("jj") && diagnostic.contains("not found on PATH"),
        "the refusal must be the typed ToolMissing diagnostic: {diagnostic}"
    );
    assert!(
        !repo.join(".worktrees/feat-jj").exists(),
        "a refused Jujutsu fork must never create a Git worktree"
    );

    // Positive: the same `.jj` repository with `jj` on PATH creates a Jujutsu workspace.
    let tmp = scratch_dir();
    let scratch = tmp.path();
    let repo = repo_with_manifest(scratch, "acme-jj", PLAIN_SNOW);
    let status = Command::new(&jj)
        .args(["git", "init", "--colocate"])
        .current_dir(&repo)
        .env("JJ_USER", "Slipfork Test")
        .env("JJ_EMAIL", "test@localhost")
        .status()
        .expect("spawn jj");
    assert!(status.success(), "jj git init --colocate failed");
    let mut path = std::ffi::OsString::from(jj_dir);
    path.push(":");
    path.push(std::env::var_os("PATH").expect("PATH"));
    let Some(daemon) = daemon_with_path(scratch, Path::new(&path)) else {
        return;
    };
    assert_success(&daemon.cli(&repo, &["fork", "feat-jj"]), "fork feat-jj");
    assert!(
        repo.join(".worktrees/feat-jj").is_dir(),
        "the jj workspace checkout must exist"
    );
    assert_success(&daemon.cli(&repo, &["drop", "feat-jj"]), "drop feat-jj");
}
