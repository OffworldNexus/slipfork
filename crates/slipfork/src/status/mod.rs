//! The bare `slipfork` / `slipfork status` dashboard (`docs/design/dx-0.1.0-ux.md` §1, §3, §10).
//!
//! Every function here is pure over the one [`DaemonControl`](slipfork_edge::DaemonControl)
//! `status` payload: the CLI resolves the socket, performs exactly one read, and hands the typed
//! [`DaemonStatus`] to these renderers. Nothing here inspects the host's process table, sysfs tree,
//! environment, or log directory, and nothing here can start a daemon — `status` is a read verb, so
//! a bare `slipfork` never leaves a mystery `--listen 127.0.0.1:0` process behind (§1, `M3.28.1`
//! R5).
//!
//! The table and the lineage line live in [`table`] and [`lineage`] so each file stays one cohesive
//! renderer; both are re-exported from here because `status`, `list`, `ps`, and `restart` share them.

use std::collections::BTreeSet;

use slipfork_edge::INGRESS_PORT;
use slipfork_wire::control::{DaemonStatus, WorldState, WorldSummary};

use crate::cli::implemented_commands;
use crate::ui::TerminalUi;

mod lineage;
mod table;

pub(crate) use lineage::render_lineage;
pub use table::WorldTable;

/// The `next` commands of one world state (`docs/design/dx-0.1.0-ux.md` §1 is the only table).
///
/// Rows are the bare `<verb>` commands of the state; a caller that renders a world other than the
/// cwd-selected one appends its own `--world <WORLD>`, so the daemon never guesses a selector.
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

/// The one lifecycle label rendered by `status`, `list`, and `ps`.
pub fn world_state_label(state: WorldState) -> &'static str {
    match state {
        WorldState::Running => "running",
        WorldState::Failed => "failed",
        WorldState::Stopped => "stopped",
    }
}

/// The human dashboard for one daemon status: the daemon line, then the project/world tree.
///
/// Each world row carries its state, origin, and worktree; its `lineage` and `next` sub-lines carry
/// the world's own build lineage (§8.2) and its [`world_next_commands`] hints (§1's single table),
/// filtered to the verbs this binary actually implements. A daemon that knows no world renders one
/// line plus the command that creates one.
pub fn render(status: &DaemonStatus, ui: &TerminalUi) -> String {
    let mut out = format!(
        "slipforkd {}, ingress {}\n",
        status.pid,
        ingress_addrs(status)
    );
    if status.worlds.is_empty() {
        out.push_str(&format!(
            "  {}\n",
            ui.secondary("no worlds — run `slipfork up` inside a project checkout")
        ));
        return out;
    }
    let table = WorldTable::new(&status.worlds, ui);
    let implemented = implemented_commands();
    for project in project_groups(&status.worlds) {
        for world in project {
            table.push_world(&mut out, world, &implemented);
        }
    }
    out
}

/// The `--json` document of `slipfork` / `slipfork status` (`docs/design/dx-0.1.0-ux.md` §10).
///
/// Every key is always present and an unknown fact is `null`, never `0` and never `""` (§8.2). The
/// process rows belong to `ps --json`, so this document never carries a service table.
pub fn json(status: &DaemonStatus) -> serde_json::Value {
    serde_json::json!({
        "ok": true,
        "pid": status.pid,
        "ingress": {
            "addrs": status.ingress.addrs().iter().map(ToString::to_string).collect::<Vec<_>>(),
            "port": status.ingress.port(),
        },
        "forward_port": status.forward_port,
        "worlds": status.worlds.iter().map(|world| serde_json::json!({
            "world": &world.world,
            "parent": &world.parent,
            "state": world.state,
            "origin": world.origin.url(),
            "worktree": &world.worktree,
            "lineage": &world.lineage,
        })).collect::<Vec<_>>(),
    })
}

