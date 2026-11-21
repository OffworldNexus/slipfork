//! Dynamic visible column widths, measured after stripping ANSI.

use crate::ps::PsWorld;

use super::rows::columns;

/// Column widths shared by every world and nested fork in one render.
#[derive(Debug, Clone, Copy, Default)]
pub struct Widths {
    /// Service-name column, including its gutter.
    pub name: usize,
    /// State/descriptor column.
    pub middle: usize,
    /// Port/route column.
    pub target: usize,
}

impl Widths {
    /// Measures every row of every world and fork.
    #[must_use]
    pub fn measure(worlds: &[PsWorld]) -> Self {
        let mut widths = Self::default();
        for world in worlds {
            widths.world(world);
        }
        widths
    }

    fn world(&mut self, world: &PsWorld) {
        for service in &world.services {
            let columns = columns(service, world.booting);
            self.name = self.name.max(visible_width(&service.id) + 1);
            self.middle = self.middle.max(visible_width(&columns.middle));
            self.target = self.target.max(visible_width(&columns.target));
        }
        for fork in &world.forks {
            self.world(fork);
        }
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
    let mut width = 0;
    let mut escaping = false;
    for character in text.chars() {
        if escaping {
            if character == 'm' {
                escaping = false;
            }
        } else if character == '\u{1b}' {
            escaping = true;
        } else {
            width += 1;
        }
    }
    width
}
