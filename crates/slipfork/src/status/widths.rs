//! Dynamic visible column widths, measured after stripping ANSI.

use std::path::Path;

use super::model::{display_name, display_path};
use super::rows::WorldRows;

/// Column widths shared by every flattened world in one render.
#[derive(Debug, Clone, Copy, Default)]
pub struct Widths {
    /// Routed sub-path column.
    pub path: usize,
    /// Route target column.
    pub target: usize,
    /// S3/CDN kind tag column; `0` when no row carries a tag.
    pub kind: usize,
    /// Support id column.
    pub name: usize,
    /// Support descriptor column.
    pub descriptor: usize,
    /// Header name column, shared by every `●` and `○` header.
    pub header_name: usize,
    /// Header path column, shared by every `●` and `○` header.
    pub header_path: usize,
}

impl Widths {
    /// Measures every routed, support, and header row of every world and fork.
    #[must_use]
    pub fn measure(worlds: &[WorldRows<'_>], home: Option<&Path>) -> Self {
        let mut widths = Self::default();
        for rows in worlds {
            for domain in &rows.domains {
                for row in &domain.rows {
                    widths.path = widths.path.max(visible_width(&row.path));
                    widths.target = widths.target.max(visible_width(&row.target));
                    if let Some(kind) = &row.kind {
                        widths.kind = widths.kind.max(visible_width(kind));
                    }
                }
            }
            for row in &rows.support {
                widths.name = widths.name.max(visible_width(&row.id));
                widths.descriptor = widths.descriptor.max(visible_width(&row.descriptor));
            }
            let world = rows.world;
            widths.header_name = widths.header_name.max(visible_width(display_name(world)));
            widths.header_path = widths
                .header_path
                .max(visible_width(&display_path(&world.path, home)));
            for fork in &world.stopped_forks {
                widths.header_name = widths.header_name.max(visible_width(&fork.name));
                widths.header_path = widths
                    .header_path
                    .max(visible_width(&display_path(&fork.path, home)));
            }
        }
        widths
    }
}

/// Pads `text` on the right to `width` visible characters.
#[must_use]
pub fn pad(text: &str, width: usize) -> String {
    let visible = visible_width(text);
    let mut padded = text.to_owned();
    for _ in visible..width {
        padded.push(' ');
    }
    padded
}

/// Character count with every `ESC[ … m` sequence excluded.
#[must_use]
pub fn visible_width(text: &str) -> usize {
    console::measure_text_width(text)
}
