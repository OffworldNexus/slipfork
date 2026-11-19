//! The one `WORLD  STATE  ORIGIN  WORKTREE` table of `status` and `list`.

use std::collections::BTreeSet;

use slipfork_wire::control::WorldSummary;

use super::{next_hint, render_lineage, world_state_label};
use crate::ui::{TerminalUi, column_width};

/// Narrowest world-id column; a longer id widens the column instead of shifting the rest.
const MIN_WORLD_COLUMN: usize = 16;

/// Narrowest origin column; the `ORIGIN` header is the floor.
const MIN_ORIGIN_COLUMN: usize = 6;

/// Width of the one `STATE` column (`running` is the widest lifecycle label).
const STATE_COLUMN: usize = 9;

/// Indentation of one canonical project row (a fork row adds two more spaces).
const PROJECT_INDENT: &str = "  ";

/// Indentation of one fork row under its project.
const FORK_INDENT: &str = "    ";

/// The one `WORLD  STATE  ORIGIN  WORKTREE` table of `status` and `list`.
///
/// Column widths come from the payload's real content with ANSI stripped, so a long world id or
/// origin widens its column instead of shearing the row; `indent` is the tree indentation (`""` for
/// the flat `list` table, the fork indent on the dashboard).
pub struct WorldTable<'a> {
    worlds: &'a [WorldSummary],
    ui: &'a TerminalUi,
}

impl<'a> WorldTable<'a> {
    /// Bind the payload and the one palette its rows are painted through.
    #[must_use]
    pub fn new(worlds: &'a [WorldSummary], ui: &'a TerminalUi) -> Self {
        Self { worlds, ui }
    }

    /// The world-id column width: the widest id, never narrower than `MIN_WORLD_COLUMN`.
    #[must_use]
    pub fn world_column(&self) -> usize {
        column_width(
            MIN_WORLD_COLUMN,
            self.worlds.iter().map(|world| world.world.as_str()),
        )
    }

    /// The origin column width: the widest rendered URL, never narrower than `ORIGIN`.
    fn origin_column(&self) -> usize {
        let urls: Vec<String> = self
            .worlds
            .iter()
            .map(|world| self.ui.url(world.origin.url()))
            .collect();
        column_width(MIN_ORIGIN_COLUMN, urls.iter().map(String::as_str))
    }

    /// The `list` header row.
    #[must_use]
    pub fn header(&self) -> String {
        format!(
            "{} {} {} WORKTREE\n",
            self.ui.pad("WORLD", self.world_column()),
            self.ui.pad("STATE", STATE_COLUMN),
            self.ui.pad("ORIGIN", self.origin_column()),
        )
    }

    /// One `WORLD  STATE  ORIGIN  WORKTREE` row at `indent`.
    #[must_use]
    pub fn row(&self, world: &WorldSummary, indent: &str) -> String {
        format!(
            "{indent}{} {} {} {}\n",
            self.ui.pad(&world.world, self.world_column()),
            self.ui
                .pad(&self.ui.state(world_state_label(world.state)), STATE_COLUMN),
            self.ui
                .pad(&self.ui.url(world.origin.url()), self.origin_column()),
            self.ui.secondary(&world.worktree.display().to_string()),
        )
    }

    /// One rendered world row plus its `lineage` and `next` sub-lines.
    pub(super) fn push_world(
        &self,
        out: &mut String,
        world: &WorldSummary,
        implemented: &BTreeSet<String>,
    ) {
        let indent = if world.parent.is_some() {
            FORK_INDENT
        } else {
            PROJECT_INDENT
        };
        out.push_str(&self.row(world, indent));
        let label_indent = " ".repeat(PROJECT_INDENT.len() + self.world_column());
        out.push_str(&format!(
            "{label_indent}lineage   {}\n",
            render_lineage(&world.lineage, self.ui)
        ));
        out.push_str(&format!(
            "{label_indent}next      {}\n",
            self.ui.secondary(&next_hint(world, implemented))
        ));
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use slipfork_wire::control::{WorldState, WorldSummary};
    use slipfork_world::{LocalHostRoute, ProjectSlug, WorldLineage, WorldOrigin, WorldTarget};

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

    fn world(id: &str, ingress_port: u16) -> WorldSummary {
        let project = ProjectSlug::parse("demo").expect("a valid project slug");
        let (parent, target) = match id.split_once(':') {
            Some((project, fork)) => (
                Some(project.to_owned()),
                WorldTarget::UserFork(fork.to_owned()),
            ),
            None => (None, WorldTarget::Canonical),
        };
        let route = LocalHostRoute::new(project, target, None, &[]).expect("a routable route");
        WorldSummary {
            world: id.to_owned(),
            parent,
            state: WorldState::Running,
            origin: WorldOrigin::local(&route, ingress_port).expect("a minted origin"),
            worktree: PathBuf::from(format!("/home/dev/{id}")),
            lineage: WorldLineage {
                rootfs: None,
                services: Vec::new(),
                seed: None,
                base: None,
            },
        }
    }

    #[test]
    fn header_widens_origin_for_a_long_url_while_world_and_state_keep_their_floors() {
        let ui = plain();
        let worlds = vec![
            world("demo", 80),
            world("demo:feat-with-a-very-long-fork-name", 40645),
        ];
        let table = WorldTable::new(&worlds, &ui);

        let widest_id = column_width(0, worlds.iter().map(|world| world.world.as_str()));
        assert_eq!(table.world_column(), widest_id);
        let urls: Vec<String> = worlds
            .iter()
            .map(|world| ui.url(world.origin.url()))
            .collect();
        let widest_origin = column_width(0, urls.iter().map(String::as_str));
        assert!(widest_origin > MIN_ORIGIN_COLUMN);
        assert_eq!(table.origin_column(), widest_origin);

        let header = table.header();
        assert_eq!(
            header.find("WORKTREE"),
            Some(table.world_column() + 1 + STATE_COLUMN + 1 + table.origin_column() + 1),
            "WORKTREE starts after the widened ORIGIN column: {header:?}"
        );
        assert!(header.starts_with("WORLD "));

        // Floors hold for an empty payload: `WORLD` and `ORIGIN` keep their minimum widths.
        let empty: Vec<WorldSummary> = Vec::new();
        let floor = WorldTable::new(&empty, &ui);
        assert_eq!(floor.world_column(), MIN_WORLD_COLUMN);
        assert_eq!(floor.origin_column(), MIN_ORIGIN_COLUMN);
        assert_eq!(
            floor.header().find("WORKTREE"),
            Some(MIN_WORLD_COLUMN + 1 + STATE_COLUMN + 1 + MIN_ORIGIN_COLUMN + 1)
        );
    }
}
