//! Real-subprocess proof for the one concrete [`Browser`] every Composition Root injects.
//!
//! Each opener this suite hands to the browser is a real `0755` script on disk that records its
//! argv; there is no `Mock*`/`Marker*` type anywhere in the path.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use slipfork::browser::{APPROVAL_HINT_PREFIX, Browser, NO_BROWSER_REMEDY};
use slipfork::open::{WorldSelection, json, render_local};
use slipfork_world::{LocalHostRoute, ProjectSlug, WorldOrigin, WorldTarget};
use tempfile::TempDir;

/// A disk scratch directory (never the RAM-backed `/tmp`).
fn scratch(prefix: &str) -> TempDir {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("/var/tmp")
        .expect("a disk scratch directory")
}

/// A real executable script that truncates `record`, then appends each argument on its own line.
fn recording_opener(dir: &Path) -> (PathBuf, PathBuf) {
    let script = dir.join("opener.sh");
    let record = dir.join("argv.txt");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\n: > '{}'\nfor arg in \"$@\"; do printf '%s\\n' \"$arg\" >> '{}'; done\n",
            record.display(),
            record.display()
        ),
    )
    .expect("write the recording opener");
    let mut permissions = fs::metadata(&script)
        .expect("stat the opener")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script, permissions).expect("chmod the recording opener");
    (script, record)
}

/// Another test thread may fork between `fs::write` and `exec`, leaving the write fd open in the child (`ETXTBSY`).
fn open_settling(browser: &Browser, url: &str) -> Result<bool, String> {
    let mut last = String::new();
    for _ in 0..20 {
        match browser.open(url) {
            Err(err) if err.contains("Text file busy") || err.contains("os error 26") => {
                last = err;
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            result => return result,
        }
    }
    Err(last)
}

fn selection() -> WorldSelection {
    let route = LocalHostRoute::new(
        ProjectSlug::parse("demo").expect("valid project slug"),
        WorldTarget::Canonical,
        None,
        &[],
    )
    .expect("a routable .localhost route");
    WorldSelection {
        world: "demo".to_owned(),
        origin: WorldOrigin::local(&route, 80).expect("a minted origin"),
    }
}

#[test]
fn open_spawns_the_resolved_program_and_the_printed_surfaces_agree() {
    let dir = scratch("slipfork-browser-open-");
    let (script, record) = recording_opener(dir.path());
    let selection = selection();
    let url = selection.origin.url();

    let launched = open_settling(
        &Browser::new(fs::canonicalize(&script).expect("absolute opener"), true),
        url,
    )
    .expect("the real opener exits zero");
    assert!(launched, "a desktop session spawns the opener");

    assert_eq!(
        fs::read_to_string(&record)
            .expect("the opener recorded its argv")
            .trim(),
        url,
        "the opener receives exactly the URL the surfaces print"
    );
    assert!(render_local(&selection).contains(url));
    assert_eq!(json(&selection, None)["origin"], url);
    assert!(
        !render_local(&selection).contains('\u{1b}'),
        "a non-TTY surface emits no escapes"
    );
}

#[test]
fn gio_passes_its_open_subcommand_before_the_url() {
    let dir = scratch("slipfork-browser-gio-");
    let (script, record) = recording_opener(dir.path());
    let url = "http://demo.localhost";

    open_settling(
        &Browser::gio(fs::canonicalize(&script).expect("absolute opener"), true),
        url,
    )
    .expect("the real opener exits zero");

    assert_eq!(
        fs::read_to_string(&record).expect("argv"),
        format!("open\n{url}\n")
    );
}

#[test]
fn the_live_pairing_url_never_reaches_an_error_line() {
    let page = format!("http://127.0.0.1:1/pair#handle={}", "ab".repeat(32));

    let missing = Browser::none()
        .open(&page)
        .expect_err("a host with no opener never reports success");
    assert_eq!(missing, NO_BROWSER_REMEDY);

    let dir = scratch("slipfork-browser-fail-");
    let failing = dir.path().join("failing.sh");
    fs::write(&failing, "#!/bin/sh\nexit 3\n").expect("write the failing opener");
    let mut permissions = fs::metadata(&failing).expect("stat").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&failing, permissions).expect("chmod");
    let error = Browser::new(failing, true)
        .open(&page)
        .expect_err("a failed launcher never reports success");
    assert!(error.contains(NO_BROWSER_REMEDY), "{error}");
    assert!(
        !error.contains("#handle="),
        "the live handle leaked: {error}"
    );
    assert!(!error.contains(&page), "the live URL leaked: {error}");
    assert_eq!(error.lines().count(), 1, "one actionable line: {error}");
}

#[test]
fn a_missing_or_non_executable_opener_is_never_selected() {
    let empty = scratch("slipfork-browser-empty-");
    let joined = std::env::join_paths([empty.path()]).expect("join an empty PATH");
    let browser = Browser::resolve(Some(joined.as_os_str()), true);
    let error = browser
        .open("http://demo.localhost")
        .expect_err("an empty PATH has no opener");
    assert_eq!(error, NO_BROWSER_REMEDY);

    let dir = scratch("slipfork-browser-noexec-");
    let inert = dir.path().join("xdg-open");
    fs::write(&inert, "#!/bin/sh\nexit 0\n").expect("write the inert opener");
    let mut permissions = fs::metadata(&inert).expect("stat").permissions();
    permissions.set_mode(0o644);
    fs::set_permissions(&inert, permissions).expect("chmod 0644");
    let joined = std::env::join_paths([dir.path()]).expect("join the inert PATH");
    let browser = Browser::resolve(Some(joined.as_os_str()), true);
    assert_eq!(
        browser
            .open("http://demo.localhost")
            .expect_err("a non-executable file is not an opener"),
        NO_BROWSER_REMEDY
    );
}

#[test]
fn a_host_without_a_desktop_session_shows_the_approval_line_instead_of_spawning() {
    assert_eq!(
        APPROVAL_HINT_PREFIX,
        "open this URL in a browser on any machine to approve: "
    );
    let dir = scratch("slipfork-browser-headless-");
    let (script, record) = recording_opener(dir.path());

    let launched = Browser::new(fs::canonicalize(&script).expect("absolute opener"), false)
        .open("http://demo.localhost")
        .expect("a headless host is not an error");
    assert!(!launched, "a headless host never spawns a launcher");
    assert!(
        !record.exists(),
        "the opener is never run on a host without a desktop session"
    );
}
