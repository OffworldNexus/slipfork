//! The flat, domain-first status tree.
//!
//! Worlds and forks are always top-level headers; each domain lists its mapped sub-paths, and every
//! unrouted service, resource, or orphaned pod lands in one trailing `support` group. Ports and
//! ages stay in `--json` and never reach a human line. Every row renders through the one borderless
//! [`Table`], so each column has a single start index across the whole tree.

use serde_json::Value;
use slipfork_world::{FORKS_DIR, ProjectScope};

use crate::ps::{PsReply, PsStoppedFork, PsWorld, PsWorldState};
use crate::ui::Palette;
use crate::ui::table::{Column, ColumnWidths, Table};

use super::model::{Options, display_name, display_path, parse as decode};
use super::rows::{DomainRows, RoutedRow, SupportRow, WorldRows, project};

pub use super::model::no_world_line;

const HEADER: [Column; 4] = [
    Column::left(),
    Column::left(),
    Column::left(),
    Column::left(),
];
const ROUTE: [Column; 5] = [
    Column::left(),
    Column::left(),
    Column::left(),
    Column::left(),
    Column::left(),
];
const SUPPORT: [Column; 4] = [
    Column::left(),
    Column::left(),
    Column::left(),
    Column::left(),
];

/// Every precomputed column grid for one render.
struct Widths {
    header: ColumnWidths,
    route: ColumnWidths,
    support: ColumnWidths,
}

impl Widths {
    /// Measures the header, route, and support grids over every flattened world.
    fn measure(worlds: &[WorldRows<'_>], options: &Options) -> Self {
        let mut header = Vec::new();
        let mut route = Vec::new();
        let mut support = Vec::new();
        for rows in worlds {
            header.push(header_cells(rows.world, options));
            for fork in &rows.world.stopped_forks {
                header.push(fork_cells(fork, options));
            }
            for domain in &rows.domains {
                for row in &domain.rows {
                    route.push(route_cells(row));
                }
            }
            for row in &rows.support {
                support.push(vec![
                    row.keyword.clone(),
                    row.target.clone(),
                    row.state.word.to_owned(),
                    row.command.clone().unwrap_or_default(),
                ]);
            }
        }
        Self {
            header: ColumnWidths::measure(&HEADER, &header),
            route: ColumnWidths::measure(&ROUTE, &route),
            support: ColumnWidths::measure(&SUPPORT, &support),
        }
    }
}

fn header_cells(world: &PsWorld, options: &Options) -> Vec<String> {
    let state = if world.state == PsWorldState::Stopped {
        "stopped"
    } else {
        ""
    };
    let pid = match world.supervisor_pid {
        Some(pid) if options.verbose && state.is_empty() => format!("pid {pid}"),
        _ => String::new(),
    };
    vec![
        display_name(world).to_owned(),
        display_path(&world.path, options.home.as_deref()),
        state.to_owned(),
        pid,
    ]
}

fn fork_cells(fork: &PsStoppedFork, options: &Options) -> Vec<String> {
    vec![
        fork.name.clone(),
        display_path(&fork.path, options.home.as_deref()),
        "stopped".to_owned(),
        String::new(),
    ]
}

fn route_cells(row: &RoutedRow) -> Vec<String> {
    vec![
        row.path.clone(),
        "->".to_owned(),
        row.target.clone(),
        row.kind.clone().unwrap_or_default(),
        row.state.word.to_owned(),
    ]
}

/// Decodes the daemon's `ps` payload; a malformed payload is an error, never an empty tree.
///
/// # Errors
///
/// Returns the serde diagnostic so the caller can name the exact remedy.
pub fn parse(value: &Value) -> Result<PsReply, String> {
    decode(value)
}

/// Renders the `ps` reply as the flat domain tree; an empty reply is the dim `○` line.
#[must_use]
pub fn render(reply: &PsReply, options: &Options) -> String {
    let mut worlds = Vec::new();
    flatten(&reply.worlds, &mut worlds);
    if worlds.is_empty() {
        return format!(
            "{}\n",
            Palette::new(options.color).meta("○ No worlds running")
        );
    }
    let projected: Vec<WorldRows<'_>> = worlds.iter().map(|world| project(world)).collect();
    let widths = Widths::measure(&projected, options);
    let palette = Palette::new(options.color);
    let mut out = String::new();
    for (index, rows) in projected.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        render_world(&mut out, rows, options, palette, &widths);
    }
    let stopped: Vec<&PsStoppedFork> = projected
        .iter()
        .flat_map(|rows| rows.world.stopped_forks.iter())
        .collect();
    render_stopped_forks(&mut out, &stopped, options, palette, &widths);
    out
}

