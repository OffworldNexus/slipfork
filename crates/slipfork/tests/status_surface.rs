//! Lab tests for the pure status renderer and `doctor`.
//!
//! The renderer journey feeds real `ps` JSON through the public typed renderer; the CLI cases run
//! the real binary against a daemon-free `/var/tmp` runtime.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use slipfork::boot::{Node, boot_widths, completed_row, elapsed_text, pane_geometry, status_line};
use slipfork::status::{self, Options};
use slipfork::ui::table::{ColumnWidths, visible_width};
use slipfork_world::LogStore;

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
    let mount = |project: &str| slipfork::mounts::mount_for(&mounts, Path::new(project));
    assert_eq!(
        mount("/var/lib/slipfork/t/fork-live-1/demo"),
        Path::new("/var/lib/slipfork")
    );
    assert_eq!(
        mount("/var/lib/slipfork2/demo"),
        Path::new("/var/lib/slipfork2"),
        "the nested mount must win over its parent"
    );
    assert_eq!(mount("/var/lib/slipfork-other/demo"), Path::new("/"));
    assert_eq!(
        mount(r"/mnt/my drive\data/app"),
        Path::new(r"/mnt/my drive\data"),
        "octal-unescaped mounts stay matchable"
    );

    let data_home = Path::new("/var/tmp/slipfork-data");
    assert_eq!(
        LogStore::root_for(
            &mounts,
            Path::new("/var/lib/slipfork-other/demo"),
            data_home,
            true
        ),
        data_home.join("slipfork/store"),
        "a `/`-rooted project resolves under data_home, never `/.slipfork-store`"
    );
    assert_eq!(
        LogStore::root_for(
            &mounts,
            Path::new("/var/lib/slipfork2/demo"),
            data_home,
            true
        ),
        PathBuf::from("/var/lib/slipfork2/.slipfork-store"),
        "a writable nested mount owns its own store"
    );
    assert_eq!(
        LogStore::root_for(
            &mounts,
            Path::new("/var/lib/slipfork2/demo"),
            data_home,
            false
        ),
        data_home.join("slipfork/store"),
        "a mount this user cannot write resolves under data_home"
    );
}

const ANSI: &str = "\u{1b}[";

