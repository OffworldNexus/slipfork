//! The one fleet tree: `World -> Services -> Forks` (`docs/design/dx-cli-0.1.0.md` §1).
//!
//! `slipfork`, `slipfork status`, and `slipfork ps` all render through this one presenter, so the
//! fleet view and the cwd-scoped view can never drift. Every column width is measured from the
//! visible cells after ANSI stripping, a world line never carries a daemon pid, and lineage
//! digests appear only under `-v`.

use slipfork_wire::control::{WorldState, WorldSummary};
use slipfork_world::{ServiceState, WorldServiceInfo};

use super::render_lineage;
use crate::ui::{TerminalUi, column_width};

/// Indentation of the `-v` lineage sub-line.
const DETAIL_INDENT: &str = "  ";

/// The tree branch before a child that has a successor.
const BRANCH: &str = "├─";

/// The tree branch before the last child of a world.
const LAST_BRANCH: &str = "└─";

/// One fleet tree over an already-read payload.
pub(super) struct FleetTree<'a> {
    ui: &'a TerminalUi,
    verbose: bool,
}

/// Render the fleet tree; an empty daemon renders `○ No worlds running` (exit `0`).
pub(super) fn render(worlds: &[WorldSummary], ui: &TerminalUi, verbose: bool) -> String {
    if worlds.is_empty() {
        return "○ No worlds running\n".to_owned();
    }
    let tree = FleetTree { ui, verbose };
    let groups = groups(worlds);
    // One id column for the whole fleet: every world's service rows share it, so a long id in one
    // world never leaves another world's state column ragged.
    let id_width = column_width(
        0,
        groups
            .iter()
            .flat_map(|group| group.iter())
            .flat_map(|world| world.services.iter())
            .map(|service| service.info.id.as_str()),
    );
    let mut out = String::new();
    for (index, group) in groups.into_iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        tree.push_group(&mut out, &group, id_width);
    }
    out
}

impl FleetTree<'_> {
    /// One canonical world (or a lone fork), its service rows, its forks, and its `-v` lineage.
    fn push_group(&self, out: &mut String, group: &[&WorldSummary], id_width: usize) {
        let Some((root, forks)) = group.split_first() else {
            return;
        };
        out.push_str(&self.root_line(root));
        let services = &root.services;
        let children = services.len() + forks.len();
        for (index, service) in services.iter().enumerate() {
            out.push_str(&self.service_line(&service.info, connector(index, children), id_width));
        }
        for (index, fork) in forks.iter().enumerate() {
            out.push_str(&self.fork_line(fork, connector(services.len() + index, children)));
        }
        if self.verbose {
            out.push_str(&format!(
                "{DETAIL_INDENT}{} {}\n",
                self.ui.secondary("lineage"),
                render_lineage(&root.lineage, self.ui)
            ));
        }
    }

    /// `● demo  http://demo.localhost  /home/dev/demo` — never a `slipforkd <pid>` header.
    fn root_line(&self, world: &WorldSummary) -> String {
        format!(
            "{} {}  {}  {}\n",
            self.marker(world.state),
            self.ui.bold(&world.world),
            self.ui.url(world.origin.url()),
            self.ui.secondary(&world.worktree.display().to_string()),
        )
    }

    /// `├─ pg  running  :5432  12s` — an unrecorded port/age is `-`, never `0`.
    fn service_line(&self, service: &WorldServiceInfo, connector: &str, id_width: usize) -> String {
        let port = service
            .port
            .map_or_else(|| self.ui.secondary("-"), |port| format!(":{port}"));
        let age = service
            .uptime_secs
            .map_or_else(|| self.ui.secondary("-"), |secs| format!("{secs}s"));
        format!(
            "{connector} {} {}  {port}  {age}\n",
            self.ui.pad(&service.id, id_width),
            self.ui.state(service_state_label(service.state)),
        )
    }

    /// `└─ feat-auth  ● http://feat-auth.demo.localhost  /repo/.worktrees/feat-auth`.
    fn fork_line(&self, world: &WorldSummary, connector: &str) -> String {
        let name = world
            .world
            .rsplit_once(':')
            .map_or(world.world.as_str(), |(_, fork)| fork);
        format!(
            "{connector} {}  {} {}  {}\n",
            self.ui.bold(name),
            self.marker(world.state),
            self.ui.url(world.origin.url()),
            self.ui.secondary(&world.worktree.display().to_string()),
        )
    }

    /// The lifecycle glyph: `●` running, `✗` failed, `○` stopped.
    fn marker(&self, state: WorldState) -> String {
        match state {
            WorldState::Running => self.ui.running("●"),
            WorldState::Failed => self.ui.failed("✗"),
            WorldState::Stopped => self.ui.secondary("○"),
        }
    }
}

/// The branch before child `index` of `total`.
fn connector(index: usize, total: usize) -> &'static str {
    if index + 1 == total {
        LAST_BRANCH
    } else {
        BRANCH
    }
}