/// Keeps the canonical project or the one fork that contains the caller's cwd.
///
/// The layout is resolved once by the composition root into a [`ProjectScope`]; this renderer only
/// reads the reply and that value struct, so a scope never probes the filesystem twice. In a
/// project root the scope carries the root row (running or dim `stopped`), every live fork, and
/// every stopped sibling discovered on disk, so a `down` root never hides a running fork.
#[must_use]
pub fn scope(reply: &PsReply, scope: &ProjectScope) -> PsReply {
    match scope {
        ProjectScope::None => PsReply::default(),
        ProjectScope::Fork { path, name, world } => scope_fork(reply, path, name, world),
        ProjectScope::Root {
            root,
            world,
            forks,
            problem,
        } => scope_project(reply, root, world, forks, problem.as_deref()),
    }
}

/// Scopes strictly to one fork checkout: its live row, or its dim stopped row.
fn scope_fork(reply: &PsReply, fork_path: &std::path::Path, name: &str, world: &str) -> PsReply {
    let mut worlds = Vec::new();
    flatten(&reply.worlds, &mut worlds);
    if let Some(found) = worlds.iter().find(|world| world.project == fork_path) {
        return PsReply {
            worlds: vec![(*found).clone()],
        };
    }
    // The scope already proved the checkout exists, so no second disk probe happens here.
    PsReply {
        worlds: vec![PsWorld::stopped(fork_path, world, Some(name))],
    }
}

/// Scopes to the project root: its own row plus every live and stopped `.forks` checkout.
fn scope_project(
    reply: &PsReply,
    root: &std::path::Path,
    world: &str,
    forks: &[slipfork_world::ForkDir],
    problem: Option<&str>,
) -> PsReply {
    let mut live = Vec::new();
    flatten(&reply.worlds, &mut live);
    let forks_dir = root.join(FORKS_DIR);
    let mut rows: Vec<PsWorld> = live
        .iter()
        .filter(|world| world.project.starts_with(&forks_dir))
        .map(|world| (*world).clone())
        .collect();
    rows.sort_by(|left, right| left.project.cmp(&right.project));
    let stopped: Vec<PsStoppedFork> = forks
        .iter()
        .filter(|fork| !rows.iter().any(|row| row.project == fork.path))
        .map(|fork| PsStoppedFork {
            name: fork.name.clone(),
            path: fork.path.clone(),
        })
        .collect();
    let mut root_row = live
        .iter()
        .find(|row| row.project == root)
        .map_or_else(|| PsWorld::stopped(root, world, None), |row| (*row).clone());
    root_row.forks = rows;
    root_row.stopped_forks = stopped;
    if let Some(problem) = problem {
        root_row.problem = Some(match root_row.problem.take() {
            Some(existing) => format!("{existing}; {problem}"),
            None => problem.to_owned(),
        });
    }
    PsReply {
        worlds: vec![root_row],
    }
}

fn flatten<'a>(worlds: &'a [PsWorld], out: &mut Vec<&'a PsWorld>) {
    for world in worlds {
        out.push(world);
        flatten(&world.forks, out);
    }
}

