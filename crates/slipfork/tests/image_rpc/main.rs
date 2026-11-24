//! One real daemon over a socket: typed builds, refusals, broken pipe, and a moving tag.
mod build;
mod common;
mod pull;
#[path = "../support/image_rpc_registry.rs"]
mod registry;
#[test]
fn the_daemon_owns_typed_image_builds_and_pulls() {
    let mut lab = common::Lab::new();
    lab.write_project("scratch");
    lab.spawn_daemon();
    build::cold_then_warm(&lab);
    build::refusals(&lab);
    build::broken_pipe(&lab);
    pull::fresh_tag(&lab);
}
