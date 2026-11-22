//! Lab tests for the pure status renderer, `doctor`, and `daemon install`.
//!
//! The renderer journey feeds real `ps` JSON through the public typed renderer; the CLI cases run
//! the real binary against a daemon-free `/var/tmp` runtime, and `daemon install` writes real files
//! into injected roots under `/var/tmp`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use slipfork::boot::{Node, completed_row, elapsed_text, pane_geometry, status_line, summary_line};
use slipfork::install::{self, InstallHost, UnitWriter};
use slipfork::status::{self, Options};

#[test]
fn mountinfo_unescapes_octal_and_picks_the_longest_containing_mount() {
    const SAMPLE: &str = "\
29 1 253:1 / / rw,relatime shared:1 - ext4 /dev/vda1 rw
49 29 0:34 / /var/lib/slipfork rw,noatime shared:46 - btrfs /dev/sda rw,user_subvol_rm_allowed
50 29 0:35 / /var/lib/slipfork2 rw,noatime - btrfs /dev/sdb rw
55 29 0:41 / /mnt/my\\040drive\\134data rw,relatime - btrfs /dev/sdc rw
";
    let mounts = slipfork::mounts::parse_mountinfo(SAMPLE);
    assert_eq!(
        mounts,
        vec![
            PathBuf::from("/"),
            PathBuf::from("/var/lib/slipfork"),
            PathBuf::from("/var/lib/slipfork2"),
            PathBuf::from(r"/mnt/my drive\data"),
        ]
    );
    let store = |project: &str| slipfork::mounts::store_root(&mounts, Path::new(project));
    assert_eq!(
        store("/var/lib/slipfork/t/fork-live-1/demo"),
        PathBuf::from("/var/lib/slipfork/.slipfork-store")
    );
    assert_eq!(
        store("/var/lib/slipfork2/demo"),
        PathBuf::from("/var/lib/slipfork2/.slipfork-store"),
        "the nested mount must win over its parent"
    );
    assert_eq!(
        store("/var/lib/slipfork-other/demo"),
        PathBuf::from("/.slipfork-store")
    );
    assert_eq!(
        store(r"/mnt/my drive\data/app"),
        PathBuf::from(r"/mnt/my drive\data/.slipfork-store"),
        "octal-unescaped mounts stay matchable"
    );
}

#[test]
fn openrc_install_names_the_injected_user_and_keeps_a_user_edit() {
    let root = scratch("openrc");
    let host = InstallHost {
        exe: PathBuf::from("/usr/bin/slipfork"),
        path: Vec::new(),
        config_dir: root.join("config"),
        install_root: root.clone(),
        uid: 1000,
        ingress_uid: 1000,
        ingress_user: "builder".to_owned(),
        openrc: true,
    };
    let writer = UnitWriter::from_host(&host);
    let planned = writer.plan(true);
    let conf_d = root.join("etc/conf.d");
    let (conf_path, conf) = planned
        .iter()
        .find(|(path, _)| path.parent().is_some_and(|parent| parent == conf_d))
        .expect("an OpenRC host plans a conf.d file");
    assert!(
        conf.contains("SLIPFORK_INGRESS_USER=builder\n"),
        "the conf must name the injected user: {conf}"
    );
    let service = planned
        .iter()
        .find(|(path, _)| path.ends_with(install::SERVICE_UNIT))
        .map(|(_, contents)| contents)
        .expect("the ingress service is still planned on an OpenRC host");
    assert!(service.contains("User=1000\n"), "{service}");

    let written = writer.write_all(true).expect("write the install plan");
    assert!(written.contains(conf_path), "{written:?}");
    assert_eq!(
        fs::read_to_string(conf_path).expect("read the conf"),
        conf.clone()
    );

    fs::write(conf_path, "SLIPFORK_INGRESS_USER=alice\n").expect("simulate a user edit");
    writer.write_all(true).expect("keep the user edit");
    assert_eq!(
        fs::read_to_string(conf_path).expect("re-read the kept conf"),
        "SLIPFORK_INGRESS_USER=alice\n"
    );
    assert_eq!(writer.preserved_conf(true), Some(conf_path.clone()));
    fs::remove_dir_all(&root).expect("remove the scratch directory");
}

const ANSI: &str = "\u{1b}[";

