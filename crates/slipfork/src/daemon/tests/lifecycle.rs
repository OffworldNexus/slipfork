use super::engine::{ReloadFault, disk_scratch, restart_snow, test_service};
use slipfork_edge::WorldRegistry;
use slipfork_wire::control::UpRequest;
use slipfork_wire::control::WorldSelector;
use slipfork_wire::control::{DaemonError, DaemonErrorKind, WorldState};
use slipfork_world::WorldEngine;
use slipfork_world::{ProjectSlug, ServiceState, WorldTarget};
use std::fs;
use std::sync::atomic::Ordering;

#[test]
pub(super) fn restart_applies_an_edited_snow_file_and_a_second_up_touches_nothing() {
    let scratch = disk_scratch("restart");
    let worktree = scratch.join("worktree");
    fs::create_dir_all(&worktree).expect("worktree");
    fs::write(
        worktree.join("snow.yml"),
        restart_snow(7000, "echo one", "one"),
    )
    .expect("snow.yml");
    let (service, registry, engine) = test_service(&scratch, &worktree);

    let request = UpRequest {
        cwd: worktree.clone(),
        world: None,
        dump: slipfork_wire::control::DumpRequest::Default,
        resources: slipfork_wire::control::ResourceRequest::Default,
        host_caches: false,
    };
    let up = service.up(&request, &mut |_| {}).expect("boot the world");
    assert_eq!(up.world, "acme");
    assert_eq!(up.state, WorldState::Running);
    assert!(up.changed, "a fresh boot created the world");
    assert_eq!(
        up.restarted,
        vec!["api".to_owned()],
        "the boot started `api`, read back from the engine's rows"
    );
    assert_eq!(
        engine.counts(),
        (1, 0, 1),
        "one spawn and one pre-deploy hook, no world stop"
    );
    assert_eq!(route_port(&registry, "acme.localhost"), 7000);

    // A second `up` on the running world reports no change and performs no engine work.
    let again = service.up(&request, &mut |_| {}).expect("idempotent up");
    assert!(!again.changed);
    assert!(again.restarted.is_empty());
    assert_eq!(
        engine.counts(),
        (1, 0, 1),
        "the idempotent `up` must issue no spawn, stop, or hook call"
    );

    // The edit a `restart` must apply: a new ingress port, a new hook body, a new env value.
    fs::write(
        worktree.join("snow.yml"),
        restart_snow(7100, "echo two", "two"),
    )
    .expect("edited snow.yml");
    let restarted = service
        .restart(
            &WorldSelector {
                cwd: worktree.clone(),
                world: None,
            },
            &mut |_| {},
        )
        .expect("restart");
    assert_eq!(restarted.state, WorldState::Running);
    assert!(
        restarted.changed,
        "`api` came back and the hook ran: the restart changed the world"
    );
    let running: Vec<String> = restarted
        .services
        .iter()
        .filter(|row| row.info.state == ServiceState::Running)
        .map(|row| row.info.id.clone())
        .collect();
    assert_eq!(
        restarted.restarted, running,
        "`restarted` is exactly the engine's post-reboot `Running` set"
    );
    assert_eq!(restarted.restarted, vec!["api".to_owned()]);
    assert_eq!(
        route_port(&registry, "acme.localhost"),
        7100,
        "restart must re-attach the edited `ingresses:` table"
    );
    assert_eq!(
        engine.counts(),
        (2, 1, 2),
        "one world stop, one respawn, and the hook ran again"
    );
    assert_eq!(
        engine.hooks.lock().expect("hook records")[1].last(),
        Some(&"echo two".to_owned()),
        "the edited hook body is what ran on restart"
    );
    assert_eq!(
        engine.greetings.lock().expect("spawn envs")[1],
        Some("two".to_owned()),
        "the edited env value reached the respawned component"
    );
    assert_eq!(
        engine.destroyed.load(Ordering::SeqCst),
        0,
        "restart never destroys the world handle (the CoW state survives)"
    );
    assert_eq!(
        engine.spawns.load(Ordering::SeqCst),
        2,
        "the daemon never stopped `api` through its own handle"
    );

    service
        .down(&WorldSelector {
            cwd: worktree,
            world: None,
        })
        .expect("down");
    let _ = fs::remove_dir_all(&scratch);
}

/// A reload the engine refuses at its pre-flight gate leaves the world exactly as it was: the
/// daemon record stays `Running` with its services bound, and `ps` still reads them.
#[test]
pub(super) fn a_refused_reload_keeps_the_running_world_and_its_service_rows() {
    let scratch = disk_scratch("reload-refused");
    let worktree = scratch.join("worktree");
    fs::create_dir_all(&worktree).expect("worktree");
    fs::write(
        worktree.join("snow.yml"),
        restart_snow(7000, "echo one", "one"),
    )
    .expect("snow.yml");
    let (service, _registry, engine) = test_service(&scratch, &worktree);
    let selector = WorldSelector {
        cwd: worktree.clone(),
        world: None,
    };
    service
        .up(
            &UpRequest {
                cwd: worktree.clone(),
                world: None,
                dump: slipfork_wire::control::DumpRequest::Default,
                resources: slipfork_wire::control::ResourceRequest::Default,
                host_caches: false,
            },
            &mut |_| {},
        )
        .expect("boot the world");

    // The engine's pre-flight refusal (a pruned base, a malformed spec, a sealing world) stops
    // nothing: the world is still live behind the same handle.
    engine.fail_next_reload(ReloadFault::Refused);
    let refused = service
        .restart(&selector, &mut |_| {})
        .expect_err("the engine refused the reload");
    assert!(
        refused.to_string().contains("cannot re-run world 'acme'"),
        "the refusal reaches the user verbatim: {refused}"
    );
    assert_eq!(
        service.list(None).expect("list")[0].state,
        WorldState::Running,
        "a refusal that mutated nothing must never fail a healthy world"
    );
    let processes = service
        .world_processes(&selector)
        .expect("`ps` reads a world this refusal never touched");
    assert_eq!(processes.state, WorldState::Running);
    assert_eq!(
        processes
            .services
            .iter()
            .map(|row| (row.info.id.as_str(), row.info.state))
            .collect::<Vec<_>>(),
        vec![("api", ServiceState::Running)],
        "the refused reload left the component's own row running"
    );

    let _ = fs::remove_dir_all(&scratch);
}

