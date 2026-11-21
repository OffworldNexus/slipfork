//! The tree itself: world headers, service rows, nested forks, and scoped selection.

use std::path::Path;

use serde_json::Value;

use crate::ps::{PsReply, PsWorld};
use crate::render::paint;

use super::model::{Options, display_name, display_path, human_age, parse as decode};
use super::rows::columns;
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

/// Renders the `ps` reply as the status tree; an empty reply is the dim `○` line.
#[must_use]
pub fn render(reply: &PsReply, options: &Options) -> String {
    if reply.worlds.is_empty() {
        return format!("{}\n", paint("○ No worlds running", "2", options.color));
    }
    let widths = Widths::measure(&reply.worlds);
    let mut out = String::new();
    for (index, world) in reply.worlds.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        render_world(&mut out, world, options, widths, 0, true);
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
    world: &PsWorld,
    options: &Options,
    widths: Widths,
    depth: usize,
    last_fork: bool,
) {
    if depth > 0 {
        out.push_str(&" ".repeat(depth * 6 - 1));
        out.push_str(if last_fork { "└─ " } else { "├─ " });
    }
    out.push_str(&paint("●", "32", options.color));
    out.push(' ');
    out.push_str(display_name(world));
    out.push_str("  ");
    out.push_str(&paint(&world.url, "1;36", options.color));
    out.push_str("  ");
    out.push_str(&paint(
        &display_path(&world.path, options.home.as_deref()),
        "2",
        options.color,
    ));
    if options.verbose {
        out.push_str(&paint(
            &format!("  pid {}", world.supervisor_pid),
            "2",
            options.color,
        ));
    }
    out.push('\n');

    let indent = " ".repeat(depth * 6 + 2);
    let forks_follow = !world.forks.is_empty();
    let started = (world.started_at_ms > 0).then_some(world.started_at_ms);
    for (index, service) in world.services.iter().enumerate() {
        let last = !forks_follow && index + 1 == world.services.len();
        render_service(
            out,
            service,
            world.booting,
            started,
            options,
            widths,
            &indent,
            last,
        );
    }
    if forks_follow {
        out.push_str(&format!("{indent}└─ forks\n"));
        for (index, fork) in world.forks.iter().enumerate() {
            render_world(
                out,
                fork,
                options,
                widths,
                depth + 1,
                index + 1 == world.forks.len(),
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_service(
    out: &mut String,
    service: &crate::ps::PsService,
    booting: bool,
    started: Option<u64>,
    options: &Options,
    widths: Widths,
    indent: &str,
    last: bool,
) {
    let columns = columns(service, booting);
    out.push_str(indent);
    out.push_str(if last { "└─ " } else { "├─ " });
    out.push_str(&pad(&service.id, widths.name));
    out.push(' ');
    let middle = match columns.state {
        Some(code) => paint(&columns.middle, code, options.color),
        None => columns.middle.clone(),
    };
    out.push_str(&pad(&middle, widths.middle));
    out.push(' ');
    out.push_str(&columns.target);
    if let Some(started) = started {
        let gap = widths
            .target
            .saturating_sub(super::widths::visible_width(&columns.target))
            + 2;
        out.push_str(&" ".repeat(gap));
        out.push_str(&paint(
            &human_age(started, options.now_ms),
            "2",
            options.color,
        ));
    }
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::{parse, render, scope};
    use crate::ps::PsReply;
    use serde_json::json;
    use std::path::Path;

    #[test]
    fn empty_state_is_the_dim_dot() {
        let reply = PsReply::default();
        let text = render(&reply, &super::super::Options::new(false));
        assert_eq!(text, "○ No worlds running\n");
    }

    #[test]
    fn malformed_payloads_are_errors() {
        let error = parse(&json!({ "worlds": "nope" })).unwrap_err();
        assert!(!error.is_empty());
    }

    #[test]
    fn a_long_fork_name_widens_its_column() {
        let long = "feature-with-a-very-long-name-0123456789";
        let reply = parse(&json!({
            "worlds": [{
                "project": "/home/me/demo",
                "path": "/home/me/demo",
                "world": "demo",
                "url": "http://demo.localhost",
                "supervisor_pid": 1,
                "alive": true,
                "booting": false,
                "services": [{ "id": "api", "kind": "service", "running": true, "port": 8000 }],
                "forks": [{
                    "project": format!("/home/me/demo/.forks/{long}"),
                    "path": format!("/home/me/demo/.forks/{long}"),
                    "world": format!("{long}--demo"),
                    "fork": long,
                    "url": format!("http://{long}--demo.localhost"),
                    "supervisor_pid": 2,
                    "alive": true,
                    "booting": false,
                    "services": [{ "id": "api", "kind": "service", "running": true, "port": 8000 }]
                }]
            }]
        }))
        .expect("valid payload");
        let text = render(
            &reply,
            &super::super::Options::new(false).with_home(Some("/home/me".into())),
        );
        assert!(text.contains(long), "{text}");
    }

    #[test]
    fn scope_promotes_the_containing_fork() {
        let reply = parse(&json!({
            "worlds": [{
                "project": "/home/me/demo",
                "path": "/home/me/demo",
                "world": "demo",
                "url": "http://demo.localhost",
                "supervisor_pid": 1,
                "alive": true,
                "booting": false,
                "services": [],
                "forks": [{
                    "project": "/home/me/demo/.forks/feat-auth",
                    "path": "/home/me/demo/.forks/feat-auth",
                    "world": "feat-auth--demo",
                    "fork": "feat-auth",
                    "url": "http://feat-auth--demo.localhost",
                    "supervisor_pid": 2,
                    "alive": true,
                    "booting": false,
                    "services": []
                }]
            }]
        }))
        .expect("valid payload");
        let scoped = scope(&reply, Path::new("/home/me/demo/.forks/feat-auth/src"));
        assert_eq!(scoped.worlds.len(), 1);
        assert_eq!(scoped.worlds[0].world, "feat-auth--demo");
    }
}
