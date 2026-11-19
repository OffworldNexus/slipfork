//! Real-subprocess proof for `slipfork migrate`: the actual binary converts a realistic upstream
//! `snow.yml` into a statically valid `Forkfile` on disk, refuses to clobber one without `--force`,
//! previews under `--stdout`, and fails closed with distinct `error:`/`next:` remedies.
//!
//! Every fixture is a real file in a disk-backed `/var/tmp` scratch directory; no mock, stub, or
//! fake participates in the conversion.

use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use serde_json::Value;
use tempfile::TempDir;

/// A realistic upstream document: 2 services, 1 PostgreSQL cluster, 3 env groups.
const UPSTREAM: &str = r#"
images:
  - id: api
    context: ./api
    dockerfile: docker/Dockerfile.api
  - id: front
    context: ./front
resources:
  pg_clusters:
    - id: pg
      version: "18"
  pg_databases:
    - id: app_db
      cluster: pg
domains:
  - id: main
envs:
  common:
    DATABASE_URL: "${{ resources::pg_databases.app_db.url }}"
    SECRET_KEY: "${{ secrets::SECRET_KEY }}"
  telemetry:
    OTEL_ENDPOINT: "http://${{ domains::main.name }}/otel"
modes:
  all:
    envs:
      frontend:
        API_BASE: "${{ resources::app.url }}"
components:
  - id: api
    image: api
    run_command: "uv run gunicorn app.main:app --bind 0.0.0.0:8000"
    envs: [common, telemetry]
  - id: front
    image: front
    run_command: "node server.js"
    envs: [common, frontend]
    internal_ports: [5173]
ingresses:
  - match:
      prefix: /
    target:
      component: front
      port: 5173
hooks:
  - component: api
    when: pre-deploy
    run_command: "uv run alembic upgrade head"
"#;

/// A disk scratch directory (never the RAM-backed `/tmp`).
fn scratch() -> TempDir {
    tempfile::Builder::new()
        .prefix("migrate-")
        .tempdir_in("/var/tmp")
        .expect("a disk scratch directory")
}

/// Run the real `slipfork` binary in `dir` with color forced off.
fn cli(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("NO_COLOR", "1")
        .env_remove("XDG_CACHE_HOME")
        .env_remove("XDG_DATA_HOME")
        .output()
        .expect("spawn slipfork migrate")
}

/// Run the real binary under a pseudo-terminal so `isatty` holds on both ends.
fn cli_with_pty(dir: &Path, args: &[&str]) -> Output {
    let line = std::iter::once(env!("CARGO_BIN_EXE_slipfork").to_owned())
        .chain(args.iter().map(|arg| (*arg).to_owned()))
        .map(|arg| format!("'{}'", arg.replace('\'', "'\\''")))
        .collect::<Vec<_>>()
        .join(" ");
    Command::new("script")
        .arg("-q")
        .arg("-e")
        .arg("-c")
        .arg(line)
        .arg("/dev/null")
        .current_dir(dir)
        .env("HOME", dir)
        .env("TERM", "xterm-256color")
        .env_remove("NO_COLOR")
        .env_remove("XDG_CACHE_HOME")
        .env_remove("XDG_DATA_HOME")
        .stdin(Stdio::null())
        .output()
        .expect("spawn a pseudo-terminal")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("a JSON document on stdout")
}

