//! The bare `slipfork` / `slipfork status` fleet tree (`docs/design/dx-cli-0.1.0.md` §1, §3).
//!
//! Every function here is pure over the one [`DaemonControl`](slipfork_edge::DaemonControl)
//! `status` payload: the CLI resolves the socket, performs exactly one read, and hands the typed
//! [`DaemonStatus`] to these renderers. Nothing here inspects the host's process table, sysfs tree,
//! environment, or log directory, and nothing here can start a daemon — `status` is a read verb, so
//! a bare `slipfork` never leaves a mystery `--listen 127.0.0.1:0` process behind.
//!
//! The tree lives in the private `table` module and the compact lineage line in `lineage`; both
//! are shared with `ps` and the `up`/`fork`/`restart` block so there is exactly one renderer per
//! fact (`docs/design/dx-cli-0.1.0.md` §1: no `slipforkd <pid>` header, no lineage hash outside
//! `-v`, no `next` spam).

use slipfork_edge::INGRESS_PORT;
use slipfork_wire::control::{DaemonStatus, WorldState, WorldSummary};

use crate::ui::TerminalUi;

mod lineage;
mod lineage_table;
mod table;

pub(crate) use lineage::render_lineage;
pub(crate) use lineage_table::render as render_lineage_table;

/// The `next` commands of one world state (`docs/design/dx-cli-0.1.0.md` §1 is the only table).
///
/// `up`/`fork`/`restart` embed this table in their success block; the bare tree deliberately does
/// not print it.
#[must_use]
pub fn world_next_commands(state: WorldState) -> Vec<String> {
    let verbs: &[&str] = match state {
        WorldState::Running => &["open", "db", "logs", "restart", "down"],
        WorldState::Failed => &["logs", "restart", "down"],
        WorldState::Stopped => &["up", "logs", "down"],
    };
    verbs
        .iter()
        .map(|verb| format!("slipfork {verb}"))
        .collect()
}

/// The one lifecycle label rendered by `status`, `ps`, and the boot block.
pub fn world_state_label(state: WorldState) -> &'static str {
    match state {
        WorldState::Running => "running",
        WorldState::Failed => "failed",
        WorldState::Stopped => "stopped",
    }
}

/// The human fleet tree: one `World -> Services -> Forks` block per running world.
///
/// An empty daemon renders `○ No worlds running` so a read-only bare `slipfork` exits `0` without
/// starting anything.
pub fn render(status: &DaemonStatus, ui: &TerminalUi) -> String {
    render_worlds(&status.worlds, ui)
}

/// The same tree over an already-read world list (`ps` renders its one cwd-scoped world here).
pub fn render_worlds(worlds: &[WorldSummary], ui: &TerminalUi) -> String {
    table::render(worlds, ui, ui.verbose())
}

/// The `--json` document of `slipfork` / `slipfork status` (`docs/design/dx-cli-0.1.0.md` §3).
///
/// Exactly `ok`, `ingress_port`, and `worlds`; every world carries its own `services` array, so a
/// consumer (`@slipfork/opencode`) never issues a second call. Unknown facts are `null`, never `0`.
pub fn json(status: &DaemonStatus) -> serde_json::Value {
    serde_json::json!({
        "ok": true,
        "ingress_port": status.ingress.port(),
        "worlds": status.worlds.iter().map(world_json).collect::<Vec<_>>(),
    })
}

/// The `--json` document of a bare `slipfork` when no daemon answers.
///
/// `ingress_port` is `null` because no daemon was read: the empty fleet is the truth, never a
/// guessed `:80`.
#[must_use]
pub fn json_empty() -> serde_json::Value {
    serde_json::json!({
        "ok": true,
        "ingress_port": Option::<u16>::None,
        "worlds": Vec::<WorldSummary>::new(),
    })
}

/// One world's §3 object, shared by `status` and the `ps` document.
pub(crate) fn world_json(world: &WorldSummary) -> serde_json::Value {
    serde_json::json!({
        "world": &world.world,
        "parent": &world.parent,
        "state": world.state,
        "origin": world.origin.url(),
        "worktree": &world.worktree,
        "lineage": &world.lineage,
        "services": &world.services,
    })
}

