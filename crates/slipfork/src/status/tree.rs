//! The flat, domain-first status tree.
//!
//! Worlds and forks are always top-level headers; each domain lists its mapped sub-paths, and every
//! unrouted service, resource, or orphaned pod lands in one trailing `support` group. Ports and
//! ages stay in `--json` and never reach a human line.

use std::path::Path;

use serde_json::Value;

use crate::ps::{PsReply, PsWorld};
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

/// Keeps only the world or fork whose project contains `cwd`; forks are searched first.
#[must_use]
pub fn scope(reply: &PsReply, cwd: &Path) -> PsReply {
    let mut best: Option<(usize, &PsWorld)> = None;
    for world in &reply.worlds {
        consider(world, cwd, &mut best);
    }
    match best {
        Some((_, world)) => PsReply {
            worlds: vec![world.clone()],
        },
        None => PsReply::default(),
    }
}

fn flatten<'a>(worlds: &'a [PsWorld], out: &mut Vec<&'a PsWorld>) {
    for world in worlds {
        out.push(world);
        flatten(&world.forks, out);
    }
}

fn consider<'a>(world: &'a PsWorld, cwd: &Path, best: &mut Option<(usize, &'a PsWorld)>) {
    if cwd.starts_with(&world.path) {
        let score = world.path.as_os_str().len();
        if best.as_ref().is_none_or(|(previous, _)| score >= *previous) {
            *best = Some((score, world));
        }
    }
    for fork in &world.forks {
        consider(fork, cwd, best);
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
    out.push_str(&palette.tint("●", "32"));
    out.push(' ');
    out.push_str(&palette.value(display_name(world)));
    out.push_str("  ");
    out.push_str(&palette.path(&display_path(&world.path, options.home.as_deref())));
    if options.verbose {
        out.push_str(&palette.dim(&format!("  pid {}", world.supervisor_pid)));
    }
    out.push('\n');

    if let Some(problem) = &world.problem {
        out.push_str(&palette.error(&format!("! {problem}")));
        out.push('\n');
    }
    for fork in &world.stopped_forks {
        out.push_str(&palette.dim("○ "));
        out.push_str(&palette.dim(&pad(&fork.name, widths.stopped_name)));
        out.push_str("  ");
        out.push_str(&palette.dim(&pad(
            &display_path(&fork.path, options.home.as_deref()),
            widths.stopped_path,
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
