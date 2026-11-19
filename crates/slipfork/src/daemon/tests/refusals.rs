//! Manifest refusals the daemon prints as one line plus a `next:` remedy.
//!
//! Every case loads a real `Forkfile` from a `/var/tmp` scratch directory and stops at the pure
//! loader: no daemon boot, no Btrfs, no process spawn.

use slipfork_edge::EdgeError;
use slipfork_wire::control::{DaemonError, DaemonErrorKind};

use crate::daemon::env::{ingress_routes, resolve_hook};
use crate::daemon::fork::load_project_fork;

/// A service publishing a route with no `port` to serve it.
const ROUTE_WITHOUT_PORT: &str = "\
project demo

features {
    local
}

service api ./api {
    route /api
    run /bin/true
}
";

/// A service with a `port` but no `route`, so the world has no ingress.
const WORLD_WITHOUT_ROUTE: &str = "\
project demo

features {
    local
}

service api ./api {
    port 3000
    run /bin/true
}
";

/// One `/var/tmp` directory holding a `Forkfile` with `source`.
fn scratch(label: &str, source: &str) -> tempfile::TempDir {
    let dir = scratch_dir(label);
    std::fs::write(dir.path().join("Forkfile"), source).expect("write Forkfile");
    dir
}

/// One empty `/var/tmp` directory (a repository with no `Forkfile`).
fn scratch_dir(label: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(label)
        .tempdir_in("/var/tmp")
        .expect("disk scratch")
}

/// Assert one typed refusal and return it (every refusal is a `next:`-carrying rejection).
fn refused<T>(result: Result<T, EdgeError>) -> DaemonError {
    let error = match result {
        Ok(_) => panic!("expected a manifest refusal"),
        Err(EdgeError::Control(error)) => error,
        Err(other) => panic!("a refusal must be a typed control error, got {other:?}"),
    };
    assert_eq!(error.kind, DaemonErrorKind::Rejected, "got {error:?}");
    assert!(
        error.message.contains("next: "),
        "every refusal carries the remedy: {}",
        error.message
    );
    error
}

#[test]
fn a_missing_forkfile_is_a_rejection_naming_the_fix() {
    let dir = scratch_dir("slipfork-missing");
    let error = refused(load_project_fork(dir.path()));
    assert!(
        error.message.starts_with("cannot read "),
        "{}",
        error.message
    );
    assert!(
        error.message.contains("next: add the project `Forkfile`"),
        "{}",
        error.message
    );
}

#[test]
fn an_unparseable_forkfile_is_a_rejection_with_line_and_remedy() {
    let dir = scratch("slipfork-unparseable", "features local {\n");
    let error = refused(load_project_fork(dir.path()));
    assert!(
        error.message.starts_with('/') && error.message.contains("Forkfile:"),
        "path:line:col comes first: {}",
        error.message
    );
}

#[test]
fn a_route_without_a_port_is_refused() {
    let dir = scratch("slipfork-route", ROUTE_WITHOUT_PORT);
    let fork = load_project_fork(dir.path()).expect("a routeless port still parses");
    let error = refused(ingress_routes(&fork));
    assert!(
        error.message.contains("no `port` to serve it"),
        "{}",
        error.message
    );
    assert!(
        error.message.contains("next: add `port <n>`"),
        "{}",
        error.message
    );
}

#[test]
fn a_world_without_an_ingress_route_is_refused() {
    let dir = scratch("slipfork-ingress", WORLD_WITHOUT_ROUTE);
    let fork = load_project_fork(dir.path()).expect("a valid single-service manifest");
    let error = refused(ingress_routes(&fork));
    assert!(
        error.message.contains("no HTTP ingress"),
        "{}",
        error.message
    );
    assert!(
        error.message.contains("next: add `route /`"),
        "{}",
        error.message
    );
}

#[test]
fn an_unknown_hook_id_is_refused_with_the_declared_list() {
    let error = refused(resolve_hook(&[], "api:unknown"));
    assert!(
        error.message.contains("world has no hook 'api:unknown'"),
        "{}",
        error.message
    );
    assert!(
        error.message.contains("declared hooks: none"),
        "{}",
        error.message
    );
    assert!(
        error
            .message
            .contains("next: name one of the declared hooks"),
        "{}",
        error.message
    );
}