#[test]
fn a_realistic_snow_yml_converts_into_the_expected_bytes_and_counts() {
    let dir = scratch();
    fs::write(dir.path().join("snow.yml"), UPSTREAM).expect("write snow.yml");

    let output = cli(dir.path(), &["--json", "migrate"]);
    assert!(
        output.status.success(),
        "migrate failed: {}",
        stderr(&output)
    );
    assert!(
        !stdout(&output).contains("\x1b["),
        "--json on a pipe emits zero ANSI: {}",
        stdout(&output)
    );
    let document = json(&output);
    assert_eq!(document["ok"], true);
    assert_eq!(document["input"], "snow.yml");
    assert_eq!(document["output"], "Forkfile");
    assert_eq!(document["services"], 2);
    assert_eq!(document["pg_clusters"], 1);
    assert_eq!(document["env_groups"], 3);

    let written = fs::read_to_string(dir.path().join("Forkfile")).expect("read Forkfile");
    assert!(
        written.starts_with("# migrated from snow.yml"),
        "provenance leads the file: {written}"
    );
    assert!(
        written.contains("postgres pg {\n    version 18;\n"),
        "the PostgreSQL cluster keeps its id and version: {written}"
    );
    assert!(
        written.contains("database app_db;"),
        "the database stays on its cluster: {written}"
    );
    assert!(
        written.contains("env common {\n") && written.contains("DATABASE_URL = "),
        "the common env group keeps its variables: {written}"
    );
    assert!(
        written.contains("service api ./api {") && written.contains("service front ./front {"),
        "both services survive with their build contexts: {written}"
    );
    assert!(
        written.contains("use env common"),
        "a service keeps its env-group references: {written}"
    );
    let parsed = slipfork_forkfile::parse_forkfile(&written);
    assert!(
        parsed.is_ok(),
        "the written Forkfile parses: {:?}",
        parsed.err()
    );

    // The same conversion on a real TTY paints the `✓` summary and nothing else.
    let pty = cli_with_pty(dir.path(), &["migrate", "-f"]);
    assert!(pty.status.success(), "pty migrate failed: {}", stderr(&pty));
    let text = stdout(&pty);
    assert!(
        text.contains("\x1b["),
        "a TTY paints the success line: {text}"
    );
    assert!(text.contains("✓"), "the success mark leads: {text}");
    assert!(text.contains("Migrated"), "the verb names itself: {text}");
    assert!(
        text.contains("snow.yml") && text.contains("Forkfile"),
        "{text}"
    );
    assert!(
        text.contains("2 services, 1 postgres cluster, 3 env groups"),
        "dynamic counts: {text}"
    );
}

#[test]
fn an_existing_forkfile_is_refused_without_force() {
    let dir = scratch();
    fs::write(dir.path().join("snow.yml"), UPSTREAM).expect("write snow.yml");
    fs::write(dir.path().join("Forkfile"), "sentinel\n").expect("write the existing Forkfile");

    let refused = cli(dir.path(), &["migrate"]);
    assert!(!refused.status.success(), "the refusal exits non-zero");
    let text = stderr(&refused);
    assert!(
        text.contains("error: refusing to overwrite Forkfile"),
        "{text}"
    );
    assert!(
        text.contains("next: pass -f/--force to replace it"),
        "one remedy: {text}"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("Forkfile")).expect("unchanged Forkfile"),
        "sentinel\n",
        "a refusal never truncates the existing file"
    );

    let refused_json = cli(dir.path(), &["--json", "migrate"]);
    assert!(
        !refused_json.status.success(),
        "a --json refusal still exits non-zero"
    );
    let document = json(&refused_json);
    assert_eq!(document["ok"], false);
    assert_eq!(document["code"], "output_exists");
    assert!(
        document["next"]
            .as_str()
            .unwrap_or_default()
            .contains("--force")
    );

    let forced = cli(dir.path(), &["migrate", "-f"]);
    assert!(
        forced.status.success(),
        "force replaces: {}",
        stderr(&forced)
    );
    assert!(
        fs::read_to_string(dir.path().join("Forkfile"))
            .expect("forced Forkfile")
            .starts_with("# migrated from snow.yml")
    );
}

#[test]
fn stdout_previews_the_conversion_and_writes_nothing() {
    let dir = scratch();
    fs::write(dir.path().join("snow.yml"), UPSTREAM).expect("write snow.yml");

    let preview = cli(dir.path(), &["migrate", "--stdout"]);
    assert!(
        preview.status.success(),
        "stdout preview failed: {}",
        stderr(&preview)
    );
    let text = stdout(&preview);
    assert!(text.starts_with("# migrated from snow.yml"), "{text}");
    assert!(!text.contains("\x1b["), "a pipe stays monochrome: {text}");
    assert!(
        !dir.path().join("Forkfile").exists(),
        "--stdout writes nothing"
    );

    let document = json(&cli(dir.path(), &["--json", "migrate", "--stdout"]));
    let Some(converted) = document.as_str() else {
        panic!("--stdout --json is a JSON string, got {document}");
    };
    assert!(converted.starts_with("# migrated from snow.yml"));

    // `-o` selects another file and leaves the default `Forkfile` untouched.
    let custom = cli(dir.path(), &["migrate", "-o", "Forkfile.dev"]);
    assert!(
        custom.status.success(),
        "custom output: {}",
        stderr(&custom)
    );
    let routed = fs::read_to_string(dir.path().join("Forkfile.dev")).expect("read Forkfile.dev");
    assert!(
        routed.starts_with("# migrated from snow.yml")
            && routed.contains("service api ./api {")
            && routed.contains("postgres pg {"),
        "`-o` writes the converted bytes: {routed}"
    );
    assert_eq!(
        json(&cli(
            dir.path(),
            &["--json", "migrate", "-o", "Forkfile.json", "-f"]
        ))["output"],
        "Forkfile.json",
        "the JSON reports the file actually written"
    );
    assert!(!dir.path().join("Forkfile").exists());
}

