//! Exact human contract of the flat, domain-first status tree.
//!
//! The fixture is the full demo surface: two service paths on the primary domain, a `mail.*`
//! service domain, two S3 bucket domains, a `support` group, and one flat fork. Every row must
//! stay aligned, carry no port or age, and style only through the named palette roles.

use std::path::PathBuf;
use std::process::Command;

use serde_json::{Value, json};
use slipfork::status::{self, Options};

const ESCAPE: char = '\u{1b}';

fn reply() -> slipfork::ps::PsReply {
    status::parse(&json!({
        "worlds": [{
            "project": "/home/me/demo",
            "path": "/home/me/demo",
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
                               "domain": { "kind": "prefix", "value": "mail" } }] },
                { "id": "pg", "kind": "postgres", "running": true, "port": 5432, "version": "18" },
                { "id": "cache", "kind": "redis", "running": true, "port": 6379, "version": "7" },
                { "id": "uploads", "kind": "s3", "running": true, "port": 8333, "policy": "private" },
                { "id": "assets", "kind": "s3", "running": true, "port": 8333,
                  "policy": "public-read", "cdn": "assets-cdn" }
            ],
            "forks": [{
                "project": "/home/me/demo/.forks/feat-auth",
                "path": "/home/me/demo/.forks/feat-auth",
                "world": "feat-auth--demo",
                "fork": "feat-auth",
                "url": "http://feat-auth--demo.localhost",
                "started_at_ms": 1_000_000_500,
                "supervisor_pid": 12,
                "alive": true,
                "booting": false,
                "services": [{ "id": "api", "kind": "service", "running": true, "port": 8000,
                               "routes": [{ "prefix": "/api", "port": 8000, "mode": "preserve" }] }]
            }]
        }]
    }))
    .expect("the demo payload follows the ps schema")
}

fn plain() -> String {
    status::render(
        &reply(),
        &Options::new(false).with_home(Some(PathBuf::from("/home/me"))),
    )
}

#[test]
fn the_flat_domain_tree_renders_exact_aligned_lines() {
    let expected = concat!(
        "● demo       ~/demo\n",
        "  ├─ http://demo.localhost\n",
        "  │  ├─ /api  -> api                       running\n",
        "  │  └─ /     -> front                     running\n",
        "  ├─ http://mail.demo.localhost\n",
        "  │  └─ /     -> mailpit                   running\n",
        "  ├─ http://uploads.demo.localhost\n",
        "  │  └─ /     -> uploads  s3 (private)     running\n",
        "  ├─ http://assets.demo.localhost\n",
        "  │  └─ /     -> assets   s3 (public-read) running\n",
        "  └─ support\n",
        "     ├─ worker   service      running\n",
        "     ├─ pg       postgres 18  running\n",
        "     └─ cache    redis 7      running\n",
        "\n",
        "● feat-auth  ~/demo/.forks/feat-auth\n",
        "  └─ http://feat-auth--demo.localhost\n",
        "     └─ /api  -> api                       running\n"
    );
    assert_eq!(plain(), expected);
}

#[test]
fn every_s3_bucket_owns_a_uniform_domain_and_the_cdn_adds_none() {
    let text = plain();
    for line in [
        "http://uploads.demo.localhost",
        "http://assets.demo.localhost",
    ] {
        assert!(text.contains(line), "missing bucket domain {line}: {text}");
    }
    assert!(
        !text.contains("assets-cdn.demo.localhost"),
        "a local cdn alias must not add a host: {text}"
    );
    assert!(
        !text.contains("cdn"),
        "a local cdn alias must not render a separate row: {text}"
    );
    for hidden in [":8000", ":8025", ":8333", ":5432", ":6379", "12m", "4m"] {
        assert!(
            !text.contains(hidden),
            "human output leaked `{hidden}`: {text}"
        );
    }
    assert!(!text.contains("  └─ forks"), "{text}");
}

#[test]
fn palette_roles_style_the_tree_and_plain_renders_have_zero_ansi() {
    let text = plain();
    assert!(
        !text.contains(ESCAPE),
        "plain output must stay pure: {text}"
    );

    let colored = status::render(
        &reply(),
        &Options::new(true).with_home(Some(PathBuf::from("/home/me"))),
    );
    for token in [
        "\u{1b}[2m├─ \u{1b}[0m",
        "\u{1b}[1;4;36mhttp://demo.localhost\u{1b}[0m",
        "\u{1b}[1;33m/api\u{1b}[0m",
        "\u{1b}[1;37mfront\u{1b}[0m",
        "\u{1b}[36ms3 (private)\u{1b}[0m",
        "\u{1b}[32mrunning\u{1b}[0m",
        "\u{1b}[2msupport\u{1b}[0m",
        "\u{1b}[1;37mdemo",
        "\u{1b}[1;36m~/demo\u{1b}[0m",
    ] {
        assert!(
            colored.contains(token),
            "missing style {token:?}: {colored}"
        );
    }
}

#[test]
fn orphaned_pods_render_bold_yellow_in_support() {
    let reply = status::parse(&json!({
        "worlds": [{
            "project": "/home/me/demo", "path": "/home/me/demo", "world": "demo",
            "url": "http://demo.localhost", "supervisor_pid": 1, "alive": true,
            "booting": false, "services": [
                { "id": "api", "kind": "service", "running": true, "port": 8000,
                  "routes": [{ "prefix": "/", "port": 8000, "mode": "preserve" }] },
                { "id": "beta", "kind": "service", "running": true, "orphaned": true }
            ]
        }]
    }))
    .expect("payload parses");
    let text = status::render(&reply, &Options::new(false));
    assert!(text.contains("beta   service  orphaned"), "{text}");
    let colored = status::render(&reply, &Options::new(true));
    assert!(
        colored.contains("\u{1b}[1;33morphaned\u{1b}[0m"),
        "{colored}"
    );
}

#[test]
fn json_keeps_ports_ages_routes_and_orphaned() {
    let value: Value = serde_json::to_value(reply()).expect("ps reply serializes");
    let world = &value["worlds"][0];
    assert_eq!(world["started_at_ms"], 1_000_000_000_u64);
    assert_eq!(world["url"], "http://demo.localhost");
    assert_eq!(world["services"][0]["port"], 8000);
    assert_eq!(world["services"][0]["routes"][0]["prefix"], "/api");
    assert_eq!(world["services"][7]["orphaned"], false);
    assert_eq!(world["forks"][0]["world"], "feat-auth--demo");
}

#[test]
fn a_piped_status_emits_no_escape_bytes() {
    let root = PathBuf::from("/var/tmp").join(format!(
        "slipfork-tree-domain-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock reads after the epoch")
            .as_nanos()
    ));
    for dir in ["home", "run", "state", "config"] {
        std::fs::create_dir_all(root.join(dir)).expect("create scratch directory");
    }
    let output = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .arg("status")
        .current_dir(&root)
        .env("HOME", root.join("home"))
        .env("XDG_RUNTIME_DIR", root.join("run"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env_remove("NO_COLOR")
        .env("TERM", "xterm-256color")
        .output()
        .expect("the slipfork binary must run");
    let stdout = String::from_utf8(output.stdout).expect("status is UTF-8");
    assert_eq!(stdout, "○ No worlds running\n");
    assert!(!stdout.contains(ESCAPE), "a pipe must never carry ANSI");
    let _ = std::fs::remove_dir_all(&root);
}