/// The one `:80` line `status` prints to stderr, or `None` when the ingress already is `:80`.
///
/// Rootless `slipforkd --listen 127.0.0.1:0` mints port-qualified origins, which is a real port and
/// never a mystery — but it is not the port-less experience, so the dashboard names the one command
/// that fixes it (`docs/design/dx-0.1.0-ux.md` §2 rule 4). The caller writes it to stderr only,
/// never under `--json`.
pub fn ingress_hint(port: u16) -> Option<String> {
    (port != INGRESS_PORT).then(|| {
        format!(
            "slipfork: ingress is :{port}, not :{INGRESS_PORT} — run `sudo slipfork daemon install` \
             once for port-less origins"
        )
    })
}

/// One world's `next` hint from the single membership table (§1), filtered to real verbs.
///
/// A row may only name a verb this binary implements: the table is the design's target surface, so
/// a verb that has not landed yet is dropped rather than printed as an `unrecognized subcommand`.
/// A fork id is never the default selector, so its hint carries the id that pastes back into
/// `--world`; the canonical world is the default inside its own checkout and stays bare.
fn next_hint(world: &WorldSummary, implemented: &BTreeSet<String>) -> String {
    world_next_commands(world.state)
        .into_iter()
        .filter(|command| hint_verb(command).is_some_and(|verb| implemented.contains(verb)))
        .map(|command| match world.parent {
            Some(_) => format!("{command} --world {}", world.world),
            None => command,
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

/// The verb a `world_next_commands` row names (`slipfork open` -> `open`).
fn hint_verb(command: &str) -> Option<&str> {
    let (program, verb) = command.split_once(' ')?;
    (program == "slipfork" && !verb.trim().is_empty() && !verb.contains(' ')).then_some(verb)
}

/// Every canonical world followed by the forks the daemon names as its children.
///
/// Grouping uses `WorldSummary.parent` (never a split of the world id) and preserves the daemon's
/// payload order, so a project row always precedes its own forks. A fork whose parent has no row in
/// this payload is still rendered (indented) rather than silently dropped.
fn project_groups(worlds: &[WorldSummary]) -> Vec<Vec<&WorldSummary>> {
    let mut groups: Vec<(String, Vec<&WorldSummary>)> = Vec::new();
    for world in worlds {
        let project = world.parent.clone().unwrap_or_else(|| world.world.clone());
        match groups.iter_mut().find(|(key, _)| *key == project) {
            Some((_, members)) => members.push(world),
            None => groups.push((project, vec![world])),
        }
    }
    groups
        .into_iter()
        .map(|(_, mut members)| {
            // Stable: the canonical row (`parent: None`) always leads its own forks.
            members.sort_by_key(|world| world.parent.is_some());
            members
        })
        .collect()
}

/// The daemon's bound ingress listeners (`127.0.0.1:80, [::1]:80` under socket activation).
fn ingress_addrs(status: &DaemonStatus) -> String {
    status
        .ingress
        .addrs()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod next_table_tests {
    use slipfork_wire::control::WorldState;

    use super::*;

    #[test]
    fn world_next_commands_follow_the_one_state_table() {
        let verbs = |state| world_next_commands(state);
        assert_eq!(
            verbs(WorldState::Running),
            vec![
                "slipfork open".to_owned(),
                "slipfork db".to_owned(),
                "slipfork logs".to_owned(),
                "slipfork restart".to_owned(),
                "slipfork down".to_owned(),
            ]
        );
        assert_eq!(
            verbs(WorldState::Failed),
            vec![
                "slipfork logs".to_owned(),
                "slipfork restart".to_owned(),
                "slipfork down".to_owned(),
            ]
        );
        assert_eq!(
            verbs(WorldState::Stopped),
            vec![
                "slipfork up".to_owned(),
                "slipfork logs".to_owned(),
                "slipfork down".to_owned(),
            ]
        );
    }

    #[test]
    fn world_state_labels_are_the_lowercase_table() {
        assert_eq!(world_state_label(WorldState::Running), "running");
        assert_eq!(world_state_label(WorldState::Failed), "failed");
        assert_eq!(world_state_label(WorldState::Stopped), "stopped");
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use slipfork_wire::control::{DaemonStatus, IngressInfo, WorldState};
    use slipfork_world::{BaseLineage, RootfsLineage, SeedLineage, WorldLineage};
    use slipfork_world::{CacheOutcome, LocalHostRoute, ProjectSlug, WorldOrigin, WorldTarget};

    use super::*;
    use crate::ui::Capabilities;

    fn plain() -> TerminalUi {
        TerminalUi::new(Capabilities {
            tty: false,
            color: false,
            verbose: false,
            json: false,
        })
    }

    fn render_plain(status: &DaemonStatus) -> String {
        render(status, &plain())
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

    fn lineage(rootfs: Option<&str>, seed: Option<&str>, base: Option<&str>) -> WorldLineage {
        WorldLineage {
            rootfs: rootfs.map(|digest| RootfsLineage {
                digest: digest.to_owned(),
                cache: CacheOutcome::Hit,
                millis: None,
            }),
            services: Vec::new(),
            seed: Some(SeedLineage {
                digest: seed.map(str::to_owned),
                rebuilt: Some(false),
                millis: None,
            }),
            base: base.map(|key| BaseLineage {
                key: key.to_owned(),
                millis: None,
            }),
        }
    }

    fn world(
        id: &str,
        parent: Option<&str>,
        state: WorldState,
        ingress_port: u16,
        lineage: WorldLineage,
    ) -> WorldSummary {
        let (project, fork) = match parent {
            Some(project) => (
                project.to_owned(),
                Some(id.split_once(':').expect("a fork id").1),
            ),
            None => (id.to_owned(), None),
        };
        WorldSummary {
            world: id.to_owned(),
            parent: parent.map(str::to_owned),
            state,
            origin: origin(&project, fork, ingress_port),
            worktree: PathBuf::from(format!("/home/dev/{id}")),
            lineage,
        }
    }

    fn status(ingress_port: u16, worlds: Vec<WorldSummary>) -> DaemonStatus {
        DaemonStatus {
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
    fn status_renders_the_canonical_world_then_its_indented_forks() {
        let all_known = || {
            lineage(
                Some(&digest("a1")),
                Some(&digest("9f")),
                Some(&digest("45")),
            )
        };
        let dashboard = render_plain(&status(
            80,
            vec![
                world("demo", None, WorldState::Running, 80, all_known()),
                world(
                    "demo:feat-api",
                    Some("demo"),
                    WorldState::Stopped,
                    80,
                    all_known(),
                ),
                world(
                    "demo:feat-ux",
                    Some("demo"),
                    WorldState::Running,
                    80,
                    all_known(),
                ),
            ],
        ));
        let lines: Vec<&str> = dashboard.lines().collect();
        assert_eq!(lines[0], "slipforkd 1420, ingress 127.0.0.1:80");

        // The canonical world leads, its forks follow indented, never re-ordered by payload order.
        assert!(
            lines[1].starts_with("  demo ") && lines[1].contains("running"),
            "the canonical row is first: {lines:?}"
        );
        assert!(lines[4].starts_with("    demo:feat-api"));
        assert!(lines[7].starts_with("    demo:feat-ux"));

        // Every world row carries exactly one origin and its worktree.
        assert!(lines[1].contains("http://demo.localhost "));
        assert!(lines[1].contains("/home/dev/demo"));
        assert!(lines[4].contains("http://feat-api.demo.localhost"));
        assert!(lines[4].contains("stopped"));

        // The `next` table is filtered to real verbs; forks carry the id that pastes back into
        // `--world` and the canonical world stays bare.
        assert_eq!(
            lines[2],
            "                  lineage   rootfs a10000000000 cached · seed 9f0000000000 cached · base 450000000000 reused"
        );
        assert_eq!(
            lines[3],
            "                  next      slipfork open | slipfork db | slipfork logs | slipfork restart | slipfork down"
        );
        assert_eq!(
            lines[6],
            "                  next      slipfork up --world demo:feat-api | slipfork logs --world demo:feat-api | slipfork down --world demo:feat-api"
        );
        assert_eq!(dashboard.matches("lineage   ").count(), 3);
        assert_eq!(dashboard.matches("next      ").count(), 3);
    }

    #[test]
    fn status_next_hints_only_name_verbs_the_binary_implements() {
        let implemented = implemented_commands();
        assert!(
            implemented.contains("status") && implemented.contains("list"),
            "the command set is the clap table: {implemented:?}"
        );
        for state in [WorldState::Running, WorldState::Failed, WorldState::Stopped] {
            let dashboard = render_plain(&status(
                80,
                vec![world("demo", None, state, 80, lineage(None, None, None))],
            ));
            let hints = dashboard
                .lines()
                .find_map(|line| line.split_once("next      "))
                .unwrap_or_else(|| panic!("every world carries one next line: {dashboard}"))
                .1;
            let verbs: Vec<&str> = hints.split(" | ").filter_map(hint_verb).collect();
            assert_eq!(
                verbs.len(),
                hints.split(" | ").count(),
                "every hinted row names a `slipfork <verb>`: {hints}"
            );
            assert!(!verbs.is_empty(), "no state renders an empty hint: {hints}");
            for verb in verbs {
                assert!(
                    implemented.contains(verb),
                    "`slipfork {verb}` is hinted but is not a subcommand of this binary: {hints}"
                );
            }
        }
    }

    #[test]
    fn status_renders_zero_worlds_as_one_line_plus_the_up_command() {
        let dashboard = render_plain(&status(80, Vec::new()));
        assert_eq!(
            dashboard,
            "slipforkd 1420, ingress 127.0.0.1:80\n  no worlds — run `slipfork up` inside a project checkout\n"
        );
    }

    #[test]
    fn status_renders_port_less_origins_at_80_and_one_hint_on_any_other_port() {
        let canonical = |port: u16| {
            render_plain(&status(
                port,
                vec![world(
                    "demo",
                    None,
                    WorldState::Running,
                    port,
                    lineage(None, None, None),
                )],
            ))
        };
        assert!(canonical(80).contains("http://demo.localhost "));
        assert_eq!(ingress_hint(80), None);

        let ephemeral = canonical(40645);
        assert!(ephemeral.contains("http://demo.localhost:40645 "));
        let hint = ingress_hint(40645).expect("a non-:80 ingress is announced");
        assert_eq!(
            hint,
            "slipfork: ingress is :40645, not :80 — run `sudo slipfork daemon install` once for port-less origins"
        );
        assert_eq!(hint.lines().count(), 1, "exactly one line");
    }

    #[test]
    fn status_json_carries_every_key_and_no_human_text() {
        let document = json(&status(
            80,
            vec![
                world(
                    "demo",
                    None,
                    WorldState::Running,
                    80,
                    lineage(Some(&digest("a1")), None, Some(&digest("45"))),
                ),
                world(
                    "demo:feat-ux",
                    Some("demo"),
                    WorldState::Failed,
                    80,
                    lineage(None, None, None),
                ),
            ],
        ));
        assert_eq!(document["ok"], true);
        assert_eq!(document["pid"], 1420);
        assert_eq!(document["ingress"]["addrs"][0], "127.0.0.1:80");
        assert_eq!(document["ingress"]["port"], 80);
        assert!(
            document["forward_port"].is_null(),
            "an absent forward port is null"
        );
        let worlds = document["worlds"].as_array().expect("worlds array");
        assert_eq!(worlds.len(), 2);
        assert!(
            worlds[0]["parent"].is_null(),
            "the canonical world has no parent"
        );
        assert_eq!(worlds[1]["parent"], "demo");
        assert_eq!(worlds[1]["state"], "failed");
        assert_eq!(worlds[0]["origin"], "http://demo.localhost");
        assert_eq!(worlds[0]["worktree"], "/home/dev/demo");
        for key in ["rootfs", "services", "seed", "base"] {
            assert!(
                worlds[0]["lineage"].get(key).is_some(),
                "lineage.{key} is always present: {}",
                worlds[0]["lineage"]
            );
        }
        assert!(
            worlds[1]["lineage"]["rootfs"].is_null(),
            "an unknown fact is null, never 0 or \"\""
        );
        let text = serde_json::to_string(&document).expect("json");
        for human in ["slipforkd", "lineage   ", "next      ", "no worlds"] {
            assert!(!text.contains(human), "no human text in --json: {human}");
        }
    }
}
