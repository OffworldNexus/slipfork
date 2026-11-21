//! Protected lab journey for `slipfork fmt` and `slipfork migrate`.
//!
//! Real disk under `/var/tmp`, a real `git init`, and the shipped binary as a
//! subprocess. No mocks, no in-memory filesystems, no PTY theater.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ANSI: &str = "\u{1b}[";

const UNFORMATTED: &str =
    "# forkfile: 0.1\n\nimage backend . {\n    service api {\n        port 8000\n    }\n}\n\n";

const CANONICAL: &str =
    "# forkfile: 0.1\nimage backend . {\n  service api {\n    port 8000\n  }\n}\n";

const SNOW: &str = "images:\n  - id: backend\n    context: .\ncomponents:\n  - id: api\n    image: backend\n    run_command: gunicorn app:app\n";

struct Lab {
    dir: PathBuf,
}

impl Lab {
    fn new(name: &str) -> Self {
        let dir = Path::new("/var/tmp").join(format!("slipfork-cli-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create lab dir");
        let status = Command::new("git")
            .arg("init")
            .arg("--quiet")
            .arg(&dir)
            .status()
            .expect("git must be installed");
        assert!(status.success(), "git init must succeed");
        Self { dir }
    }

    fn write(&self, name: &str, text: &str) -> PathBuf {
        let path = self.dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent directory");
        }
        fs::write(&path, text).expect("write lab file");
        path
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.dir.join(name)).expect("read lab file")
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_with(args, &[("NO_COLOR", "1")])
    }

    fn run_with(&self, args: &[&str], envs: &[(&str, &str)]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_slipfork"));
        command.args(args).current_dir(&self.dir);
        for (key, value) in envs {
            command.env(key, value);
        }
        command
            .output()
            .expect("the slipfork binary must be runnable")
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn parse_json(text: &str) -> serde_json::Value {
    serde_json::from_str(text).expect("output must be JSON")
}

#[test]
fn fmt_checks_rewrites_and_emits_pure_json() {
    let lab = Lab::new("fmt");
    let inside = Command::new("git")
        .arg("-C")
        .arg(&lab.dir)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .expect("git must be runnable");
    assert_eq!(
        text(&inside.stdout).trim(),
        "true",
        "lab must be a git repo"
    );

    lab.write("Forkfile", UNFORMATTED);

    let check = lab.run_with(
        &["fmt", "--check"],
        &[("NO_COLOR", "1"), ("TERM", "xterm-256color")],
    );
    assert_eq!(
        check.status.code(),
        Some(1),
        "unformatted must fail --check"
    );
    let diff = format!("{}{}", text(&check.stdout), text(&check.stderr));
    assert!(
        diff.contains("--- Forkfile") && diff.contains("+++ Forkfile (formatted)"),
        "{diff}"
    );
    assert!(!diff.contains(ANSI), "pipes must stay plain: {diff}");

    lab.write("Other.Forkfile", UNFORMATTED);
    let custom = lab.run(&["fmt", "--check", "Other.Forkfile"]);
    assert_eq!(custom.status.code(), Some(1));
    assert!(
        text(&custom.stderr).contains("--- Other.Forkfile"),
        "{}",
        text(&custom.stderr)
    );

    let rewrite = lab.run(&["fmt"]);
    assert!(rewrite.status.success());
    assert_eq!(lab.read("Forkfile"), CANONICAL);

    let again = lab.run(&["fmt", "--check"]);
    assert!(again.status.success(), "second --check must pass");

    let json = lab.run(&["fmt", "--check", "--json"]);
    assert!(json.status.success());
    let body = text(&json.stdout);
    assert!(!body.contains(ANSI));
    assert_eq!(
        body.trim(),
        "{\"changed\":false,\"formatted\":true,\"path\":\"Forkfile\"}"
    );

    lab.write("Forkfile", UNFORMATTED);
    let unformatted = lab.run(&["fmt", "--check", "--json"]);
    assert_eq!(unformatted.status.code(), Some(1));
    let body = text(&unformatted.stdout);
    assert!(!body.contains(ANSI));
    assert!(!body.contains("@@") && !body.contains('^'), "{body}");
    assert_eq!(
        body.trim(),
        "{\"changed\":true,\"formatted\":false,\"path\":\"Forkfile\"}"
    );
    assert_eq!(
        parse_json(&body)["formatted"],
        serde_json::Value::Bool(false)
    );

    let json = lab.run(&["fmt", "--json"]);
    assert!(json.status.success());
    assert_eq!(lab.read("Forkfile"), CANONICAL);
}

#[test]
fn migrate_prints_a_forkfile_that_reparses_and_writes_in_place() {
    let lab = Lab::new("migrate");
    lab.write("snow.yml", SNOW);

    let printed = lab.run(&["migrate"]);
    assert!(printed.status.success());
    let forkfile = text(&printed.stdout);
    assert!(forkfile.starts_with("# forkfile: 0.1\n"), "{forkfile}");
    lab.write("Forkfile", &forkfile);
    let check = lab.run(&["fmt", "--check"]);
    assert!(
        check.status.success(),
        "migrated output must re-parse and format"
    );

    fs::remove_file(lab.dir.join("Forkfile")).expect("drop hand-written copy");
    let written = lab.run(&["migrate", "--write"]);
    assert!(written.status.success());
    assert_eq!(lab.read("Forkfile"), forkfile);

    let refused = lab.run(&["migrate", "--write"]);
    assert_eq!(refused.status.code(), Some(1));
    let stderr = text(&refused.stderr);
    assert!(
        stderr.contains("next: mv Forkfile Forkfile.old"),
        "{stderr}"
    );

    let forced = lab.run(&["migrate", "--write", "--force"]);
    assert!(forced.status.success());
    assert!(
        text(&forced.stderr).contains("overwriting Forkfile (--force)"),
        "{}",
        text(&forced.stderr)
    );
    assert!(
        text(&forced.stderr).contains("next: slipfork fmt --check Forkfile"),
        "{}",
        text(&forced.stderr)
    );

    let json = lab.run(&["migrate", "--json"]);
    assert!(json.status.success());
    let body = text(&json.stdout);
    assert!(!body.contains(ANSI));
    let value = parse_json(&body);
    assert_eq!(value["path"], serde_json::Value::String("snow.yml".into()));
    assert_eq!(value["written"], serde_json::Value::Bool(false));
    let expected = serde_json::json!({
        "path": "snow.yml",
        "written": false,
        "forkfile": forkfile,
    });
    assert_eq!(value, expected);
}

#[test]
fn migrate_render_errors_point_at_snow_yml_with_a_caret() {
    let lab = Lab::new("migrate-error");
    lab.write("broken.yml", "images: []\nmystery: 1\n");
    let out = lab.run(&["migrate", "broken.yml"]);
    assert_eq!(out.status.code(), Some(1));
    let stderr = text(&out.stderr);
    assert!(stderr.contains("broken.yml:2:"), "{stderr}");
    assert!(stderr.contains('^'), "{stderr}");
    assert!(stderr.contains("next: "), "{stderr}");
    assert!(!stderr.contains(ANSI), "{stderr}");

    let json = lab.run(&["migrate", "--json", "broken.yml"]);
    assert_eq!(json.status.code(), Some(1));
    let body = text(&json.stdout);
    assert!(!body.contains('^') && !body.contains(ANSI), "{body}");
    let value = parse_json(&body);
    assert_eq!(
        value["path"],
        serde_json::Value::String("broken.yml".into())
    );
    assert_eq!(value["line"], serde_json::Value::from(2));
    assert!(
        value["error"]
            .as_str()
            .is_some_and(|m| m.contains("mystery"))
    );
    assert!(
        value["next"]
            .as_str()
            .is_some_and(|n| n.contains("unknown key"))
    );
}
