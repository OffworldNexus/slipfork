//! The flat, domain-first status tree.
//!
//! Worlds and forks are always top-level headers; each domain lists its mapped sub-paths, and every
//! unrouted service, resource, or orphaned pod lands in one trailing `support` group. Ports and
//! ages stay in `--json` and never reach a human line.

use serde_json::Value;
use slipfork_world::{FORKS_DIR, ProjectScope};

use crate::ps::{PsReply, PsStoppedFork, PsWorld, PsWorldState};
use crate::ui::Palette;

use super::model::{Options, display_name, display_path, parse as decode};
use super::rows::{RoutedRow, SupportRow, WorldRows, project};
use super::widths::{Widths, pad};

pub use super::model::no_world_line;

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
            Palette::new(options.color).dim("○ No worlds running")
        );
    }
    let projected: Vec<WorldRows<'_>> = worlds.iter().map(|world| project(world)).collect();
    let widths = Widths::measure(&projected, options.home.as_deref());
    let palette = Palette::new(options.color);
    let mut out = String::new();
    for (index, rows) in projected.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        render_world(&mut out, rows, options, palette, widths);
    }
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
    widths: Widths,
) {
    let world = rows.world;
    if world.state == PsWorldState::Stopped {
        out.push_str(&palette.dim("○ "));
        out.push_str(&palette.dim(&pad(display_name(world), widths.header_name)));
        out.push_str("  ");
        out.push_str(&palette.dim(&pad(
            &display_path(&world.path, options.home.as_deref()),
            widths.header_path,
        )));
        out.push_str("  ");
        out.push_str(&palette.dim("stopped"));
        out.push('\n');
    } else {
        out.push_str(&palette.tint("●", "32"));
        out.push(' ');
        out.push_str(&palette.value(&pad(display_name(world), widths.header_name)));
        out.push_str("  ");
        out.push_str(&palette.path(&display_path(&world.path, options.home.as_deref())));
        if options.verbose
            && let Some(pid) = world.supervisor_pid
        {
            out.push_str(&palette.dim(&format!("  pid {pid}")));
        }
        out.push('\n');
    }

    if let Some(problem) = &world.problem {
        out.push_str(&palette.error(&format!("! {problem}")));
        out.push('\n');
    }
    for fork in &world.stopped_forks {
        out.push_str(&palette.dim("○ "));
        out.push_str(&palette.dim(&pad(&fork.name, widths.header_name)));
        out.push_str("  ");
        out.push_str(&palette.dim(&pad(
            &display_path(&fork.path, options.home.as_deref()),
            widths.header_path,
        )));
        out.push_str("  ");
        out.push_str(&palette.dim("stopped"));
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

fn render_domain(
    out: &mut String,
    domain: &super::rows::DomainRows,
    last: bool,
    palette: Palette,
    widths: Widths,
) {
    out.push_str("  ");
    out.push_str(&palette.dim(if last { "└─ " } else { "├─ " }));
    out.push_str(&palette.url(&format!("http://{}", domain.host)));
    out.push('\n');
    let branch = if last { "   " } else { "│  " };
    for (index, row) in domain.rows.iter().enumerate() {
        out.push_str("  ");
        out.push_str(&palette.dim(branch));
        let last_row = index + 1 == domain.rows.len();
        out.push_str(&palette.dim(if last_row { "└─ " } else { "├─ " }));
        render_route(out, row, palette, widths);
    }
}

fn render_route(out: &mut String, row: &RoutedRow, palette: Palette, widths: Widths) {
    out.push_str(&pad(&palette.tint(&row.path, "1;33"), widths.path));
    out.push_str("  ");
    out.push_str(&palette.dim("->"));
    out.push(' ');
    out.push_str(&pad(&palette.value(&row.target), widths.target));
    out.push_str("  ");
    if widths.kind > 0 {
        match &row.kind {
            Some(kind) => out.push_str(&pad(&palette.kind(kind), widths.kind)),
            None => out.push_str(&" ".repeat(widths.kind)),
        }
        out.push(' ');
    }
    out.push_str(&palette.status(row.state.word));
    out.push('\n');
}

fn render_support(out: &mut String, rows: &[SupportRow], palette: Palette, widths: Widths) {
    out.push_str("  ");
    out.push_str(&palette.dim("└─ "));
    out.push_str(&palette.dim("support"));
    out.push('\n');
    for (index, row) in rows.iter().enumerate() {
        out.push_str("     ");
        out.push_str(&palette.dim(if index + 1 == rows.len() {
            "└─ "
        } else {
            "├─ "
        }));
        out.push_str(&pad(&row.id, widths.name));
        out.push_str("   ");
        out.push_str(&pad(&row.descriptor, widths.descriptor));
        out.push_str("  ");
        out.push_str(&palette.status(row.state.word));
        out.push('\n');
    }
}
