//! `slipfork ps`: one world's process tree (`docs/design/dx-0.1.0-ux.md` §1, §5, §10).
//!
//! Every function here is pure over the single [`WorldProcessReport`] the daemon returned: the
//! world's `state`, its build lineage, and one row per supervised component / `crun` backing
//! service. The CLI reads no process table, sysfs tree, environment, or log directory, and a
//! `None` pid/port/uptime renders `-` — never an invented `0`. `ps` is the per-world process view;
//! [`crate::status`] is the fleet view.

use slipfork_engine::ServiceState;
use slipfork_wire::control::{ServiceKind, WorldProcessReport};

use crate::status::{render_lineage, world_state_label};

/// Width of the block's label column (`state     `, `lineage   `), shared with `restart`'s block.
const LABEL_WIDTH: usize = 10;

/// Narrowest id column; a longer service id widens the column instead of shifting the rest.
const MIN_ID_COLUMN: usize = 16;

/// The human `ps` block: the world's `state`, the one lineage line, then the process table.
///
/// A `stopped` or `failed` world still prints its `state` and every retained row (the engine keeps
/// stopped rows), so `ps` never fails for a world that exists.
pub fn render(report: &WorldProcessReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{:<LABEL_WIDTH$}{}\n",
        "state",
        world_state_label(report.state)
    ));
    out.push_str(&format!(
        "{:<LABEL_WIDTH$}{}\n",
        "lineage",
        render_lineage(&report.lineage)
    ));
    out.push_str(&table(report));
    out
}

/// The `ps --json` document (`docs/design/dx-0.1.0-ux.md` §10).
///
/// Every fact is the daemon's own: `state`, the full 64-hex lineage, and the flattened service rows
/// (`world`, `id`, `kind`, `state`, `pid`, `port`, `uptime_secs`). Unknown facts stay `null`, never
/// `0`. The header's world id comes from the rows themselves (a report carries no world field), so
/// a world with no retained rows reports `"world": null` instead of inventing one.
pub fn json(report: &WorldProcessReport) -> serde_json::Value {
    serde_json::json!({
        "ok": true,
        "world": report.services.first().map(|service| &service.world),
        "state": report.state,
        "lineage": &report.lineage,
        "services": &report.services,
    })
}

/// The process table, aligned to the widest value in the report.
fn table(report: &WorldProcessReport) -> String {
    let kind_width = report
        .services
        .iter()
        .map(|service| service_kind_label(service.info.kind).len())
        .max()
        .unwrap_or(0)
        .max("KIND".len());
    let id_width = report
        .services
        .iter()
        .map(|service| service.info.id.chars().count())
        .max()
        .unwrap_or(0)
        .max(MIN_ID_COLUMN);
    let pid_width = "PID".len().max(
        report
            .services
            .iter()
            .map(|service| dash_len(service.info.pid))
            .max()
            .unwrap_or(0),
    );
    let port_width = "PORT".len().max(
        report
            .services
            .iter()
            .map(|service| dash_len(service.info.port))
            .max()
            .unwrap_or(0),
    );

    let mut out = format!(
        "{:<kind_width$}  {:<id_width$}  {:<7}  {:<pid_width$}  {:<port_width$}  {}\n",
        "KIND", "ID", "STATE", "PID", "PORT", "UPTIME"
    );
    for service in &report.services {
        out.push_str(&format!(
            "{:<kind_width$}  {:<id_width$}  {:<7}  {:<pid_width$}  {:<port_width$}  {}\n",
            service_kind_label(service.info.kind),
            service.info.id,
            service_state_label(service.info.state),
            dash(service.info.pid),
            dash(service.info.port),
            service
                .info
                .uptime_secs
                .map_or_else(|| "-".to_owned(), |secs| format!("{secs}s")),
        ));
    }
    out
}

/// The service's lifecycle label; `exited` = ended on its own, `stopped` = `down`/`restart`.
fn service_state_label(state: ServiceState) -> &'static str {
    match state {
        ServiceState::Running => "running",
        ServiceState::Exited => "exited",
        ServiceState::Stopped => "stopped",
    }
}

/// The row's kind: a `snow.yml` component or a `crun`-backed resource service.
fn service_kind_label(kind: ServiceKind) -> &'static str {
    match kind {
        ServiceKind::Component => "component",
        ServiceKind::BackingService => "backing_service",
    }
}

/// One optional fact as text, with `-` for `None` (never `0`).
fn dash<T: std::fmt::Display>(value: Option<T>) -> String {
    value.map_or_else(|| "-".to_owned(), |value| value.to_string())
}

fn dash_len<T: std::fmt::Display>(value: Option<T>) -> usize {
    value.map_or(1, |value| value.to_string().len())
}

#[cfg(test)]
mod tests {
    use slipfork_engine::WorldServiceInfo;
    use slipfork_wire::control::{
        BaseLineage, CacheOutcome, RootfsLineage, SeedLineage, ServiceImageLineage, WorldLineage,
        WorldServiceSummary, WorldState,
    };