/// Measures the six boot columns over the same four leading cells the live plan carries.
fn plan_widths(nodes: &[&Node<'_>]) -> ColumnWidths {
    let rows: Vec<Vec<String>> = nodes
        .iter()
        .map(|node| {
            vec![
                String::new(),
                node.kind.to_owned(),
                node.name.to_owned(),
                node.state.to_lowercase(),
            ]
        })
        .collect();
    boot_widths(&rows)
}

/// Removes every `ESC[ … m` sequence, leaving the visible cells.
fn strip_ansi(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for ch in chars.by_ref() {
                if ch.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// The visible column index where `needle` starts on one rendered row.
fn column_start(line: &str, needle: &str) -> usize {
    let byte = line
        .find(needle)
        .unwrap_or_else(|| panic!("`{needle}` missing from `{line}`"));
    visible_width(&line[..byte])
}

#[test]
fn every_boot_row_keeps_one_start_per_role_and_one_right_edge() {
    let long = format!("service-{}", "x".repeat(20));
    assert_eq!(long.chars().count(), 28);
    let nodes = vec![
        Node {
            kind: "pg",
            name: "db",
            state: "running",
            millis: 7_300,
            run_id: None,
            success: true,
        },
        Node {
            kind: "image",
            name: "base",
            state: "cached",
            millis: 812,
            run_id: None,
            success: true,
        },
        Node {
            kind: "service",
            name: "api",
            state: "running",
            millis: 3,
            run_id: None,
            success: true,
        },
        Node {
            kind: "service",
            name: "web",
            state: "starting",
            millis: 12,
            run_id: None,
            success: true,
        },
        Node {
            kind: "redis",
            name: "cache",
            state: "running",
            millis: 45,
            run_id: None,
            success: true,
        },
        Node {
            kind: "postgres",
            name: "pg",
            state: "running",
            millis: 900,
            run_id: None,
            success: true,
        },
        Node {
            kind: "service",
            name: "worker",
            state: "cached",
            millis: 1,
            run_id: None,
            success: false,
        },
        Node {
            kind: "service",
            name: long.as_str(),
            state: "running",
            millis: 7_300,
            run_id: None,
            success: true,
        },
        Node {
            kind: "service",
            name: "docs",
            state: "up to date",
            millis: 3,
            run_id: None,
            success: false,
        },
    ];
    let widths = plan_widths(&nodes.iter().collect::<Vec<_>>());
    let plain: Vec<String> = nodes
        .iter()
        .map(|node| completed_row(node, &widths, false))
        .collect();
    let styled: Vec<String> = nodes
        .iter()
        .map(|node| completed_row(node, &widths, true))
        .collect();

    for (index, node) in nodes.iter().enumerate() {
        assert!(
            !plain[index].contains(ANSI),
            "row {index} is plain: {:?}",
            plain[index]
        );
        assert!(
            styled[index].contains(ANSI),
            "row {index} is styled: {:?}",
            styled[index]
        );
        let visible = strip_ansi(&styled[index]);
        assert_eq!(
            visible, plain[index],
            "row {index} styles without moving a cell"
        );
        for line in [&plain[index], &visible] {
            assert_eq!(
                column_start(line, node.kind),
                column_start(&plain[0], "pg"),
                "kind start drifts on `{line}`"
            );
            assert_eq!(
                column_start(line, node.name),
                column_start(&plain[1], "base"),
                "name start drifts on `{line}`"
            );
            assert_eq!(
                column_start(line, node.state),
                column_start(&plain[1], "cached"),
                "state start drifts on `{line}`"
            );
            assert_eq!(
                visible_width(line.trim_end()),
                visible_width(plain[1].trim_end()),
                "duration right edge drifts on `{line}`"
            );
        }
    }
    assert!(plain[8].contains("up to date"), "{:?}", plain[8]);
    assert!(plain[7].contains(&long), "{:?}", plain[7]);
}

#[test]
fn the_live_boot_pane_renders_exact_rows_and_bars() {
    let node = Node {
        kind: "image",
        name: "base",
        state: "Cached",
        millis: 420,
        run_id: None,
        success: true,
    };
    let widths = plan_widths(&[&node]);
    assert_eq!(
        completed_row(&node, &widths, true),
        "\u{1b}[32m✓\u{1b}[0m  \u{1b}[2mimage\u{1b}[0m  \u{1b}[1mbase\u{1b}[0m  \u{1b}[36mcached\u{1b}[0m   \u{1b}[2m420ms\u{1b}[0m"
    );
    assert_eq!(
        completed_row(&node, &widths, false),
        "✓  image  base  cached   420ms"
    );
    assert!(!completed_row(&node, &widths, false).contains(ANSI));
    assert!(!completed_row(&node, &widths, false).contains('\r'));

    let cached = Node {
        state: "Up-to-date",
        run_id: None,
        success: false,
        ..node
    };
    let widths = plan_widths(&[&cached]);
    assert_eq!(
        completed_row(&cached, &widths, true),
        "\u{1b}[2m•\u{1b}[0m  \u{1b}[2mimage\u{1b}[0m  \u{1b}[1mbase\u{1b}[0m  \u{1b}[36mup-to-date\u{1b}[0m   \u{1b}[2m420ms\u{1b}[0m"
    );
    assert_eq!(
        completed_row(&cached, &widths, false),
        "•  image  base  up-to-date   420ms"
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

    assert_eq!(pane_geometry(120), Some(24));
    assert_eq!(pane_geometry(80), Some(24));
    assert_eq!(pane_geometry(79), None);

    let elapsed = Duration::from_millis(1_200);
    let panel = status_line('-', 6, 10, "Starting api…", elapsed, 120);
    assert_eq!(
        panel,
        "- [##############..........]  6/10  60% · Starting api… · 1.2s"
    );
    let cells = panel
        .split_once('[')
        .and_then(|(_, rest)| rest.split_once(']'))
        .map(|(cells, _)| cells)
        .expect("the panel keeps its bar");
    assert_eq!(cells.chars().count(), 24, "{panel}");
    assert_eq!(cells.matches('#').count(), 14, "{panel}");
    assert_eq!(
        status_line('-', 6, 10, "Starting api…", elapsed, 40),
        "- 6/10  60% · Starting api… · 1.2s"
    );
    assert_eq!(
        status_line('-', 6, 10, "Starting api…", elapsed, 79),
        "- 6/10  60% · Starting api… · 1.2s"
    );
    assert!(!panel.contains(ANSI), "{panel}");
    assert!(!panel.contains('\r'), "{panel}");

    for spinner in ['|', '/', '-', '\\'] {
        let line = status_line(
            spinner,
            3,
            10,
            "Restarting api…",
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
            24,
            "{line}"
        );
    }
}

#[test]
fn orphaned_pods_render_yellow_and_serialize() {
    let orphaned = reply(json!({
        "worlds": [{
            "project": "/home/me/demo", "path": "/home/me/demo", "world": "demo",
            "url": "http://demo.localhost", "supervisor_pid": 1, "alive": true,
            "booting": false,
            "services": [
                { "id": "api", "kind": "service", "running": true, "port": 8000 },
                { "id": "beta", "kind": "service", "running": true, "orphaned": true }
            ]
        }]
    }));
    let plain = status::render(&orphaned, &Options::new(false));
    assert!(plain.contains("orphaned"), "{plain}");
    let coloured = status::render(&orphaned, &Options::new(true));
    assert!(
        coloured.contains("\u{1b}[1;33morphaned\u{1b}[0m"),
        "orphaned must be bold yellow: {coloured}"
    );
    let value = serde_json::to_value(&orphaned).expect("ps reply serializes");
    assert_eq!(value["worlds"][0]["services"][1]["orphaned"], true);
    let defaulted = reply(json!({
        "worlds": [{
            "project": "/home/me/demo", "path": "/home/me/demo", "world": "demo",
            "url": "http://demo.localhost", "supervisor_pid": 1, "alive": true,
            "booting": false,
            "services": [{ "id": "api", "kind": "service", "running": true }]
        }]
    }));
    assert!(!defaulted.worlds[0].services[0].orphaned);
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

fn demo_reply() -> slipfork::ps::PsReply {
    reply(json!({
        "worlds": [{
            "project": "/home/me/projects/demo",
            "path": "/home/me/projects/demo",
            "world": "demo",
            "url": "http://demo.localhost",
            "started_at_ms": 1_000_000_000,
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
                               "domain": "mail" }] },
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
                "started_at_ms": 1_000_000_500,
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
    let text = status::render(
        &demo_reply(),
        &Options::new(false).with_home(Some(PathBuf::from("/home/me"))),
    );
    assert!(text.starts_with("● demo       ~/projects/demo\n"), "{text}");
    for (prefix, state) in [
        ("  ├─ http://demo.localhost", ""),
        ("  │  ├─ /api  ->  api", "running"),
        ("  │  └─ /     ->  front", "running"),
        ("  ├─ http://mail.demo.localhost", ""),
        ("  │  └─ /     ->  mailpit", "running"),
        ("  ├─ http://uploads.demo.localhost", ""),
        ("  │  └─ /     ->  uploads  s3 (private)", "running"),
        ("  ├─ http://assets.demo.localhost", ""),
        ("  │  └─ /     ->  assets   s3 (public-read)", "running"),
        ("  └─ support", ""),
        ("     ├─ worker  service", "running"),
        ("     ├─ pg      postgres 18", "running"),
        ("     └─ cache   redis 7", "running"),
    ] {
        let line = text
            .lines()
            .find(|line| line.starts_with(prefix))
            .unwrap_or_else(|| panic!("missing `{prefix}` in:\n{text}"));
        if !state.is_empty() {
            assert!(
                line.trim_end().ends_with(state),
                "`{prefix}` lost its `{state}` state: `{line}`"
            );
        }
    }
    assert!(
        text.contains("● feat-auth  ~/projects/demo/.forks/feat-auth"),
        "{text}"
    );
    assert!(!text.contains("  └─ forks"), "{text}");
    assert!(!text.contains(":8000"), "{text}");
    assert!(!text.contains(":8333"), "{text}");
    assert!(!text.contains("12m"), "{text}");
    assert!(!text.contains("4m"), "{text}");
    assert!(!text.contains("assets-cdn"), "{text}");
    assert!(!text.contains(ANSI), "plain renders emit zero ANSI: {text}");
}

#[test]
fn crashed_rows_render_a_bold_red_failed_state() {
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
    assert!(plain.contains("postgres 18  failed"), "{plain}");
    assert!(plain.contains("s3 (private)  failed"), "{plain}");
    let colored = status::render(&reply, &Options::new(true));
    assert!(colored.contains("\u{1b}[1;31mfailed\u{1b}[0m"), "{colored}");
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
            }],
            "stopped_forks": [{
                "name": long,
                "path": format!("/home/me/demo/.forks/{long}")
            }]
        }]
    }));
    let text = status::render(
        &reply,
        &Options::new(false).with_home(Some("/home/me".into())),
    );
    assert!(text.contains(long), "the full name must survive: {text}");
    let live = text
        .lines()
        .find(|line| line.starts_with("● demo"))
        .unwrap_or_else(|| panic!("missing the live world header: {text}"));
    let stopped = text
        .lines()
        .find(|line| line.starts_with("○ "))
        .unwrap_or_else(|| panic!("missing the stopped fork row: {text}"));
    assert_eq!(
        column_start(stopped, long),
        column_start(live, "demo"),
        "a long stopped fork keeps the world name column: {text}"
    );
    assert_eq!(
        column_start(stopped, &format!("~/demo/.forks/{long}")),
        column_start(live, "~/demo"),
        "a long stopped fork keeps the world path column: {text}"
    );
}

