use super::super::backing::PostgresReady;
use super::super::backing::{await_postgres_ready, readiness_detail};
use super::super::boot::{
    BootCommand, BootVerb, boot_verb, check_rerun_policy, not_running, rerun_policy,
    restarted_service_ids,
};
use super::super::env::{parse_hook_id, validate_secret_key};
use super::super::protocol::tail_lines;
use super::super::types::BootPolicy;
use slipfork_dag::DumpPolicy;
use slipfork_edge::EdgeError;
use slipfork_snow::HookPhase;
use slipfork_wire::control::{DaemonErrorKind, WorldServiceSummary, WorldState};
use slipfork_world::StreamChunk;
use slipfork_world::{ProjectSlug, ServiceKind, WorldTarget};
use std::time::Duration;

#[test]
pub(super) fn up_twice_on_a_running_world_confirms_without_touching_it() {
    assert_eq!(
        boot_verb(BootCommand::Up, Some(WorldState::Running)),
        Some(BootVerb::Confirm),
        "an `up` of a running world reports `changed: false` and performs no work"
    );
}

#[test]
pub(super) fn up_boots_an_absent_world_and_rescues_a_failed_one_in_place() {
    assert_eq!(boot_verb(BootCommand::Up, None), Some(BootVerb::Boot));
    assert_eq!(
        boot_verb(BootCommand::Up, Some(WorldState::Failed)),
        Some(BootVerb::Rerun),
        "a failed world is rescued through the same in-place pipeline as `restart`"
    );
    assert_eq!(
        boot_verb(BootCommand::Up, Some(WorldState::Failed)),
        boot_verb(BootCommand::Restart, Some(WorldState::Failed)),
    );
}

#[test]
pub(super) fn restart_rescues_a_failed_world_and_reruns_a_running_one() {
    assert_eq!(
        boot_verb(BootCommand::Restart, Some(WorldState::Failed)),
        Some(BootVerb::Rerun)
    );
    assert_eq!(
        boot_verb(BootCommand::Restart, Some(WorldState::Running)),
        Some(BootVerb::Rerun)
    );
    assert_eq!(
        boot_verb(BootCommand::Restart, Some(WorldState::Stopped)),
        None,
        "a stopped world has nothing to stop: `restart` refuses it (§4)"
    );
}

#[test]
pub(super) fn a_rerun_refuses_a_policy_only_a_fresh_boot_can_produce() {
    let held = BootPolicy {
        dump: DumpPolicy::Default,
        force_local_resources: false,
        host_caches: false,
    };
    assert!(check_rerun_policy("acme", held, held).is_ok());
    assert!(
        check_rerun_policy(
            "acme",
            held,
            BootPolicy {
                force_local_resources: true,
                ..held
            }
        )
        .is_ok(),
        "`--resources local` re-resolves in place"
    );

    // A bare `up` of a held world keeps the policy it booted with.
    let booted = BootPolicy {
        dump: DumpPolicy::NoDump,
        force_local_resources: true,
        host_caches: true,
    };
    let bare = rerun_policy(
        booted,
        slipfork_wire::control::DumpRequest::Default,
        slipfork_wire::control::ResourceRequest::Default,
        false,
    );
    assert_eq!(bare, booted);
    assert!(check_rerun_policy("acme", booted, bare).is_ok());

    // An explicit flag a rerun cannot apply is carried through and refused, never dropped.
    let explicit = rerun_policy(
        held,
        slipfork_wire::control::DumpRequest::NoDump,
        slipfork_wire::control::ResourceRequest::Local,
        true,
    );
    assert_eq!(explicit.dump, DumpPolicy::NoDump);
    assert!(explicit.force_local_resources);
    for (policy, needle) in [
        (
            BootPolicy {
                dump: DumpPolicy::RefreshDump,
                ..held
            },
            "dump policy re-seals the base",
        ),
        (
            BootPolicy {
                host_caches: true,
                ..held
            },
            "--host-caches` rebuilds it",
        ),
    ] {
        let error = check_rerun_policy("acme", held, policy)
            .expect_err("a fresh-boot-only fact is refused in place");
        let message = error.to_string();
        assert!(message.contains(needle), "got: {message}");
        assert!(
            message.contains("run `slipfork down --world acme` first"),
            "the refusal must name the next command, got: {message}"
        );
    }
}

#[test]
pub(super) fn restart_on_a_never_booted_world_fails_closed_naming_up() {
    assert_eq!(
        boot_verb(BootCommand::Restart, None),
        None,
        "`restart` never boots a world the caller believed was running"
    );
    let project = ProjectSlug::parse("acme").expect("valid project slug");
    match not_running(&project, &WorldTarget::Canonical) {
        EdgeError::Control(error) => {
            assert_eq!(error.kind, DaemonErrorKind::NotRunning);
            assert_eq!(
                error.message,
                "world 'acme' is not running; boot it with `slipfork up`"
            );
        }
        other => panic!("expected a typed `not_running` refusal, got {other:?}"),
    }
}