#[test]
fn the_live_boot_pane_renders_exact_rows_and_bars() {
    let node = Node {
        kind: "image",
        name: "base",
        state: "Cached",
        millis: 420,
        success: true,
    };
    assert_eq!(
        completed_row(&node, true),
        "\u{1b}[32m✓\u{1b}[0m \u{1b}[2mimage   \u{1b}[0m  \u{1b}[1mbase\u{1b}[0m cached  \u{1b}[2m  420ms\u{1b}[0m"
    );
    assert_eq!(completed_row(&node, false), "✓ image base cached  420ms");
    assert!(!completed_row(&node, false).contains(ANSI));
    assert!(!completed_row(&node, false).contains('\r'));

    let cached = Node {
        state: "Up-to-date",
        success: false,
        ..node
    };
    assert_eq!(
        completed_row(&cached, true),
        "\u{1b}[2m•\u{1b}[0m \u{1b}[2mimage   \u{1b}[0m  \u{1b}[1mbase\u{1b}[0m up-to-date  \u{1b}[2m  420ms\u{1b}[0m"
    );
    assert_eq!(
        completed_row(&cached, false),
        "• image base up-to-date  420ms"
    );

    // A malformed frame fails closed instead of inventing `step`/`ready`/`0` fiction.
    assert!(
        Node::from_event(&json!({ "event": "finished" })).is_none(),
        "a frame without kind/name/duration must not render a row"
    );
    let real_frame = json!({
        "event": "finished", "kind": "image", "name": "base",
        "state": "Cached", "duration_ms": 420,
    });
    let real = Node::from_event(&real_frame).expect("a complete frame parses");
    assert_eq!(real, node);

    assert_eq!(elapsed_text(0), "<1ms");
    assert_eq!(elapsed_text(420), "420ms");
    assert_eq!(elapsed_text(1_200), "1.2s");

    assert_eq!(pane_geometry(120), Some(16));
    assert_eq!(pane_geometry(48), Some(16));
    assert_eq!(pane_geometry(47), None);

    let elapsed = Duration::from_millis(1_200);
    let bar = summary_line(10, 6, "Booting demo", elapsed, 80);
    assert_eq!(bar, "[██████████░░░░░░] 6/10  Booting demo  1.2s");
    assert_eq!(bar.matches(['█', '░']).count(), 16, "{bar}");
    assert_eq!(
        summary_line(10, 6, "Booting demo", elapsed, 40),
        "6/10  Booting demo  1.2s"
    );

    let ascii = status_line('|', 6, 10, "Booting demo", "api", elapsed, 80);
    assert_eq!(ascii, "| [##########......] 6/10  Booting demo  api  1.2s");
    assert!(!ascii.contains(ANSI), "{ascii}");
    assert!(!ascii.contains('\r'), "{ascii}");
    assert_eq!(
        status_line('|', 6, 10, "Booting demo", "", elapsed, 40),
        "6/10  Booting demo  1.2s"
    );
    for spinner in ['|', '/', '-', '\\'] {
        let line = status_line(
            spinner,
            3,
            10,
            "Restarting demo",
            "db",
            Duration::from_millis(50),
            80,
        );
        assert!(line.starts_with(spinner), "{line}");
        assert!(!line.contains(ANSI) && !line.contains('\r'), "{line}");
        let cells = line
            .split_once('[')
            .and_then(|(_, rest)| rest.split_once(']'))
            .map(|(bar, _)| bar)
            .unwrap_or_else(|| panic!("the ASCII line keeps its bar: {line}"));
        assert_eq!(
            cells.matches('#').count() + cells.matches('.').count(),
            16,
            "{line}"
        );
    }
}

