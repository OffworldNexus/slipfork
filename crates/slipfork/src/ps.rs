//! `slipfork ps`: one world's process tree (`docs/design/dx-0.1.0-ux.md` §1, §5, §10).
//!
//! Every function here is pure over the single [`WorldProcessReport`] the daemon returned: the
//! world's `state`, its build lineage, and one row per supervised component / `crun` backing
//! service. The CLI reads no process table, sysfs tree, environment, or log directory, and a
//! `None` pid/port/uptime renders `-` — never an invented `0`. `ps` is the per-world process view;
//! [`crate::status`] is the fleet view.

use slipfork_wire::control::WorldProcessReport;
use slipfork_world::ServiceKind;
use slipfork_world::ServiceState;

use crate::status::{render_lineage, world_state_label};
use crate::ui::{TerminalUi, block_label_width, column_width};

/// Narrowest id column; a longer service id widens the column instead of shifting the rest.
const MIN_ID_COLUMN: usize = 16;

/// Width of the table's `STATE` column (`running` is the widest label).
const STATE_COLUMN: usize = 7;

/// The human `ps` block: the world's `state`, the one lineage line, then the process table.
///
/// A `stopped` or `failed` world still prints its `state` and every retained row (the engine keeps
/// stopped rows), so `ps` never fails for a world that exists.
pub fn render(report: &WorldProcessReport, ui: &TerminalUi) -> String {
    let label_width = block_label_width(&["state", "lineage"]);
    let mut out = String::new();
    out.push_str(&format!(
        "{}{}\n",
        ui.pad("state", label_width),
        ui.state(world_state_label(report.state))
    ));
    out.push_str(&format!(
        "{}{}\n",
        ui.pad("lineage", label_width),
        render_lineage(&report.lineage, ui)
    ));
    out.push_str(&table(report, ui));
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
fn table(report: &WorldProcessReport, ui: &TerminalUi) -> String {
    let kind_width = column_width(
        "KIND".len(),
        report
            .services
            .iter()
            .map(|service| service_kind_label(service.info.kind)),
    );
    let id_width = column_width(
        MIN_ID_COLUMN,
        report
            .services
            .iter()
            .map(|service| service.info.id.as_str()),
    );
    let pids: Vec<String> = report
        .services
        .iter()
        .map(|service| dash(service.info.pid))
        .collect();
    let pid_width = column_width("PID".len(), pids.iter().map(String::as_str));
    let ports: Vec<String> = report
        .services
        .iter()
        .map(|service| dash(service.info.port))
        .collect();
    let port_width = column_width("PORT".len(), ports.iter().map(String::as_str));

    let mut out = format!(
        "{}  {}  {}  {}  {}  {}\n",
        ui.secondary(&ui.pad("KIND", kind_width)),
        ui.secondary(&ui.pad("ID", id_width)),
        ui.secondary(&ui.pad("STATE", STATE_COLUMN)),
        ui.secondary(&ui.pad("PID", pid_width)),
        ui.secondary(&ui.pad("PORT", port_width)),
        ui.secondary("UPTIME"),
    );
    for service in &report.services {
        out.push_str(&format!(
            "{}  {}  {}  {}  {}  {}\n",
            ui.secondary(&ui.pad(service_kind_label(service.info.kind), kind_width)),
            ui.pad(&service.info.id, id_width),
            ui.pad(
                &ui.state(service_state_label(service.info.state)),
                STATE_COLUMN
            ),
            ui.pad(&dash(service.info.pid), pid_width),
            ui.pad(&dash(service.info.port), port_width),
            service
                .info
                .uptime_secs
                .map_or_else(|| ui.secondary("-"), |secs| format!("{secs}s")),
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

#[cfg(test)]
mod tests {
    use slipfork_wire::control::{WorldServiceSummary, WorldState};
    use slipfork_world::WorldServiceInfo;
    use slipfork_world::{
        BaseLineage, CacheOutcome, RootfsLineage, SeedLineage, ServiceImageLineage, WorldLineage,
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
        let block = render(&report, &plain());
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
        let block = render(&report, &plain());
        assert!(block.starts_with("state     stopped\n"), "{block}");
        let row = block.lines().last().expect("one service row");
        let columns: Vec<&str> = row.split_whitespace().collect();
        assert_eq!(columns, ["component", "api", "stopped", "-", "-", "-"]);
    }

    #[test]
    fn ps_renders_a_failed_world_with_no_rows_as_its_header_only() {
        let report = report(WorldState::Failed, Vec::new());
        let block = render(&report, &plain());
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

    #[test]
    fn a_long_service_id_widens_the_id_column_without_shearing_later_columns() {
        let long_id = "pg_clusters.primary_psql";
        let report = report(
            WorldState::Running,
            vec![running(
                "demo",
                long_id,
                ServiceKind::BackingService,
                1_488,
                5_432,
                42,
            )],
        );
        let block = render(&report, &plain());
        let lines: Vec<&str> = block.lines().collect();
        let header = lines[2];
        let row = lines[3];

        assert!(
            block.starts_with("state     running\n"),
            "label floor: {block}"
        );
        assert_eq!(row.find(long_id), header.find("ID"), "{block}");
        assert_eq!(row.find("running"), header.find("STATE"), "{block}");
        assert_eq!(row.find("1488"), header.find("PID"), "{block}");
        assert_eq!(row.find("5432"), header.find("PORT"), "{block}");
    }
}