fn render_world(
    out: &mut String,
    rows: &WorldRows<'_>,
    options: &Options,
    palette: Palette,
    widths: &Widths,
) {
    let world = rows.world;
    let table = Table::new(&HEADER);
    if world.state == PsWorldState::Stopped {
        out.push_str(&palette.meta("○ "));
        let cells = vec![
            palette.meta(display_name(world)),
            palette.meta(&display_path(&world.path, options.home.as_deref())),
            palette.status("stopped"),
            String::new(),
        ];
        out.push_str(&table.line(&widths.header, &cells));
        out.push('\n');
    } else {
        out.push_str(&palette.tint("●", "32"));
        out.push(' ');
        let pid = match world.supervisor_pid {
            Some(pid) if options.verbose => palette.meta(&format!("pid {pid}")),
            _ => String::new(),
        };
        let cells = vec![
            palette.value(display_name(world)),
            palette.path(&display_path(&world.path, options.home.as_deref())),
            String::new(),
            pid,
        ];
        out.push_str(&table.line(&widths.header, &cells));
        out.push('\n');
    }

    if let Some(problem) = &world.problem {
        out.push_str(&palette.error(&format!("! {problem}")));
        out.push('\n');
    }

    let support = !rows.support.is_empty();
    let groups = rows.domains.len() + usize::from(support);
    for (index, domain) in rows.domains.iter().enumerate() {
        let last = index + 1 == groups;
        render_domain(out, domain, last, palette, widths);
    }
    if support {
        render_support(out, &rows.support, palette, widths);
    }
}

/// Every world's stopped `.forks/<name>` checkouts render in one trailing block after all live
/// world trees, on the shared header grid, so a live fork never renders below a stopped one.
fn render_stopped_forks(
    out: &mut String,
    forks: &[&PsStoppedFork],
    options: &Options,
    palette: Palette,
    widths: &Widths,
) {
    if forks.is_empty() {
        return;
    }
    out.push('\n');
    let table = Table::new(&HEADER);
    for fork in forks {
        out.push_str(&palette.meta("○ "));
        let cells = vec![
            palette.meta(&fork.name),
            palette.meta(&display_path(&fork.path, options.home.as_deref())),
            palette.status("stopped"),
            String::new(),
        ];
        out.push_str(&table.line(&widths.header, &cells));
        out.push('\n');
    }
}

fn render_domain(
    out: &mut String,
    domain: &DomainRows,
    last: bool,
    palette: Palette,
    widths: &Widths,
) {
    out.push_str("  ");
    out.push_str(&palette.meta(if last { "└─ " } else { "├─ " }));
    out.push_str(&palette.url(&format!("http://{}", domain.host)));
    out.push('\n');
    let branch = if last { "   " } else { "│  " };
    let table = Table::new(&ROUTE);
    for (index, row) in domain.rows.iter().enumerate() {
        out.push_str("  ");
        out.push_str(&palette.meta(branch));
        let last_row = index + 1 == domain.rows.len();
        out.push_str(&palette.meta(if last_row { "└─ " } else { "├─ " }));
        let cells = [
            palette.meta(&row.path),
            palette.meta("->"),
            palette.value(&row.target),
            match &row.kind {
                Some(kind) => palette.kind(kind),
                None => String::new(),
            },
            palette.status(row.state.word),
        ];
        out.push_str(&table.line(&widths.route, &cells));
        out.push('\n');
    }
}

fn render_support(out: &mut String, rows: &[SupportRow], palette: Palette, widths: &Widths) {
    out.push_str("  ");
    out.push_str(&palette.meta("└─ "));
    out.push_str(&palette.meta("support"));
    out.push('\n');
    let table = Table::new(&SUPPORT);
    for (index, row) in rows.iter().enumerate() {
        out.push_str("     ");
        out.push_str(&palette.meta(if index + 1 == rows.len() {
            "└─ "
        } else {
            "├─ "
        }));
        let cells = [
            palette.verb(&row.keyword),
            palette.value(&row.target),
            palette.status(row.state.word),
            row.command.clone().unwrap_or_default(),
        ];
        out.push_str(&table.line(&widths.support, &cells));
        out.push('\n');
    }
}