fn scratch(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock reads after the epoch")
        .as_nanos();
    let root = PathBuf::from("/var/tmp").join(format!(
        "slipfork-status-{prefix}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&root).expect("create /var/tmp scratch directory");
    root
}

fn isolated(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .args(args)
        .current_dir(root)
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_RUNTIME_DIR", root.join("run"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("NO_COLOR", "1")
        .output()
        .expect("the slipfork binary must be runnable")
}

fn reply(value: Value) -> slipfork::ps::PsReply {
    status::parse(&value).expect("the fabricated payload follows the ps schema")
}

fn demo_reply(now_ms: u64) -> slipfork::ps::PsReply {
    reply(json!({
        "worlds": [{
            "project": "/home/me/projects/demo",
            "path": "/home/me/projects/demo",
            "world": "demo",
            "url": "http://demo.localhost",
            "started_at_ms": now_ms - 12 * 60 * 1_000,
            "supervisor_pid": 11,
            "alive": true,
            "booting": false,
            "services": [
                { "id": "api", "kind": "service", "running": true, "port": 8000,
                  "routes": [{ "prefix": "/api", "port": 8000, "mode": "preserve" }] },
                { "id": "worker", "kind": "service", "running": true },
                { "id": "front", "kind": "service", "running": true, "port": 5173,
                  "routes": [{ "prefix": "/", "port": 5173, "mode": "preserve" }] },
                { "id": "mailpit", "kind": "service", "running": true, "port": 8025,
                  "routes": [{ "prefix": "/", "port": 8025, "mode": "preserve",
                               "domain": { "kind": "prefix", "value": "mail" } }] },
                { "id": "pg", "kind": "postgres", "running": true, "port": 5432, "version": "18" },
                { "id": "cache", "kind": "redis", "running": true, "port": 6379, "version": "7" },
                { "id": "uploads", "kind": "s3", "running": true, "port": 8333, "policy": "private" },
                { "id": "assets", "kind": "s3", "running": true, "port": 8333, "policy": "public-read", "cdn": "assets-cdn" }
            ],
            "forks": [{
                "project": "/home/me/projects/demo/.forks/feat-auth",
                "path": "/home/me/projects/demo/.forks/feat-auth",
                "world": "feat-auth--demo",
                "fork": "feat-auth",
                "url": "http://feat-auth--demo.localhost",
                "started_at_ms": now_ms - 4 * 60 * 1_000,
                "supervisor_pid": 12,
                "alive": true,
                "booting": false,
                "services": [
                    { "id": "api", "kind": "service", "running": true, "port": 8000,
                      "routes": [{ "prefix": "/api", "port": 8000, "mode": "preserve" }] }
                ]
            }]
        }]
    }))
}

#[test]
fn the_tree_contract_holds_for_every_row_shape() {
    let now = 1_000_000_000;
    let text = status::render(
        &demo_reply(now),
        &Options::new(false)
            .with_now(now)
            .with_home(Some(PathBuf::from("/home/me"))),
    );
    assert!(
        text.starts_with("● demo  http://demo.localhost  ~/projects/demo\n"),
        "{text}"
    );
    let row = |id: &str| -> String {
        text.lines()
            .find(|line| line.contains(&format!(" {id} ")))
            .unwrap_or_else(|| panic!("no `{id}` row in:\n{text}"))
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    assert_eq!(row("api"), "├─ api running :8000 -> /api 12m");
    assert_eq!(row("worker"), "├─ worker running - 12m");
    assert_eq!(row("mailpit"), "├─ mailpit running :8025 -> mail.*/ 12m");
    assert_eq!(row("pg"), "├─ pg postgres 18 :5432 12m");
    assert_eq!(row("cache"), "├─ cache redis 7 :6379 12m");
    assert_eq!(row("uploads"), "├─ uploads s3 (private) :8333 12m");
    assert_eq!(
        row("assets"),
        "├─ assets s3 (public-read) -> assets-cdn.* 12m"
    );
    assert!(text.contains("  └─ forks"), "{text}");
    assert!(
        text.contains("     └─ ● feat-auth  http://feat-auth--demo.localhost  ~/projects/demo/.forks/feat-auth"),
        "{text}"
    );
    assert!(text.contains("12m"), "{text}");
    assert!(text.contains("4m"), "{text}");
    assert!(!text.contains(ANSI), "plain renders emit zero ANSI: {text}");
}

#[test]
fn crashed_resources_render_red_state_not_a_healthy_descriptor() {
    let reply = reply(json!({
        "worlds": [{
            "project": "/home/me/demo", "path": "/home/me/demo", "world": "demo",
            "url": "http://demo.localhost", "supervisor_pid": 1, "alive": true,
            "booting": false,
            "services": [
                { "id": "pg", "kind": "postgres", "running": false, "port": 5432, "version": "18" },
                { "id": "uploads", "kind": "s3", "running": false, "port": 8333 }
            ]
        }]
    }));
    let plain = status::render(&reply, &Options::new(false));
    assert!(plain.contains("failed"), "{plain}");
    assert!(!plain.contains("postgres 18"), "{plain}");
    assert!(!plain.contains("s3 (private)"), "{plain}");
    let colored = status::render(&reply, &Options::new(true));
    assert!(colored.contains("\u{1b}[31mfailed\u{1b}[0m"), "{colored}");
}

#[test]
fn mixed_short_and_long_names_keep_the_columns_aligned() {
    let reply = reply(json!({
        "worlds": [{
            "project": "/home/me/demo", "path": "/home/me/demo", "world": "demo",
            "url": "http://demo.localhost", "supervisor_pid": 1, "alive": true,
            "booting": false,
            "services": [
                { "id": "api", "kind": "service", "running": true },
                { "id": "an-extremely-long-service-name", "kind": "service", "running": true }
            ]
        }]
    }));
    let text = status::render(&reply, &Options::new(false));
    let state_column = |needle: &str| {
        text.lines()
            .find(|line| line.contains(needle))
            .and_then(|line| line.find("running"))
            .unwrap_or_else(|| panic!("no running row for {needle}: {text}"))
    };
    assert_eq!(
        state_column("─ api"),
        state_column("an-extremely-long-service-name"),
        "{text}"
    );
}

#[test]
fn a_forty_character_fork_name_never_shears() {
    let long = "feature-with-a-very-long-name-0123456789";
    assert_eq!(long.chars().count(), 40);
    let reply = reply(json!({
        "worlds": [{
            "project": "/home/me/demo", "path": "/home/me/demo", "world": "demo",
            "url": "http://demo.localhost", "supervisor_pid": 1, "alive": true,
            "booting": false,
            "services": [{ "id": "api", "kind": "service", "running": true, "port": 8000 }],
            "forks": [{
                "project": format!("/home/me/demo/.forks/{long}"),
                "path": format!("/home/me/demo/.forks/{long}"),
                "world": format!("{long}--demo"),
                "fork": long,
                "url": format!("http://{long}--demo.localhost"),
                "supervisor_pid": 2, "alive": true, "booting": false,
                "services": [{ "id": "api", "kind": "service", "running": true, "port": 8000 }]
            }]
        }]
    }));
    let text = status::render(
        &reply,
        &Options::new(false).with_home(Some("/home/me".into())),
    );
    assert!(text.contains(long), "the full name must survive: {text}");
}

#[test]
fn ages_render_as_human_units_and_colour_appears_only_when_allowed() {
    let now = 10_000_000_000;
    let world = |name: &str, started: u64| {
        json!({
            "project": format!("/home/me/{name}"),
            "path": format!("/home/me/{name}"),
            "world": name,
            "url": format!("http://{name}.localhost"),
            "started_at_ms": now - started,
            "supervisor_pid": 1, "alive": true, "booting": false,
            "services": [{ "id": "api", "kind": "service", "running": true }]
        })
    };
    let reply = reply(json!({ "worlds": [
        world("a", 4_000),
        world("b", 12 * 60_000),
        world("c", 3 * 3_600_000),
        world("d", 2 * 86_400_000)
    ]}));
    let plain = status::render(&reply, &Options::new(false).with_now(now));
    for age in ["4s", "12m", "3h", "2d"] {
        assert!(plain.contains(age), "missing {age}: {plain}");
    }
    let colored = status::render(&reply, &Options::new(true).with_now(now));
    assert!(
        colored.contains(ANSI),
        "a colour render must style: {colored}"
    );
}

#[test]
fn the_empty_state_is_dim_and_exits_zero() {
    assert_eq!(
        status::render(&slipfork::ps::PsReply::default(), &Options::new(false)),
        "○ No worlds running\n"
    );
    let hint = status::no_world_line(
        Path::new("/var/tmp/elsewhere"),
        Some(Path::new("/var/tmp")),
        false,
    );
    assert_eq!(hint, "○ No world for ~/elsewhere\n");
    let root = scratch("empty");
    for args in [vec![], vec!["status"], vec!["ps"]] {
        let output = isolated(&root, &args);
        assert!(output.status.success(), "empty state must exit 0: {args:?}");
        assert_eq!(
            String::from_utf8(output.stdout).expect("status is UTF-8"),
            "○ No worlds running\n"
        );
    }
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn json_and_non_tty_matrix_emits_zero_ansi() {
    let root = scratch("ansi");
    type Case<'a> = (&'a [&'a str], &'a [(&'a str, &'a str)]);
    let cases: [Case<'_>; 4] = [
        (&["status"], &[]),
        (&["status"], &[("TERM", "dumb")]),
        (&["status", "--json"], &[]),
        (&["ps"], &[]),
    ];
    for (args, envs) in cases {
        let mut command = Command::new(env!("CARGO_BIN_EXE_slipfork"));
        command
            .args(args)
            .current_dir(&root)
            .env("HOME", root.join("home"))
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_RUNTIME_DIR", root.join("run"))
            .env("XDG_STATE_HOME", root.join("state"))
            .env_remove("NO_COLOR")
            .env("TERM", "xterm-256color");
        for (key, value) in envs {
            command.env(key, value);
        }
        let output = command.output().expect("the slipfork binary must run");
        assert!(
            !String::from_utf8_lossy(&output.stdout).contains(ANSI),
            "{args:?} with {envs:?} emitted ANSI"
        );
    }
    let json = isolated(&root, &["status", "--json"]);
    let parsed: Value = serde_json::from_slice(&json.stdout).expect("status --json parses");
    assert_eq!(parsed["worlds"], json!([]));
    let _ = fs::remove_dir_all(&root);
}

fn row_count(text: &str) -> usize {
    text.lines()
        .filter(|line| line.starts_with("  ✓") || line.starts_with("  ✗"))
        .count()
}

fn next_count(text: &str) -> usize {
    text.lines()
        .filter(|line| line.trim_start().starts_with("next:"))
        .count()
}

fn userns_ok() -> bool {
    Command::new("/usr/bin/unshare")
        .arg("--user")
        .arg("/usr/bin/true")
        .output()
        .is_ok_and(|output| output.status.success())
}

#[test]
fn doctor_has_four_plain_rows_and_fails_closed_on_missing_tools() {
    let root = scratch("doctor");
    let injected_path = root.join("bin");
    fs::create_dir_all(&injected_path).expect("create injected PATH");
    let unshare_available = Path::new("/usr/bin/unshare").is_file();
    if unshare_available {
        std::os::unix::fs::symlink("/usr/bin/unshare", injected_path.join("unshare"))
            .expect("link unshare into the injected PATH");
        std::os::unix::fs::symlink("/usr/bin/true", injected_path.join("true"))
            .expect("link true into the injected PATH");
    }
    let btrfs = slipfork_world::is_btrfs_path(&root).unwrap_or(false)
        || slipfork_world::is_btrfs_path(&root.join("state")).unwrap_or(false);
    // The injected PATH has no crun and no systemctl, the runtime has no daemon socket, and a
    // non-Btrfs scratch root fails Storage: every row is closed.
    let expected_issues = 4 - usize::from(btrfs);
    let output = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .arg("doctor")
        .current_dir(&root)
        .env("PATH", &injected_path)
        .env("HOME", root.join("home"))
        .env("XDG_RUNTIME_DIR", root.join("run"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("NO_COLOR", "1")
        .output()
        .expect("doctor must run");
    let stdout = String::from_utf8(output.stdout).expect("doctor is UTF-8");
    assert_eq!(row_count(&stdout), 4, "{stdout}");
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.starts_with(&format!("✗ {expected_issues} issue(s) found")),
        "{stdout}"
    );
    assert_eq!(next_count(&stdout), expected_issues, "{stdout}");
    for name in ["Storage", "Containers", "Daemon", "Domains"] {
        assert!(stdout.contains(name), "missing {name}: {stdout}");
    }
    assert!(stdout.contains("crun not installed"), "{stdout}");
    for jargon in ["netns", "ingress", "unprivileged user", ":80"] {
        assert!(!stdout.contains(jargon), "leaked `{jargon}`: {stdout}");
    }
    assert!(!stdout.contains(ANSI), "piped doctor stays plain: {stdout}");

    let json = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .args(["doctor", "--json"])
        .current_dir(&root)
        .env("PATH", &injected_path)
        .env("HOME", root.join("home"))
        .env("XDG_RUNTIME_DIR", root.join("run"))
        .env("XDG_STATE_HOME", root.join("state"))
        .output()
        .expect("doctor --json must run");
    let parsed: Value = serde_json::from_slice(&json.stdout).expect("doctor --json parses");
    assert_eq!(parsed["ready"], false);
    let checks = parsed["checks"].as_array().expect("checks is an array");
    assert_eq!(checks.len(), 4);
    let names: Vec<&str> = checks
        .iter()
        .map(|check| check["name"].as_str().expect("check name"))
        .collect();
    assert_eq!(names, ["Storage", "Containers", "Daemon", "Domains"]);
    assert!(!String::from_utf8_lossy(&json.stdout).contains(ANSI));

    // With real tools on PATH the four rows stay four and the verdict matches the header.
    let crun_available = Command::new("/usr/bin/crun")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success());
    let ready = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .arg("doctor")
        .current_dir(&root)
        .env("HOME", root.join("home"))
        .env("XDG_RUNTIME_DIR", root.join("run"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("NO_COLOR", "1")
        .output()
        .expect("doctor must run");
    let stdout = String::from_utf8(ready.stdout).expect("doctor is UTF-8");
    assert_eq!(row_count(&stdout), 4, "{stdout}");
    let ready_line = stdout.starts_with("✓ Ready");
    assert_eq!(
        ready.status.code(),
        Some(if ready_line { 0 } else { 1 }),
        "{stdout}"
    );
    if crun_available && unshare_available && userns_ok() {
        assert!(
            stdout.contains("rootless container sandboxing enabled"),
            "{stdout}"
        );
    } else {
        assert!(stdout.contains("rootless container sandboxing"), "{stdout}");
    }
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn daemon_install_writes_every_unit_byte_for_byte() {
    let root = scratch("install");
    let exe = PathBuf::from(env!("CARGO_BIN_EXE_slipfork"));
    let writer = UnitWriter {
        system_root: root.join("etc/systemd/system"),
        user_root: root.join("config/systemd/user"),
        config_root: root.join("config"),
        apparmor_root: root.join("etc/apparmor.d"),
        conf_d_root: root.join("etc/conf.d"),
        exe: exe.clone(),
        ingress_uid: 1000,
        ingress_user: "builder".to_owned(),
        openrc: false,
    };
    let written = writer.write_all(true).expect("write every unit");
    assert_eq!(written.len(), 4, "{written:?}");
    let exe = exe.display().to_string();
    assert_eq!(
        fs::read_to_string(writer.user_root.join("slipfork.service")).expect("user unit"),
        install::USER_UNIT.replace("{exe}", &exe)
    );
    assert_eq!(
        fs::read_to_string(writer.system_root.join(install::SOCKET_UNIT)).expect("socket unit"),
        install::INGRESS_SOCKET
    );
    assert_eq!(
        fs::read_to_string(writer.system_root.join(install::SERVICE_UNIT)).expect("service unit"),
        install::INGRESS_SERVICE
            .replace("{exe}", &exe)
            .replace("{uid}", "1000")
    );
    assert_eq!(
        fs::read_to_string(writer.apparmor_root.join(install::APPARMOR_FILE)).expect("profile"),
        install::APPARMOR_PROFILE
    );
    for path in &written {
        let mode = fs::metadata(path)
            .expect("stat the unit")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o644, "{}", path.display());
    }

    // Without root the CLI must refuse before writing anything.
    let denied = isolated(&root, &["daemon", "install", "--ingress"]);
    assert_eq!(denied.status.code(), Some(1));
    let stderr = String::from_utf8(denied.stderr).expect("errors are UTF-8");
    assert!(
        stderr.contains("next: sudo slipfork daemon install --ingress"),
        "{stderr}"
    );
    assert!(
        !root.join("config/systemd/user/slipfork.service").exists()
            || root.join("etc/systemd/system").exists(),
        "root validation must happen before any write"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_real_pty_stays_plain_under_no_color_and_term_dumb() {
    if !Command::new("script")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
    {
        eprintln!("skipping PTY assertion: `script` (util-linux) is not available");
        return;
    }
    let root = scratch("pty");
    for envs in [vec![("NO_COLOR", "1")], vec![("TERM", "dumb")]] {
        let line = format!("'{}' status", env!("CARGO_BIN_EXE_slipfork"));
        let mut command = Command::new("script");
        command
            .args(["-qec", &line, "/dev/null"])
            .current_dir(&root)
            .env("HOME", root.join("home"))
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_RUNTIME_DIR", root.join("run"))
            .env("XDG_STATE_HOME", root.join("state"))
            .env("TERM", "xterm-256color")
            .env_remove("NO_COLOR");
        for (key, value) in &envs {
            command.env(key, value);
        }
        let output = command.output().expect("script must run");
        assert!(output.status.success(), "{output:?}");
        assert!(
            !String::from_utf8_lossy(&output.stdout).contains(ANSI),
            "{envs:?} emitted ANSI on a PTY"
        );
    }
    let _ = fs::remove_dir_all(&root);
}