/// A reload that dies *past* the gate stopped the services and tore the world down: the record
/// is `Failed`, and `ps` names the failed boot and its retry instead of a lost handle.
#[test]
pub(super) fn a_reload_failure_past_the_gate_fails_the_world_and_ps_names_the_retry() {
    let scratch = disk_scratch("reload-torndown");
    let worktree = scratch.join("worktree");
    fs::create_dir_all(&worktree).expect("worktree");
    fs::write(
        worktree.join("snow.yml"),
        restart_snow(7000, "echo one", "one"),
    )
    .expect("snow.yml");
    let (service, _registry, engine) = test_service(&scratch, &worktree);
    let selector = WorldSelector {
        cwd: worktree.clone(),
        world: None,
    };
    service
        .up(
            &UpRequest {
                cwd: worktree.clone(),
                world: None,
                dump: slipfork_wire::control::DumpRequest::Default,
                resources: slipfork_wire::control::ResourceRequest::Default,
                host_caches: false,
            },
            &mut |_| {},
        )
        .expect("boot the world");

    engine.fail_next_reload(ReloadFault::AfterTheGate);
    let failed = service
        .restart(&selector, &mut |_| {})
        .expect_err("the reload died after its gate");
    assert!(
        failed.to_string().contains("cannot re-run world 'acme'"),
        "the boot failure reaches the user: {failed}"
    );
    assert_eq!(
        service
            .world_facts(
                &ProjectSlug::parse("acme").expect("project slug"),
                &WorldTarget::Canonical,
            )
            .expect("the failed world stays in the index")
            .state,
        WorldState::Failed,
        "a reload that stopped every service leaves a failed world record"
    );
    let err = service
        .world_processes(&selector)
        .expect_err("a world with no live PID 1 has no rows to read");
    let error = DaemonError::from(&err);
    assert_eq!(error.kind, DaemonErrorKind::Internal);
    assert!(
        error
            .message
            .contains("last boot failed; run `slipfork up` to retry"),
        "`ps` names the failed boot and the retry: {}",
        error.message
    );
    assert!(
        !error.message.contains("engine handle is gone"),
        "the daemon still holds this handle: {}",
        error.message
    );

    let _ = fs::remove_dir_all(&scratch);
}

/// A reload that dies after it reaped the world PID 1 but before the engine stamps `Failed`
/// leaves a record that still reads `Live` — the exact shape the old lineage-as-liveness check
/// mistook for a healthy world. The daemon must mark it `Failed`, never `Running`.
#[test]
pub(super) fn a_reload_failure_that_already_reaped_the_world_init_fails_the_world() {
    let scratch = disk_scratch("reload-init-gone");
    let worktree = scratch.join("worktree");
    fs::create_dir_all(&worktree).expect("worktree");
    fs::write(
        worktree.join("snow.yml"),
        restart_snow(7000, "echo one", "one"),
    )
    .expect("snow.yml");
    let (service, _registry, engine) = test_service(&scratch, &worktree);
    let selector = WorldSelector {
        cwd: worktree.clone(),
        world: None,
    };
    service
        .up(
            &UpRequest {
                cwd: worktree.clone(),
                world: None,
                dump: slipfork_wire::control::DumpRequest::Default,
                resources: slipfork_wire::control::ResourceRequest::Default,
                host_caches: false,
            },
            &mut |_| {},
        )
        .expect("boot the world");

    engine.fail_next_reload(ReloadFault::TeardownBeforeStamp);
    let failed = service
        .restart(&selector, &mut |_| {})
        .expect_err("the reload died while tearing the world down");
    assert!(
        failed.to_string().contains("cannot re-run world 'acme'"),
        "the teardown failure reaches the user: {failed}"
    );

    let facts = service
        .world_facts(
            &ProjectSlug::parse("acme").expect("project slug"),
            &WorldTarget::Canonical,
        )
        .expect("the failed world stays in the index");
    assert_eq!(
        facts.state,
        WorldState::Failed,
        "a reload whose world PID 1 is gone must never leave the world `Running`"
    );
    // The bug's precondition, pinned: the engine still answers a record read while the world is
    // dead, so only `world_is_live` may decide liveness.
    assert!(
        engine.world_lineage(&facts.handle).is_ok(),
        "this shape must still read as a record, or it would not reproduce the bug"
    );
    assert!(
        !engine
            .world_is_live(&facts.handle)
            .expect("the engine answers its own liveness"),
        "the reload reaped the world PID 1"
    );

    let _ = fs::remove_dir_all(&scratch);
}

/// The port the registry serves for one `*.localhost` host at `/`.
pub(super) fn route_port(registry: &WorldRegistry, host: &str) -> u16 {
    registry.resolve(host, "/").expect("attached route").port()
}