#[test]
pub(super) fn restarted_ids_are_the_engine_rows_this_run_brought_to_running() {
    pub(super) fn row(
        world: &str,
        id: &str,
        state: slipfork_world::ServiceState,
    ) -> WorldServiceSummary {
        WorldServiceSummary {
            world: world.to_owned(),
            info: slipfork_world::WorldServiceInfo {
                id: id.to_owned(),
                kind: ServiceKind::Component,
                state,
                pid: None,
                port: None,
                uptime_secs: None,
            },
        }
    }
    use slipfork_world::ServiceState;

    let stopped = vec![
        row("acme", "pg_databases.main", ServiceState::Stopped),
        row("acme", "api", ServiceState::Stopped),
    ];
    let running = vec![
        row("acme", "pg_databases.main", ServiceState::Running),
        row("acme", "api", ServiceState::Running),
    ];

    // After the stop every row is `Stopped`; what is `Running` afterwards is what restarted.
    assert_eq!(
        restarted_service_ids(&stopped, &running),
        vec!["pg_databases.main", "api"]
    );
    // An idempotent run changes nothing.
    assert!(restarted_service_ids(&running, &running).is_empty());
    // An `exited` row is never named beside `restarted`.
    let mut half = running.clone();
    half[1].info.state = ServiceState::Exited;
    assert_eq!(
        restarted_service_ids(&stopped, &half),
        vec!["pg_databases.main"]
    );
    // A service that had crashed before the run and came back counts as restarted.
    let crashed = vec![
        row("acme", "pg_databases.main", ServiceState::Exited),
        row("acme", "api", ServiceState::Stopped),
    ];
    assert_eq!(
        restarted_service_ids(&crashed, &running),
        vec!["pg_databases.main", "api"]
    );
    // Duplicate rows never duplicate an id.
    let mut doubled = running.clone();
    doubled.push(row("acme", "api", ServiceState::Running));
    assert_eq!(
        restarted_service_ids(&stopped, &doubled),
        vec!["pg_databases.main", "api"]
    );
}

#[test]
pub(super) fn hook_ids_round_trip_every_phase() {
    assert_eq!(
        parse_hook_id("api:pre-deploy"),
        Some(("api", HookPhase::PreDeploy))
    );
    assert_eq!(
        parse_hook_id("api:post-deploy"),
        Some(("api", HookPhase::PostDeploy))
    );
    assert_eq!(
        parse_hook_id("api:failed-deploy"),
        Some(("api", HookPhase::FailedDeploy))
    );
    assert_eq!(parse_hook_id("api"), None);
    assert_eq!(parse_hook_id("api:unknown"), None);
    assert_eq!(parse_hook_id(":pre-deploy"), None);
}

#[test]
pub(super) fn secret_keys_reject_separators_and_control_characters() {
    assert!(validate_secret_key("STRIPE_SECRET_KEY").is_ok());
    assert!(validate_secret_key("").is_err());
    assert!(validate_secret_key("A=B").is_err());
    assert!(validate_secret_key("A\nB").is_err());
}

#[test]
pub(super) fn postgres_readiness_gate_returns_on_the_first_accepted_probe() {
    let policy = PostgresReady {
        deadline: Duration::from_secs(1),
        poll: Duration::from_millis(1),
    };
    await_postgres_ready(&policy, || Ok((0, String::new())))
        .expect("a postmaster that answers is ready");
}

#[test]
pub(super) fn postgres_readiness_gate_fails_closed_at_the_deadline() {
    let policy = PostgresReady {
        deadline: Duration::ZERO,
        poll: Duration::ZERO,
    };
    let mut probes = 0;
    let error = await_postgres_ready(&policy, || {
        probes += 1;
        Ok((2, "FATAL:  the database system is starting up".to_owned()))
    })
    .expect_err("a postmaster that never accepts must fail closed");
    assert_eq!(probes, 1, "a zero deadline fails on the first rejection");
    let message = error.to_string();
    assert!(message.contains("within 0s"), "got: {message}");
    assert!(message.contains("starting up"), "got: {message}");
}

#[test]
pub(super) fn postgres_readiness_gate_polls_until_the_real_deadline() {
    let policy = PostgresReady {
        deadline: Duration::from_millis(120),
        poll: Duration::from_millis(10),
    };
    let mut probes = 0;
    let started = std::time::Instant::now();
    let error = await_postgres_ready(&policy, || {
        probes += 1;
        Ok((2, "FATAL:  the database system is starting up".to_owned()))
    })
    .expect_err("a postmaster that never accepts must fail closed");
    let elapsed = started.elapsed();
    assert!(
        probes >= 2,
        "the gate polls again after each rejection (probes: {probes})"
    );
    assert!(
        elapsed >= Duration::from_millis(120) && elapsed < Duration::from_millis(500),
        "the gate waits the policy deadline in real time (elapsed {elapsed:?}): {error}"
    );
    assert!(
        error.to_string().contains("starting up"),
        "the last server diagnostic survives the deadline: {error}"
    );
}

#[test]
pub(super) fn readiness_detail_prefers_stderr_and_falls_back_to_stdout() {
    assert_eq!(
        readiness_detail(
            "127.0.0.1:5432 - rejecting connections\n",
            "FATAL:  the database system is starting up\n",
        ),
        "FATAL:  the database system is starting up"
    );
    assert_eq!(
        readiness_detail("127.0.0.1:5432 - rejecting connections\n", ""),
        "127.0.0.1:5432 - rejecting connections"
    );
    assert_eq!(readiness_detail("", "  \n"), "");
}

#[test]
pub(super) fn tail_lines_keeps_the_requested_tail() {
    let chunk = StreamChunk {
        source: slipfork_world::StreamSource::Stdout,
        bytes: b"one\ntwo\nthree\n".to_vec(),
    };
    let out = tail_lines(vec![chunk], Some(2));
    assert_eq!(out.len(), 1);
    assert_eq!(String::from_utf8_lossy(&out[0].bytes), "two\nthree\n");
}