    use super::*;

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
            services: vec![ServiceImageLineage {
                id: "postgres:18".to_owned(),
                cache: CacheOutcome::Hit,
                millis: None,
            }],
            seed: Some(SeedLineage {
                digest: Some(digest("9f8e7d6c5b4a")),
                rebuilt: Some(true),
                millis: Some(2_104),
            }),
            base: Some(BaseLineage {
                key: digest("4570132bffb8"),
                millis: None,
            }),
        }
    }

    fn service(world: &str, row: WorldServiceInfo) -> WorldServiceSummary {
        WorldServiceSummary {
            world: world.to_owned(),
            info: row,
        }
    }

    fn running(
        world: &str,
        id: &str,
        kind: ServiceKind,
        pid: u32,
        port: u16,
        uptime: u64,
    ) -> WorldServiceSummary {
        service(
            world,
            WorldServiceInfo {
                id: id.to_owned(),
                kind,
                state: ServiceState::Running,
                pid: Some(pid),
                port: Some(port),
                uptime_secs: Some(uptime),
            },
        )
    }

    fn report(state: WorldState, services: Vec<WorldServiceSummary>) -> WorldProcessReport {
        WorldProcessReport::new(state, lineage(), services).expect("unique service ids")
    }

    #[test]
    fn ps_renders_the_state_lineage_header_then_one_row_per_component_and_backing_service() {
        let report = report(
            WorldState::Running,
            vec![
                running(
                    "demo",
                    "pg_clusters.psql",
                    ServiceKind::BackingService,
                    1_488,
                    5_432,
                    42,
                ),
                running("demo", "api", ServiceKind::Component, 1_512, 8_000, 41),
            ],
        );
        let block = render(&report);
        let lines: Vec<&str> = block.lines().collect();
        assert_eq!(lines[0], "state     running");
        assert_eq!(
            lines[1],
            "lineage   rootfs a1b2c3d4e5f6 cached · postgres:18 cached · seed 9f8e7d6c5b4a rebuilt 2.1s · base 4570132bffb8 reused"
        );
        assert!(
            lines[2].starts_with("KIND") && lines[2].contains("UPTIME"),
            "the table header names every column: {block}"
        );
        for column in ["ID", "STATE", "PID", "PORT"] {
            assert!(lines[2].contains(column), "missing {column}: {block}");
        }
        assert!(lines[3].contains("backing_service"));
        assert!(lines[3].contains("pg_clusters.psql"));
        assert!(lines[3].contains("running"));
        assert!(lines[3].contains("1488"));
        assert!(lines[3].contains("5432"));
        assert!(lines[3].contains("42s"));
        assert!(lines[4].contains("component") && lines[4].contains("api"));
        assert!(lines[4].contains("1512") && lines[4].contains("8000") && lines[4].contains("41s"));
        assert_eq!(lines.len(), 5, "header + one row per service: {block}");
    }

    #[test]
    fn ps_prints_a_dash_for_every_unrecorded_fact_and_never_a_zero() {
        let report = report(
            WorldState::Stopped,
            vec![service(
                "demo",
                WorldServiceInfo {
                    id: "api".to_owned(),
                    kind: ServiceKind::Component,
                    state: ServiceState::Stopped,
                    pid: None,
                    port: None,
                    uptime_secs: None,
                },
            )],
        );
        let block = render(&report);
        assert!(block.starts_with("state     stopped\n"), "{block}");
        let row = block.lines().last().expect("one service row");
        let columns: Vec<&str> = row.split_whitespace().collect();
        assert_eq!(columns, ["component", "api", "stopped", "-", "-", "-"]);
    }

    #[test]
    fn ps_renders_a_failed_world_with_no_rows_as_its_header_only() {
        let report = report(WorldState::Failed, Vec::new());
        let block = render(&report);
        assert_eq!(block.lines().count(), 3, "{block}");
        assert!(block.contains("state     failed"));
        assert!(block.contains("KIND"));
    }

    #[test]
    fn ps_json_carries_every_key_and_preserves_null() {
        let report = report(
            WorldState::Running,
            vec![
                running("demo", "api", ServiceKind::Component, 1_512, 8_000, 41),
                service(
                    "demo",
                    WorldServiceInfo {
                        id: "pg_databases.main".to_owned(),
                        kind: ServiceKind::BackingService,
                        state: ServiceState::Exited,
                        pid: None,
                        port: None,
                        uptime_secs: None,
                    },
                ),
            ],
        );
        let document = json(&report);
        assert_eq!(document["ok"], true);
        assert_eq!(document["world"], "demo");
        assert_eq!(document["state"], "running");
        for key in ["rootfs", "services", "seed", "base"] {
            assert!(
                document["lineage"].get(key).is_some(),
                "lineage.{key} is always present: {}",
                document["lineage"]
            );
        }
        assert_eq!(
            document["lineage"]["rootfs"]["digest"],
            digest("a1b2c3d4e5f6"),
            "full digests survive --json"
        );
        let rows = document["services"].as_array().expect("services array");
        assert_eq!(rows.len(), 2);
        for key in ["world", "id", "kind", "state", "pid", "port", "uptime_secs"] {
            assert!(rows[0].get(key).is_some(), "services[0].{key} is present");
        }
        assert_eq!(rows[0]["id"], "api");
        assert_eq!(rows[0]["kind"], "component");
        assert_eq!(rows[0]["pid"], 1512);
        assert_eq!(rows[1]["kind"], "backing_service");
        assert_eq!(rows[1]["state"], "exited");
        assert!(rows[1]["pid"].is_null());
        assert!(rows[1]["port"].is_null());
        assert!(rows[1]["uptime_secs"].is_null());

        let text = serde_json::to_string(&document).expect("json");
        for human in ["state     ", "lineage   ", "KIND"] {
            assert!(!text.contains(human), "no human text in --json: {human}");
        }
    }

    #[test]
    fn ps_json_without_rows_never_invents_a_world_id() {
        let document = json(&report(WorldState::Failed, Vec::new()));
        assert!(document["world"].is_null(), "{document}");
        assert_eq!(document["services"], serde_json::json!([]));
    }
}