#[test]
fn a_missing_input_fails_closed_and_names_the_remedy() {
    let dir = scratch();

    let missing = cli(dir.path(), &["migrate"]);
    assert!(!missing.status.success());
    let text = stderr(&missing);
    assert!(
        text.contains("error: no snow.yml or snow.yaml in the current directory"),
        "{text}"
    );
    assert!(text.contains("next: add the upstream manifest"), "{text}");

    let missing_json = cli(dir.path(), &["--json", "migrate"]);
    assert!(
        !missing_json.status.success(),
        "a --json refusal still exits non-zero"
    );
    let document = json(&missing_json);
    assert_eq!(document["ok"], false);
    assert_eq!(document["code"], "missing_input");
    assert!(
        document["next"]
            .as_str()
            .unwrap_or_default()
            .contains("slipfork migrate")
    );
    assert!(!dir.path().join("Forkfile").exists(), "nothing is written");
}

#[test]
fn an_unknown_top_level_key_fails_closed_with_its_own_remedy() {
    let dir = scratch();
    fs::write(
        dir.path().join("snow.yml"),
        "images: []\nservices:\n  - id: api\n",
    )
    .expect("write snow.yml");

    let refused = cli(dir.path(), &["migrate"]);
    assert!(!refused.status.success());
    let text = stderr(&refused);
    assert!(
        text.contains("error: snow.yml:2: unknown top-level key `services`"),
        "{text}"
    );
    assert!(
        text.contains("next: rename `services` to a supported key (images, resources"),
        "the remedy lists the schema: {text}"
    );

    let refused_json = cli(dir.path(), &["--json", "migrate"]);
    assert!(
        !refused_json.status.success(),
        "a --json refusal still exits non-zero"
    );
    let document = json(&refused_json);
    assert_eq!(document["code"], "unknown_key");
    assert!(
        document["error"]
            .as_str()
            .unwrap_or_default()
            .contains("unknown top-level key `services`")
    );
    assert!(
        document["next"]
            .as_str()
            .unwrap_or_default()
            .contains("components")
    );
    assert!(!dir.path().join("Forkfile").exists(), "nothing is written");
}

#[test]
fn the_refusal_names_the_manifest_that_was_actually_read() {
    let dir = scratch();
    fs::create_dir(dir.path().join("cfg")).expect("create cfg");
    fs::write(
        dir.path().join("cfg/snow.yml"),
        "images: []\nservices:\n  - id: api\n",
    )
    .expect("write cfg/snow.yml");
    fs::write(
        dir.path().join("snow.yaml"),
        "images: []\nservices:\n  - id: api\n",
    )
    .expect("write snow.yaml");

    let nested = cli(dir.path(), &["migrate", "cfg/snow.yml"]);
    assert!(!nested.status.success());
    assert!(
        stderr(&nested).contains("error: cfg/snow.yml:2: unknown top-level key `services`"),
        "the refusal cites the path the user typed: {}",
        stderr(&nested)
    );

    let alternate = cli(dir.path(), &["migrate", "snow.yaml"]);
    assert!(!alternate.status.success());
    assert!(
        stderr(&alternate).contains("error: snow.yaml:2: unknown top-level key `services`"),
        "a `.yaml` input keeps its own name: {}",
        stderr(&alternate)
    );
}

#[test]
fn redis_and_s3_resources_are_counted_in_the_success_line() {
    let dir = scratch();
    fs::write(
        dir.path().join("snow.yml"),
        r#"
images:
  - id: app
    context: .
resources:
  redis_clusters:
    - id: cache
  s3_buckets:
    - id: uploads
components:
  - id: api
    image: app
    run_command: "exec sleep 300"
"#,
    )
    .expect("write snow.yml");

    let migrated = cli(dir.path(), &["migrate"]);
    assert!(
        migrated.status.success(),
        "migrate failed: {}",
        stderr(&migrated)
    );
    let text = stdout(&migrated);
    assert!(
        text.contains("(1 service, 1 redis cluster, 1 s3 bucket)"),
        "redis and s3 are counted, empty kinds are omitted: {text}"
    );
}
