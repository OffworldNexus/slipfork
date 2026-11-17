//! Protected `slipforkd` OSPP v1 / seed-dump / base-snapshot / VCS-routing critical tests
//! (`M2.30`, `ADR-0008` §1–§3, `ADR-0009` §2–§3).
//!
//! Every test drives the real `slipfork` + `slipforkd` binaries. The OSPP host is the in-tree
//! `ospp-v1` reference server from the unpublished `slipfork-ospp-testkit` crate (a test double,
//! never a production fallback): pairing, `env.bundle`, `resources.describe`, and
//! `dump.begin..dump.end` all run over a real loopback WebSocket. `postgres:18` and the pinned
//! static `crun` are real; a missing Docker/crun/jj dependency fails loudly instead of weakening an
//! assertion.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use slipfork_edge::daemon::ExecTargetRequest;
use slipfork_edge::{DaemonControl, DaemonControlClient};
use slipfork_ospp::KeyId;
use slipfork_ospp_testkit::{
    DumpRecord, FakeTpmd, HandleRecord, ReferenceOsppServer, ReferenceServerConfig, S3BucketRecord,
    TpmdBehavior,
};

mod support;
use support::{
    Daemon, assert_success, git_only_path, jj_binary, repo_with_manifest, stderr, stdout,
};

const OSPP_SNOW: &str = r#"
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
  s3_buckets:
    - id: assets

modes:
  local:
    envs:
      app:
        BASE_URL: '${{ resources::app.url }}'
        S3_ENDPOINT: '${{ resources::s3_buckets.assets.endpoint_url }}'
        S3_BUCKET: '${{ resources::s3_buckets.assets.bucket }}'
        REMOTE_VAR: '${{ vars::REMOTE_VAR }}'
        REMOTE_TOKEN: '${{ secrets::REMOTE_TOKEN }}'
        GENERATED: '${{ secrets::GENERATED !! random_string(32) }}'
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

fn scratch_dir() -> tempfile::TempDir {
    support::disk_tempdir("ospp-")
}

fn reference_config(key: Option<KeyId>, handle: [u8; 32]) -> ReferenceServerConfig {
    ReferenceServerConfig {
        keys: key.into_iter().collect(),
        handles: vec![HandleRecord::new(handle, 1_000)],
        account: "dev@example".to_owned(),
        vars: BTreeMap::from([("REMOTE_VAR".to_owned(), "from-remote".to_owned())]),
        secrets: BTreeMap::from([("REMOTE_TOKEN".to_owned(), "remote-token".to_owned())]),
        s3_buckets: BTreeMap::from([(
            "assets".to_owned(),
            S3BucketRecord::new(
                "http://dev-s3.example:9000",
                "us-east-1",
                "dev-assets",
                "dev-access-key",
                "dev-secret-key",
            ),
        )]),
        ..ReferenceServerConfig::default()
    }
}

/// Redeem one pairing URL through `slipfork auth login -` and return the printed device key id.
fn login(daemon: &Daemon, repo: &Path, server: &ReferenceOsppServer, handle: [u8; 32]) -> KeyId {
    let url = server.browser_pairing_url(handle);
    let output = daemon.cli_with_stdin(repo, &["auth", "login", "-"], &format!("{url}\n"));
    assert_success(&output, "auth login -");
    let text = stdout(&output);
    let token = text
        .split_whitespace()
        .find(|token| token.starts_with("p256:"))
        .unwrap_or_else(|| panic!("auth login must print the device key id: {text}"));
    KeyId::parse(token).expect("printed key id parses")
}

/// Run one service entry exactly like the thin CLI does (`crun exec` inside the container).
fn run_service_entry(
    client: &DaemonControlClient,
    repo: &Path,
    world: Option<&str>,
    service: &str,
    argv: &[&str],
) -> Output {
    let target = client
        .exec_target(&ExecTargetRequest {
            cwd: repo.to_path_buf(),
            world: world.map(str::to_owned),
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

fn base_snapshot(daemon: &Daemon, project: &str) -> PathBuf {
    let snapshots = daemon.state_dir.join("snapshots");
    fs::read_dir(&snapshots)
        .unwrap_or_else(|err| panic!("cannot read {}: {err}", snapshots.display()))
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&format!("{project}-base-")))
        })
        .unwrap_or_else(|| {
            panic!(
                "no sealed base snapshot for {project} in {}",
                snapshots.display()
            )
        })
}