#[test]
fn ports_and_ages_stay_in_json_but_never_in_the_human_tree() {
    let reply = demo_reply();
    let plain = status::render(
        &reply,
        &Options::new(false).with_home(Some("/home/me".into())),
    );
    for hidden in [":8000", ":8025", ":8333", ":5432", "12m", "4m"] {
        assert!(
            !plain.contains(hidden),
            "human output leaked `{hidden}`: {plain}"
        );
    }
    let value = serde_json::to_value(&reply).expect("ps reply serializes");
    assert_eq!(value["worlds"][0]["started_at_ms"], 1_000_000_000_u64);
    assert_eq!(value["worlds"][0]["services"][0]["port"], 8000);
    assert_eq!(value["worlds"][0]["services"][7]["orphaned"], false);
    let colored = status::render(&reply, &Options::new(true));
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
    // The unit name is a copy-verbatim remedy; the jargon guard covers the rest.
    let readable = stdout.replace("slipfork-ingress", "");
    for jargon in ["netns", "ingress", "unprivileged user", ":80"] {
        assert!(!readable.contains(jargon), "leaked `{jargon}`: {stdout}");
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

#[test]
fn stopped_forks_and_honest_problems_render_without_fabrication() {
    let home = Some(PathBuf::from("/home/me"));
    let stopped = reply(json!({
        "worlds": [{
            "project": "/home/me/demo", "path": "/home/me/demo", "world": "demo",
            "url": "http://demo.localhost", "supervisor_pid": 11, "alive": true,
            "booting": false,
            "services": [{ "id": "api", "kind": "service", "running": true }],
            "stopped_forks": [{
                "name": "feat-auth",
                "path": "/home/me/demo/.forks/feat-auth"
            }]
        }]
    }));
    let plain = status::render(&stopped, &Options::new(false).with_home(home.clone()));
    assert!(
        plain.contains("○ feat-auth  ~/demo/.forks/feat-auth  stopped"),
        "a stopped fork renders one dim row with its shortened path: {plain}"
    );
    assert!(
        plain.contains("api") && plain.contains("running"),
        "{plain}"
    );
    assert!(
        !plain.contains(ANSI),
        "plain renders emit zero ANSI: {plain}"
    );
    let coloured = status::render(&stopped, &Options::new(true).with_home(home.clone()));
    assert!(coloured.contains("\u{1b}[2m○ "), "{coloured}");
    assert!(coloured.contains("\u{1b}[2mstopped\u{1b}[0m"), "{coloured}");
    // A cwd outside any project root fails closed: no legacy path match resurrects a row.
    assert!(
        status::scope(
            &stopped,
            &slipfork_world::ProjectScope::resolve(Path::new("/home/me/demo/.forks/feat-auth")),
        )
        .worlds
        .is_empty(),
        "a missing project root never fabricates a scoped world"
    );

    let invalid = reply(json!({
        "worlds": [{
            "project": "/home/me/demo", "path": "/home/me/demo", "world": "demo",
            "url": "http://demo.localhost", "supervisor_pid": 11, "alive": true,
            "booting": false,
            "problem": "Forkfile invalid: 3:1: unknown resource",
            "services": [{ "id": "api", "kind": "service", "running": true }]
        }]
    }));
    let text = status::render(&invalid, &Options::new(false));
    assert!(
        text.contains("! Forkfile invalid: 3:1: unknown resource"),
        "{text}"
    );
    assert!(
        text.contains("api") && text.contains("running"),
        "a live pod must stay running under an invalid Forkfile: {text}"
    );

    let missing = reply(json!({
        "worlds": [{
            "project": "/home/me/demo", "path": "/home/me/demo", "world": "demo",
            "url": "http://demo.localhost", "supervisor_pid": 11, "alive": true,
            "booting": false,
            "problem": "checkout missing — run `slipfork drop feat-auth`",
            "services": []
        }]
    }));
    let text = status::render(&missing, &Options::new(false));
    assert!(
        text.contains("! checkout missing — run `slipfork drop feat-auth`"),
        "{text}"
    );
    assert!(!text.contains("orphaned"), "no fabricated rows: {text}");

    let value = serde_json::to_value(&stopped).expect("ps reply serializes");
    assert_eq!(value["worlds"][0]["stopped_forks"][0]["name"], "feat-auth");
    assert_eq!(
        value["worlds"][0]["services"][0]["running"], true,
        "JSON keeps the live service"
    );
    assert!(
        value["worlds"][0].get("problem").is_none(),
        "an absent problem stays out of the wire"
    );
    assert!(
        !serde_json::to_string(&stopped)
            .expect("serialize")
            .contains('\u{1b}')
    );
}

#[test]
fn a_stopped_fork_renders_after_every_live_world_tree() {
    let reply = reply(json!({
        "worlds": [{
            "project": "/home/me/demo", "path": "/home/me/demo", "world": "demo",
            "url": "http://demo.localhost", "supervisor_pid": 11, "alive": true,
            "booting": false,
            "services": [
                { "id": "api", "kind": "service", "running": true, "port": 8000,
                  "routes": [{ "prefix": "/api", "port": 8000, "mode": "preserve" }] },
                { "id": "worker", "kind": "service", "running": true }
            ],
            "forks": [{
                "project": "/home/me/demo/.forks/feat-auth",
                "path": "/home/me/demo/.forks/feat-auth",
                "world": "feat-auth--demo",
                "fork": "feat-auth",
                "url": "http://feat-auth--demo.localhost",
                "supervisor_pid": 12, "alive": true, "booting": false,
                "services": [{ "id": "api", "kind": "service", "running": true, "port": 8000,
                               "routes": [{ "prefix": "/api", "port": 8000, "mode": "preserve" }] }]
            }],
            "stopped_forks": [{ "name": "tralala", "path": "/home/me/demo/.forks/tralala" }]
        }]
    }));
    let text = status::render(
        &reply,
        &Options::new(false).with_home(Some(PathBuf::from("/home/me"))),
    );
    let expected = concat!(
        "● demo       ~/demo\n",
        "  ├─ http://demo.localhost\n",
        "  │  └─ /api  ->  api  running\n",
        "  └─ support\n",
        "     └─ worker  service  running\n",
        "\n",
        "● feat-auth  ~/demo/.forks/feat-auth\n",
        "  └─ http://feat-auth--demo.localhost\n",
        "     └─ /api  ->  api  running\n",
        "\n",
        "○ tralala    ~/demo/.forks/tralala    stopped\n"
    );
    assert_eq!(text, expected);
    assert!(
        text.find("● feat-auth").unwrap() < text.find("○ tralala").unwrap(),
        "a live fork never renders below a stopped one:\n{text}"
    );
}

#[test]
fn a_down_root_still_scopes_running_forks_and_stopped_siblings_from_disk() {
    let root = scratch("scope-down");
    let demo = root.join("demo");
    let forks = demo.join(".forks");
    // A copied plain checkout is a real fork; a name `ForkName::parse` rejects is ignored.
    for (path, name) in [
        (&demo, "demo"),
        (&forks.join("youpi"), "youpi"),
        (&forks.join("feat-auth"), "feat-auth"),
        (&forks.join("Junk dir"), "junk"),
    ] {
        fs::create_dir_all(path).expect("create the lab project");
        fs::write(
            path.join("Forkfile"),
            format!("# forkfile: 0.1\n# {name}\n"),
        )
        .expect("write the lab Forkfile");
    }
    fs::create_dir_all(demo.join(".slipfork")).expect("create the root record directory");

    // The daemon is running the fork only: the root world was `down`, so its row is absent.
    let fork_path = forks.join("feat-auth");
    let live = reply(json!({
        "worlds": [{
            "project": fork_path,
            "path": fork_path,
            "world": "feat-auth--demo",
            "fork": "feat-auth",
            "url": "http://feat-auth--demo.localhost",
            "started_at_ms": 42,
            "supervisor_pid": 77,
            "alive": true,
            "booting": false,
            "services": [{
                "id": "api", "kind": "service", "running": true,
                "routes": [{ "prefix": "/", "port": 8000, "mode": "preserve" }]
            }]
        }]
    }));
    let options = Options::new(false).with_home(Some(root.clone()));

    // The project root shows the stopped root and the live fork; a copied plain checkout under
    // `.forks` is a real stopped fork, while an invalid name is never treated as one.
    let scoped = status::scope(&live, &slipfork_world::ProjectScope::resolve(&demo));
    assert_eq!(
        scoped.worlds.len(),
        1,
        "one canonical project root: {scoped:?}"
    );
    assert_eq!(
        scoped.worlds[0].state,
        slipfork::ps::PsWorldState::Stopped,
        "the down root renders stopped"
    );
    assert_eq!(scoped.worlds[0].world, "demo");
    assert_eq!(scoped.worlds[0].forks.len(), 1);
    assert_eq!(scoped.worlds[0].forks[0].fork.as_deref(), Some("feat-auth"));
    assert_eq!(
        scoped.worlds[0].stopped_forks.len(),
        1,
        "a copied plain checkout is a stopped fork: {scoped:?}"
    );
    assert_eq!(scoped.worlds[0].stopped_forks[0].name, "youpi");
    let text = status::render(&scoped, &options);
    assert!(text.contains("○ demo"), "{text}");
    assert!(text.contains("● feat-auth"), "{text}");
    assert!(
        text.contains("○ youpi"),
        "a plain copied checkout renders as a stopped fork: {text}"
    );
    assert!(
        !text.contains("Junk dir"),
        "a directory whose name is not a fork must not pose as one: {text}"
    );
    assert_eq!(text.matches("○ demo").count(), 1, "{text}");
    assert_eq!(text.matches("● feat-auth").count(), 1, "{text}");
    assert!(!text.contains('\u{1b}'), "{text}");

    // A cwd inside a stopped checkout (a real directory on disk) scopes to that one dim row.
    let inside = status::scope(
        &live,
        &slipfork_world::ProjectScope::resolve(&forks.join("youpi")),
    );
    assert_eq!(inside.worlds.len(), 1);
    assert_eq!(inside.worlds[0].state, slipfork::ps::PsWorldState::Stopped);
    assert_eq!(inside.worlds[0].fork.as_deref(), Some("youpi"));
    let text = status::render(&inside, &options);
    assert!(
        text.contains("○ youpi") && !text.contains("feat-auth"),
        "{text}"
    );

    // A cwd deep inside the running fork still scopes strictly to that fork.
    let running = status::scope(
        &live,
        &slipfork_world::ProjectScope::resolve(&fork_path.join("src")),
    );
    assert_eq!(running.worlds.len(), 1);
    assert_eq!(running.worlds[0].state, slipfork::ps::PsWorldState::Live);
    let text = status::render(&running, &options);
    assert!(
        text.contains("● feat-auth") && !text.contains("● demo") && !text.contains("○ demo"),
        "{text}"
    );

    fs::remove_dir_all(&root).expect("remove the scratch lab");
}