/// Every canonical world followed by the forks the daemon names as its children.
///
/// Grouping uses `WorldSummary.parent` (never a split of the world id) and keeps the daemon's
/// payload order inside a group; a fork whose parent has no row still renders as its own group.
fn groups(worlds: &[WorldSummary]) -> Vec<Vec<&WorldSummary>> {
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

/// A service's lifecycle label; `exited` = ended on its own, `stopped` = `down`/`restart`.
fn service_state_label(state: ServiceState) -> &'static str {
    match state {
        ServiceState::Running => "running",
        ServiceState::Exited => "exited",
        ServiceState::Stopped => "stopped",
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use slipfork_wire::control::WorldServiceSummary;
    use slipfork_world::{
        LocalHostRoute, ProjectSlug, ServiceKind, WorldLineage, WorldOrigin, WorldServiceInfo,
        WorldTarget,
    };

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

    fn origin(project: &str, fork: Option<&str>) -> WorldOrigin {
        let project = ProjectSlug::parse(project).expect("a valid project slug");
        let target = match fork {
            Some(fork) => WorldTarget::UserFork(fork.to_owned()),
            None => WorldTarget::Canonical,
        };
        let route = LocalHostRoute::new(project, target, None, &[]).expect("a routable route");
        WorldOrigin::local(&route, 80).expect("a minted origin")
    }

    fn service(
        world: &str,
        id: &str,
        state: ServiceState,
        port: Option<u16>,
    ) -> WorldServiceSummary {
        WorldServiceSummary {
            world: world.to_owned(),
            info: WorldServiceInfo {
                id: id.to_owned(),
                kind: ServiceKind::Component,
                state,
                pid: Some(1_000),
                port,
                uptime_secs: port.map(|_| 12),
            },
        }
    }

    fn world(
        id: &str,
        parent: Option<&str>,
        state: WorldState,
        services: Vec<WorldServiceSummary>,
    ) -> WorldSummary {
        WorldSummary {
            world: id.to_owned(),
            parent: parent.map(str::to_owned),
            state,
            origin: origin(
                parent.unwrap_or(id),
                id.split_once(':').map(|(_, fork)| fork),
            ),
            worktree: PathBuf::from(format!("/repo/{id}")),
            lineage: WorldLineage {
                rootfs: None,
                services: Vec::new(),
                seed: None,
                base: None,
            },
            services,
        }
    }

    #[test]
    fn an_empty_fleet_is_one_line() {
        assert_eq!(render(&[], &plain(), false), "○ No worlds running\n");
    }

    #[test]
    fn a_world_leads_its_services_and_forks_and_the_last_child_closes_the_branch() {
        let worlds = vec![
            world(
                "demo",
                None,
                WorldState::Running,
                vec![
                    service("demo", "pg", ServiceState::Running, Some(5432)),
                    service("demo", "api", ServiceState::Running, Some(8000)),
                ],
            ),
            world(
                "demo:feat-auth",
                Some("demo"),
                WorldState::Running,
                Vec::new(),
            ),
        ];
        let tree = render(&worlds, &plain(), false);
        let lines: Vec<&str> = tree.lines().collect();
        assert_eq!(lines[0], "● demo  http://demo.localhost  /repo/demo");
        assert_eq!(lines[1], "├─ pg  running  :5432  12s");
        assert_eq!(lines[2], "├─ api running  :8000  12s");
        assert_eq!(
            lines[3],
            "└─ feat-auth  ● http://feat-auth.demo.localhost  /repo/demo:feat-auth"
        );
        assert!(!tree.contains("slipforkd"), "no daemon header: {tree}");
        assert!(!tree.contains("next"), "no next spam: {tree}");
        assert!(!tree.contains("lineage"), "no lineage unless -v: {tree}");
    }

    #[test]
    fn stops_and_failures_carry_their_own_glyph_and_word() {
        let worlds = vec![world(
            "demo",
            None,
            WorldState::Failed,
            vec![service("demo", "api", ServiceState::Exited, None)],
        )];
        let tree = render(&worlds, &plain(), false);
        let lines: Vec<&str> = tree.lines().collect();
        assert!(lines[0].starts_with("✗ demo  "), "{tree}");
        assert_eq!(lines[1], "└─ api exited  -  -");
    }

    #[test]
    fn a_long_service_id_widens_the_id_column_without_shearing_the_state() {
        let worlds = vec![world(
            "demo",
            None,
            WorldState::Running,
            vec![
                service(
                    "demo",
                    "pg_clusters.primary_psql",
                    ServiceState::Running,
                    Some(5432),
                ),
                service("demo", "api", ServiceState::Running, Some(8000)),
            ],
        )];
        let tree = render(&worlds, &plain(), false);
        let lines: Vec<&str> = tree.lines().collect();
        let long = lines[1];
        let short = lines[2];
        assert!(long.contains("pg_clusters.primary_psql running"), "{tree}");
        assert_eq!(
            short.find("running"),
            long.find("running"),
            "the state column stays aligned: {tree}"
        );
    }

    #[test]
    fn verbose_appends_the_one_compact_lineage_line() {
        let worlds = vec![world("demo", None, WorldState::Running, Vec::new())];
        let tree = render(&worlds, &plain(), true);
        assert_eq!(
            tree,
            "● demo  http://demo.localhost  /repo/demo\n  lineage rootfs - - · seed - - · base - -\n"
        );
    }

    #[test]
    fn a_fork_without_its_parent_row_still_renders() {
        let worlds = vec![world(
            "demo:feat-api",
            Some("demo"),
            WorldState::Stopped,
            Vec::new(),
        )];
        let tree = render(&worlds, &plain(), false);
        assert_eq!(
            tree,
            "○ demo:feat-api  http://feat-api.demo.localhost  /repo/demo:feat-api\n"
        );
    }
}
