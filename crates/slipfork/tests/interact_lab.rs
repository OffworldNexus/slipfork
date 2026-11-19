//! One real lab journey for the per-service interaction verbs.
//!
//! A single `slipforkd` boot proves the whole interaction surface: `run` streams a declared
//! task's live output and mirrors its exit code, an unknown task lists the declared ones, and
//! `env` redacts a generated secret in text and `--json` while a piped `--reveal` fails closed.

use std::fs;
use std::os::unix::fs::PermissionsExt;

use serde_json::Value;

pub mod support;
use support::{Daemon, assert_success, repo_with_manifest, stderr, stdout};

const SNOW_YML: &str = r#"
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

modes:
  local:
    envs:
      app:
        BASE_URL: '${{ resources::app.url }}'
"#;

/// The manifest half of the contract: a secret generator, a plain variable, and two tasks.
const FORKFILE: &str = "\
project acme-interact

features {
    local
}

service api ./ {
    port 8000
    env {
        PLAIN_VAR = \"verbatim\"
        SECRET_KEY = secret(SECRET_KEY) ?? random_string(24)
    }
}

task hello in api: sh -c 'echo TASK-HELLO'
task tick in api: sh -c 'echo LIVE-LINE; exit 3'
";

#[test]
fn run_streams_a_task_and_mirrors_its_exit_code_while_env_redacts() {
    let tmp = support::disk_tempdir("i-");
    let scratch = tmp.path();
    let repo = repo_with_manifest(scratch, "acme-interact", SNOW_YML);
    fs::write(repo.join("Forkfile"), FORKFILE).expect("write Forkfile");
    support::run_git(&repo, &["add", "Forkfile"]);
    support::run_git(&repo, &["commit", "-q", "-m", "forkfile"]);

    let daemon = Daemon::start(scratch);
    assert_success(&daemon.cli(&repo, &["up"]), "up before run/env");

    // The vault directory the `env` generator writes into is a real 0700 directory.
    let vaults = daemon
        .state_dir
        .join("xdg-data")
        .join("slipfork")
        .join("vaults");
    fs::create_dir_all(&vaults).expect("vault directory");
    fs::set_permissions(&vaults, fs::Permissions::from_mode(0o700)).expect("private vault dir");

    let hello = daemon.cli(&repo, &["run", "hello"]);
    assert_success(&hello, "run hello");
    assert!(
        stdout(&hello).contains("TASK-HELLO"),
        "a task's live stdout must stream: {}",
        stdout(&hello)
    );

    let failing = daemon.cli(&repo, &["run", "tick"]);
    assert_eq!(
        failing.status.code(),
        Some(3),
        "the child's non-zero exit code must be the CLI's exit code: stdout={:?} stderr={:?}",
        stdout(&failing),
        stderr(&failing)
    );
    assert!(
        stdout(&failing).contains("LIVE-LINE"),
        "output before the failure must stream: {}",
        stdout(&failing)
    );
    assert!(
        stderr(&failing).contains("next: slipfork logs api"),
        "the failure must print one rescue line: {}",
        stderr(&failing)
    );

    let unknown = daemon.cli(&repo, &["run", "nope"]);
    assert!(!unknown.status.success(), "an unknown task must fail");
    let diagnostic = stderr(&unknown);
    assert!(
        diagnostic.contains("names no task 'nope'") && diagnostic.contains("tasks: hello, tick"),
        "the refusal must list the declared tasks: {diagnostic}"
    );

    let env = daemon.cli(&repo, &["env"]);
    assert_success(&env, "env");
    let text = stdout(&env);
    assert!(text.contains("SECRET_KEY=[REDACTED]"), "got: {text}");
    assert!(text.contains("PLAIN_VAR=verbatim"), "got: {text}");

    let json = daemon.cli(&repo, &["--json", "env"]);
    assert_success(&json, "env --json");
    let document: Value = serde_json::from_str(&stdout(&json)).expect("an env json document");
    assert_eq!(document["SECRET_KEY"], Value::Null);
    assert_eq!(document["PLAIN_VAR"], "verbatim");

    let reveal = daemon.cli(&repo, &["env", "--reveal"]);
    assert!(
        !reveal.status.success(),
        "a piped --reveal must fail closed: stdout={:?}",
        stdout(&reveal)
    );
    assert!(
        stderr(&reveal).contains("interactive terminal"),
        "the refusal must name the terminal requirement: {}",
        stderr(&reveal)
    );
}