/// The one `:80` line `status` prints to stderr, or `None` when the ingress already is `:80`.
///
/// Rootless `slipforkd --listen 127.0.0.1:0` mints port-qualified origins, which is a real port and
/// never a mystery — but it is not the port-less experience, so the dashboard names the one command
/// that fixes it. The caller writes it to stderr only, never under `--json`.
pub fn ingress_hint(port: u16) -> Option<String> {
    (port != INGRESS_PORT).then(|| {
        format!(
            "ingress is :{port}, not :{INGRESS_PORT} — run `sudo slipfork daemon install` \
             once for port-less origins"
        )
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use slipfork_wire::control::{DaemonStatus, IngressInfo, WorldServiceSummary};
    use slipfork_world::{
        CacheOutcome, LocalHostRoute, ProjectSlug, RootfsLineage, SeedLineage, ServiceKind,
        WorldLineage, WorldOrigin, WorldServiceInfo, WorldTarget,
    };

    use super::*;
    use crate::ui::Capabilities;

    fn terminal(verbose: bool) -> TerminalUi {
        TerminalUi::new(Capabilities {
            tty: false,
            color: false,
            verbose,
            json: false,
        })
    }

    fn plain() -> TerminalUi {
        terminal(false)
    }

    fn origin(project: &str, fork: Option<&str>, ingress_port: u16) -> WorldOrigin {
        let project = ProjectSlug::parse(project).expect("a valid project slug");
        let target = match fork {
            Some(fork) => WorldTarget::UserFork(fork.to_owned()),
            None => WorldTarget::Canonical,
        };
        let route = LocalHostRoute::new(project, target, None, &[]).expect("a routable route");
        WorldOrigin::local(&route, ingress_port).expect("a minted origin")
    }

    fn lineage(rootfs: Option<&str>) -> WorldLineage {
        WorldLineage {
            rootfs: rootfs.map(|digest| RootfsLineage {
                digest: digest.to_owned(),
                cache: CacheOutcome::Hit,
                millis: None,
            }),
            services: Vec::new(),
            seed: Some(SeedLineage {
                digest: None,
                rebuilt: Some(false),
                millis: None,
            }),
            base: None,
        }
    }

    fn service(world: &str, id: &str, port: u16) -> WorldServiceSummary {
        WorldServiceSummary {
            world: world.to_owned(),
            info: WorldServiceInfo {
                id: id.to_owned(),
                kind: ServiceKind::Component,
                state: slipfork_world::ServiceState::Running,
                pid: None,
                port: Some(port),
                uptime_secs: Some(12),
            },
        }
    }

    fn world(
        id: &str,
        parent: Option<&str>,
        state: WorldState,
        ingress_port: u16,
        services: Vec<WorldServiceSummary>,
        lineage: WorldLineage,
    ) -> WorldSummary {
        let (project, fork) = match parent {
            Some(project) => (project.to_owned(), id.split_once(':').map(|(_, fork)| fork)),
            None => (id.to_owned(), None),
        };
        WorldSummary {
            world: id.to_owned(),
            parent: parent.map(str::to_owned),
            state,
            origin: origin(&project, fork, ingress_port),
            worktree: PathBuf::from(format!("/home/dev/{id}")),
            lineage,
            services,
        }
    }

    fn status(ingress_port: u16, worlds: Vec<WorldSummary>) -> DaemonStatus {
        DaemonStatus {
            protocol: slipfork_wire::control::CONTROL_VIEW_VERSION,
            pid: 1420,
            ingress: IngressInfo::new(vec![
                format!("127.0.0.1:{ingress_port}").parse().expect("addr"),
            ])
            .expect("ingress"),
            forward_port: None,
            worlds,
        }
    }

    fn digest(prefix: &str) -> String {
        format!("{prefix}{}", "0".repeat(64 - prefix.len()))
    }

    #[test]
    fn status_renders_one_tree_with_services_and_forks_and_no_daemon_header() {
        let dashboard = render(
            &status(
                80,
                vec![
                    world(
                        "demo",
                        None,
                        WorldState::Running,
                        80,
                        vec![service("demo", "pg", 5432), service("demo", "api", 8000)],
                        lineage(None),
                    ),
                    world(
                        "demo:feat-ux",
                        Some("demo"),
                        WorldState::Running,
                        80,
                        Vec::new(),
                        lineage(None),
                    ),
                ],
            ),
            &plain(),
        );
        let lines: Vec<&str> = dashboard.lines().collect();
        assert_eq!(lines[0], "● demo  http://demo.localhost  /home/dev/demo");
        assert_eq!(lines[1], "├─ pg  running  :5432  12s");
        assert_eq!(lines[2], "├─ api running  :8000  12s");
        assert_eq!(
            lines[3],
            "└─ feat-ux  ● http://feat-ux.demo.localhost  /home/dev/demo:feat-ux"
        );
        assert_eq!(lines.len(), 4, "no lineage/next sub-lines: {dashboard}");
        for retired in ["slipforkd", "pid", "lineage   ", "next      "] {
            assert!(
                !dashboard.contains(retired),
                "the tree never prints {retired}: {dashboard}"
            );
        }
    }

    #[test]
    fn status_renders_an_empty_daemon_as_one_exit_zero_line() {
        let dashboard = render(&status(80, Vec::new()), &plain());
        assert_eq!(dashboard, "○ No worlds running\n");
    }

    #[test]
    fn only_verbose_renders_lineage_digests() {
        let all_known = || lineage(Some(&digest("a1b2c3d4e5f6")));
        let fleet = || {
            status(
                80,
                vec![world(
                    "demo",
                    None,
                    WorldState::Running,
                    80,
                    Vec::new(),
                    all_known(),
                )],
            )
        };
        assert!(!render(&fleet(), &plain()).contains("a1b2c3d4e5f6"));
        let verbose = render(&fleet(), &terminal(true));
        assert!(
            verbose.contains(&format!("{DIGEST_PREFIX}a1b2c3d4e5f6")),
            "the 12-char digest is the -v fact: {verbose}"
        );

        const DIGEST_PREFIX: &str = "lineage rootfs ";
        assert!(
            !plain().verbose(),
            "the default capability snapshot is not verbose"
        );
    }

    #[test]
    fn status_omits_the_default_port_and_keeps_a_real_one() {
        let canonical = |port: u16| {
            render(
                &status(
                    port,
                    vec![world(
                        "demo",
                        None,
                        WorldState::Running,
                        port,
                        Vec::new(),
                        lineage(None),
                    )],
                ),
                &plain(),
            )
        };
        assert!(canonical(80).contains("http://demo.localhost  "));
        assert!(canonical(40645).contains("http://demo.localhost:40645  "));
        assert_eq!(ingress_hint(80), None);
        assert_eq!(
            ingress_hint(40645).as_deref(),
            Some(
                "ingress is :40645, not :80 — run `sudo slipfork daemon install` once for port-less origins"
            )
        );
    }

    #[test]
    fn status_json_matches_the_section_three_contract() {
        let document = json(&status(
            80,
            vec![
                world(
                    "demo",
                    None,
                    WorldState::Running,
                    80,
                    vec![service("demo", "api", 8000)],
                    lineage(Some(&digest("a1"))),
                ),
                world(
                    "demo:feat-ux",
                    Some("demo"),
                    WorldState::Failed,
                    80,
                    Vec::new(),
                    lineage(None),
                ),
            ],
        ));
        assert_eq!(document["ok"], true);
        assert_eq!(document["ingress_port"], 80);
        assert!(
            document.get("pid").is_none() && document.get("ingress").is_none(),
            "only ok/ingress_port/worlds: {document}"
        );
        let worlds = document["worlds"].as_array().expect("worlds array");
        assert_eq!(worlds.len(), 2);
        for key in [
            "world", "parent", "state", "origin", "worktree", "lineage", "services",
        ] {
            assert!(worlds[0].get(key).is_some(), "worlds[0].{key} is present");
        }
        assert!(worlds[0]["parent"].is_null(), "canonical parent is null");
        assert_eq!(worlds[1]["parent"], "demo");
        assert_eq!(worlds[0]["state"], "running");
        assert_eq!(worlds[1]["state"], "failed");
        assert_eq!(worlds[0]["origin"], "http://demo.localhost");
        assert_eq!(worlds[0]["worktree"], "/home/dev/demo");
        let services = worlds[0]["services"].as_array().expect("services array");
        assert_eq!(services.len(), 1);
        assert_eq!(services[0]["id"], "api");
        assert_eq!(services[0]["port"], 8000);
        assert_eq!(services[0]["pid"], serde_json::Value::Null);

        let text = serde_json::to_string(&document).expect("json");
        for human in ["slipforkd", "lineage   ", "next      ", "No worlds"] {
            assert!(!text.contains(human), "no human text in --json: {human}");
        }
    }

    #[test]
    fn world_next_commands_follow_the_one_state_table() {
        assert_eq!(
            world_next_commands(WorldState::Running),
            vec![
                "slipfork open".to_owned(),
                "slipfork db".to_owned(),
                "slipfork logs".to_owned(),
                "slipfork restart".to_owned(),
                "slipfork down".to_owned(),
            ]
        );
        assert_eq!(world_state_label(WorldState::Stopped), "stopped");
    }
}
