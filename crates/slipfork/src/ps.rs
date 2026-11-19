//! `slipfork ps`: the cwd-scoped view of the same fleet tree (`docs/design/dx-cli-0.1.0.md` §1, §3).
//!
//! `ps` is not a second table: it is exactly the tree `slipfork` renders, scoped by the daemon to
//! `cwd`'s world. The report is the daemon's single read — world, state, origin, worktree, lineage,
//! and every service row — so the CLI reads no process table, sysfs tree, or log directory, and a
//! `None` pid/port/uptime renders `-`, never an invented `0`.

use slipfork_wire::control::{WorldProcessReport, WorldSummary};

use crate::ui::TerminalUi;

/// The human `ps` block: the one `World -> Services` tree for the selected world.
pub fn render(report: &WorldProcessReport, ui: &TerminalUi) -> String {
    let world = WorldSummary {
        world: report.world.clone(),
        parent: report.parent.clone(),
        state: report.state,
        origin: report.origin.clone(),
        worktree: report.worktree.clone(),
        lineage: report.lineage.clone(),
        services: report.services.clone(),
    };
    crate::status::render_worlds(std::slice::from_ref(&world), ui)
}

/// The `ps --json` document (`docs/design/dx-cli-0.1.0.md` §3).
///
/// The field names are the contract: `ok`, `world`, `parent`, `state`, `origin`, `worktree`,
/// `lineage`, and the flattened service rows. Unknown facts stay `null`, never `0`.
pub fn json(report: &WorldProcessReport) -> serde_json::Value {
    serde_json::json!({
        "ok": true,
        "world": &report.world,
        "parent": &report.parent,
        "state": report.state,
        "origin": report.origin.url(),
        "worktree": &report.worktree,
        "lineage": &report.lineage,
        "services": &report.services,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use slipfork_wire::control::{WorldServiceSummary, WorldState, WorldSummary};
    use slipfork_world::{
        CacheOutcome, LocalHostRoute, ProjectSlug, RootfsLineage, ServiceKind, ServiceState,
        WorldLineage, WorldOrigin, WorldServiceInfo, WorldTarget,
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

    fn digest(prefix: &str) -> String {
        format!("{prefix}{}", "0".repeat(64 - prefix.len()))
    }

    fn lineage() -> WorldLineage {
        WorldLineage {
            rootfs: Some(RootfsLineage {
                digest: digest("a1b2c3d4e5f6"),
                cache: CacheOutcome::Hit,
                millis: None,
            }),
            services: Vec::new(),
            seed: None,
            base: None,
        }
    }

    fn service(world: &str, id: &str, port: Option<u16>) -> WorldServiceSummary {
        WorldServiceSummary {
            world: world.to_owned(),
            info: WorldServiceInfo {
                id: id.to_owned(),
                kind: ServiceKind::BackingService,
                state: ServiceState::Stopped,
                pid: None,
                port,
                uptime_secs: None,
            },
        }
    }

    fn report(state: WorldState, services: Vec<WorldServiceSummary>) -> WorldProcessReport {
        let project = ProjectSlug::parse("demo").expect("a valid project slug");
        let route = LocalHostRoute::new(project, WorldTarget::Canonical, None, &[])
            .expect("a routable route");
        let summary = WorldSummary {
            world: "demo".to_owned(),
            parent: None,
            state,
            origin: WorldOrigin::local(&route, 80).expect("a minted origin"),
            worktree: PathBuf::from("/home/dev/demo"),
            lineage: lineage(),
            services: services.clone(),
        };
        WorldProcessReport::new(&summary, services).expect("unique service ids")
    }

    #[test]
    fn ps_renders_the_same_tree_scoped_to_one_world() {
        let report = report(
            WorldState::Running,
            vec![
                service("demo", "pg_databases.main", Some(5_432)),
                service("demo", "api", Some(8_000)),
            ],
        );
        let block = render(&report, &plain());
        let lines: Vec<&str> = block.lines().collect();
        assert_eq!(lines[0], "● demo  http://demo.localhost  /home/dev/demo");
        assert_eq!(lines[1], "├─ pg_databases.main stopped  :5432  -");
        assert_eq!(lines[2], "└─ api               stopped  :8000  -");
        assert!(!block.contains("KIND"), "no second table: {block}");
        assert!(!block.contains("next"), "no next spam: {block}");
        assert!(!block.contains("PID"), "no pid column: {block}");
    }

    #[test]
    fn ps_json_matches_the_section_three_contract() {
        let report = report(
            WorldState::Running,
            vec![service("demo", "api", Some(8_000))],
        );
        let document = json(&report);
        for key in [
            "ok", "world", "parent", "state", "origin", "worktree", "lineage", "services",
        ] {
            assert!(document.get(key).is_some(), "document.{key} is present");
        }
        assert_eq!(document["ok"], true);
        assert_eq!(document["world"], "demo");
        assert_eq!(document["state"], "running");
        assert_eq!(document["origin"], "http://demo.localhost");
        assert_eq!(document["worktree"], "/home/dev/demo");
        assert_eq!(
            document["lineage"]["rootfs"]["digest"],
            digest("a1b2c3d4e5f6"),
            "full digests survive --json"
        );
        let rows = document["services"].as_array().expect("services array");
        assert_eq!(rows[0]["id"], "api");
        assert_eq!(rows[0]["port"], 8_000);
        assert!(rows[0]["pid"].is_null());

        let text = serde_json::to_string(&document).expect("json");
        for human in ["slipforkd", "KIND", "next"] {
            assert!(!text.contains(human), "no human text in --json: {human}");
        }
    }

    #[test]
    fn a_failed_world_with_no_rows_is_still_its_own_line() {
        let block = render(&report(WorldState::Failed, Vec::new()), &plain());
        assert_eq!(block, "✗ demo  http://demo.localhost  /home/dev/demo\n");
    }

    #[test]
    fn a_fork_report_keeps_its_daemon_parent() {
        let project = ProjectSlug::parse("demo").expect("a valid project slug");
        let route =
            LocalHostRoute::new(project, WorldTarget::UserFork("feat".to_owned()), None, &[])
                .expect("a routable route");
        let summary = WorldSummary {
            world: "demo:feat".to_owned(),
            parent: Some("demo".to_owned()),
            state: WorldState::Running,
            origin: WorldOrigin::local(&route, 80).expect("a minted origin"),
            worktree: PathBuf::from("/home/dev/demo/.worktrees/feat"),
            lineage: lineage(),
            services: Vec::new(),
        };
        let report = WorldProcessReport::new(&summary, Vec::new()).expect("one world");
        assert_eq!(json(&report)["parent"], "demo");
        assert_eq!(
            render(&report, &plain()),
            "● demo:feat  http://feat.demo.localhost  /home/dev/demo/.worktrees/feat\n"
        );
    }
}