#[test]
fn paired_ospp_resolves_remote_secrets_restores_a_real_dump_and_forks_rows() {
    let tmp = scratch_dir();
    let scratch = tmp.path();
    let repo = repo_with_manifest(scratch, "acme-ospp", OSPP_SNOW);
    // The host's system `tpmd` may be absent or unreachable; the composition root's
    // `$XDG_RUNTIME_DIR/tpmd/tpmd.sock` probe is served by the real pinned-protocol fake daemon.
    let _tpmd = FakeTpmd::start_at(
        scratch.join("xdg-runtime/tpmd/tpmd.sock"),
        TpmdBehavior::Sealed,
    );
    let daemon = Daemon::start(scratch);

    // 1. Learn the daemon's device key id from a real pairing redemption. Pairing itself does not
    //    check the registered key set, so an empty-key reference server is enough for this step.
    let handle = [0x11u8; 32];
    let discovery = ReferenceOsppServer::start(reference_config(None, handle));
    let key_id = login(&daemon, &repo, &discovery, handle);
    drop(discovery);

    // 2. Re-pair against a server that registers that key and serves the remote env.
    let handle = [0x22u8; 32];
    let server = ReferenceOsppServer::start(reference_config(Some(key_id.clone()), handle));
    login(&daemon, &repo, &server, handle);

    let resolved = daemon.cli(&repo, &["snow", "resolve", "--expose-secrets"]);
    assert_success(&resolved, "snow resolve against the paired OSPP server");
    let resolved = stdout(&resolved);
    assert!(
        resolved.contains("REMOTE_VAR=from-remote"),
        "remote vars:: must resolve through env.bundle: {resolved}"
    );
    assert!(
        resolved.contains("REMOTE_TOKEN=remote-token"),
        "remote secrets:: must resolve through env.bundle: {resolved}"
    );
    assert!(
        resolved.contains("GENERATED="),
        "the !! secret must be generated: {resolved}"
    );

    let vault = daemon
        .state_dir
        .join("xdg-data/slipfork/vaults/acme-ospp.json");
    let vault_text = fs::read_to_string(&vault).expect("the 0600 vault exists");
    assert!(
        vault_text.contains("GENERATED"),
        "the !! secret must persist to the local 0600 vault: {vault_text}"
    );
    assert!(
        !vault_text.contains("remote-token"),
        "the remote secret must never be copied into the local vault: {vault_text}"
    );
    assert_eq!(
        fs::metadata(&vault)
            .expect("vault metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600,
        "the local vault stays 0600"
    );
    assert!(
        server.state().ensure_calls.is_empty(),
        "a generated !! secret must never reach the remote compare-and-set"
    );

    // 3. `--no-dump` seals a real clean-initdb base through freeze_world and streams no dump.
    let up_clean = daemon.cli(&repo, &["up", "--no-dump"]);
    assert_success(&up_clean, "up --no-dump");
    assert!(
        server.state().dumps_served.is_empty(),
        "--no-dump must never stream a seed dump"
    );
    let clean_base = base_snapshot(&daemon, "acme-ospp");
    assert!(
        clean_base.join("pgdata/main/PG_VERSION").is_file(),
        "freeze_world must seal a real initdb PGDATA, got {}",
        clean_base.display()
    );

    // 4. Produce a real `pg_dump -Fc` archive from that world, then release it.
    let client = DaemonControlClient::bind(&daemon.runtime_dir);
    let created = run_service_entry(
        &client,
        &repo,
        None,
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
        None,
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
    assert!(
        !dumped.stdout.is_empty(),
        "pg_dump produced an empty archive"
    );
    assert_success(
        &daemon.cli(&repo, &["down"]),
        "down after producing the dump",
    );

    // 5. Serve that real archive from a fresh reference server and boot the dump-backed world.
    drop(server);
    let handle = [0x33u8; 32];
    let mut config = reference_config(Some(key_id.clone()), handle);
    config
        .pg_databases
        .insert("main".to_owned(), DumpRecord::new("main", dumped.stdout));
    let server = ReferenceOsppServer::start(config);
    login(&daemon, &repo, &server, handle);

    let up = daemon.cli(&repo, &["up"]);
    assert_success(&up, "up with a streamed seed dump");
    assert_eq!(
        server.state().dumps_served,
        vec!["main".to_owned()],
        "up must stream exactly one seed dump"
    );
    let restored_base = base_snapshot(&daemon, "acme-ospp");
    assert!(
        restored_base.join("pgdata/main/PG_VERSION").is_file(),
        "freeze_world must seal the restored PGDATA, got {}",
        restored_base.display()
    );

    // Remote S3 coordinates reach the world environment, not the local SeaweedFS endpoint.
    let env = daemon.cli(&repo, &["exec", "--", "sh", "-c", "printenv S3_ENDPOINT"]);
    assert_success(&env, "exec printenv S3_ENDPOINT");
    assert!(
        stdout(&env).contains("http://dev-s3.example:9000"),
        "the OSPP-offered S3 endpoint must win: {}",
        stdout(&env)
    );

    // 6. Drop the dump cache and fork: the sealed base is reused, so nothing is re-streamed or
    //    re-restored, and the restored rows are visible in the CoW clone.
    let dumps = daemon.state_dir.join("dumps");
    if dumps.is_dir() {
        for entry in fs::read_dir(&dumps).expect("read dumps") {
            let path = entry.expect("dump entry").path();
            fs::remove_file(&path).expect("remove cached dump");
        }
    }
    let fork = daemon.cli(&repo, &["fork", "feat-seed"]);
    assert_success(&fork, "fork feat-seed");
    assert_eq!(
        server.state().dumps_served,
        vec!["main".to_owned()],
        "fork must reuse the sealed base instead of re-streaming the dump"
    );
    let rows = run_service_entry(
        &client,
        &repo,
        Some("feat-seed"),
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
        "querying the fork failed: {}",
        String::from_utf8_lossy(&rows.stderr)
    );
    assert_eq!(
        stdout(&rows).trim(),
        "3",
        "the fork must CoW-clone the restored rows"
    );

    // 7. A stopped OSPP server fails `--refresh-dump` closed.
    assert_success(
        &daemon.cli(&repo, &["down"]),
        "down canonical before refresh",
    );
    drop(server);
    let refresh = daemon.cli(&repo, &["up", "--refresh-dump"]);
    assert!(
        !refresh.status.success(),
        "a stopped OSPP server must fail --refresh-dump closed"
    );
    assert!(
        stderr(&refresh).contains("requires a remote OSPP offer"),
        "the refusal must name the missing remote offer: {}",
        stderr(&refresh)
    );

    // 8. `local = "remote"` fails closed too.
    fs::write(
        repo.join("slipfork.toml"),
        "project = \"acme-ospp\"\n[resources.pg_databases.main]\nlocal = \"remote\"\n",
    )
    .expect("write remote strategy");
    let remote = daemon.cli(&repo, &["up"]);
    assert!(
        !remote.status.success(),
        "an unpaired `local = \"remote\"` must fail closed"
    );
    assert!(
        stderr(&remote).contains("requires a remote OSPP offer"),
        "the refusal must name the missing remote offer: {}",
        stderr(&remote)
    );
    assert_success(&daemon.cli(&repo, &["drop", "feat-seed"]), "drop the fork");
}

#[test]
fn unpaired_remote_strategy_and_refresh_dump_fail_closed() {
    let tmp = scratch_dir();
    let scratch = tmp.path();
    let repo = repo_with_manifest(scratch, "acme-unpaired", OSPP_SNOW);
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
        stderr(&remote).contains("requires a remote OSPP offer"),
        "the refusal must name the missing remote offer: {}",
        stderr(&remote)
    );

    let refresh = daemon.cli(&repo, &["up", "--refresh-dump"]);
    assert!(
        !refresh.status.success(),
        "an unpaired --refresh-dump must fail closed"
    );
    assert!(
        stderr(&refresh).contains("requires a remote OSPP offer"),
        "the refusal must name the missing remote offer: {}",
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
    let daemon = Daemon::start_with_path(scratch, Some(&git_only));
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
    let git_ref = Command::new("git")
        .args(["rev-parse", "--verify", "refs/heads/feat-jj"])
        .current_dir(&repo)
        .output()
        .expect("git rev-parse");
    assert!(
        !git_ref.status.success(),
        "a refused Jujutsu fork must never create a Git branch"
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
    let daemon = Daemon::start_with_path(scratch, Some(Path::new(&path)));
    assert_success(&daemon.cli(&repo, &["fork", "feat-jj"]), "fork feat-jj");
    assert!(
        repo.join(".worktrees/feat-jj").is_dir(),
        "the jj workspace checkout must exist"
    );
    let workspaces = Command::new(&jj)
        .args(["workspace", "list", "--template", "name ++ \"\\n\""])
        .current_dir(&repo)
        .env("JJ_USER", "Slipfork Test")
        .env("JJ_EMAIL", "test@localhost")
        .output()
        .expect("jj workspace list");
    assert!(workspaces.status.success(), "jj workspace list failed");
    assert!(
        String::from_utf8_lossy(&workspaces.stdout)
            .lines()
            .any(|name| name == "feat-jj"),
        "the fork must be a registered Jujutsu workspace: {}",
        String::from_utf8_lossy(&workspaces.stdout)
    );
    assert_success(&daemon.cli(&repo, &["drop", "feat-jj"]), "drop feat-jj");
}
