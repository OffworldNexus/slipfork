//! The one terminal capability snapshot and the single semantic palette (`M3.30.3.1`).
//!
//! [`TerminalUi`] is a plain value handed the facts the composition root observed once. No renderer
//! probes the terminal, reads an environment variable, or emits an escape byte on its own: the
//! `tty`/`json` conjunction is applied here, so a piped stream, `NO_COLOR=1`, and `--json` share
//! one monochrome branch.

use std::time::Duration;

use indicatif::{ProgressBar, ProgressDrawTarget};
use owo_colors::OwoColorize;

/// Width of one short lineage digest (`<digest12>`, `docs/design/dx-0.1.0-ux.md` §8.2).
pub const SHORT_DIGEST: usize = 12;

/// The terminal capability snapshot the composition root takes exactly once.
///
/// `color` is the developer's preference (`NO_COLOR` absent); [`TerminalUi::new`] additionally
/// requires a terminal and the absence of `--json` before any escape byte is emitted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// stdout is an interactive terminal.
    pub tty: bool,
    /// `NO_COLOR` did not refuse color.
    pub color: bool,
    /// `-v`: per-node lineage and `[resources]` detail.
    pub verbose: bool,
    /// `--json`: the machine-pure document.
    pub json: bool,
}

/// The one terminal capability snapshot every human renderer paints through.
///
/// Constructed once in `src/main.rs`; the spinner is the struct's own handle and stays hidden
/// whenever color is off.
pub struct TerminalUi {
    caps: Capabilities,
    spinner: ProgressBar,
}

impl TerminalUi {
    /// Bind the root's snapshot, resolving the one purity rule: color requires a terminal and is
    /// never painted for `--json`. The spinner is created only when color is on, so a TTY under
    /// `NO_COLOR=1` writes zero bytes of progress output.
    #[must_use]
    pub fn new(caps: Capabilities) -> Self {
        let color = caps.color && caps.tty && !caps.json;
        let spinner = if color {
            let spinner = ProgressBar::new_spinner();
            spinner.set_draw_target(ProgressDrawTarget::stderr());
            spinner
        } else {
            ProgressBar::hidden()
        };
        Self {
            caps: Capabilities { color, ..caps },
            spinner,
        }
    }

    /// `-v`: per-node lineage and `[resources]` detail.
    #[must_use]
    pub fn verbose(&self) -> bool {
        self.caps.verbose
    }

    /// A primary origin URL: bold cyan, with the default `:80` omitted.
    #[must_use]
    pub fn url(&self, url: &str) -> String {
        let url = without_default_port(url);
        if self.caps.color {
            url.bold().cyan().to_string()
        } else {
            url
        }
    }

    /// A `running` lifecycle word (green).
    #[must_use]
    pub fn running(&self, text: &str) -> String {
        if self.caps.color {
            text.green().to_string()
        } else {
            text.to_owned()
        }
    }

    /// A `failed` lifecycle word or verdict (red).
    #[must_use]
    pub fn failed(&self, text: &str) -> String {
        if self.caps.color {
            text.red().to_string()
        } else {
            text.to_owned()
        }
    }

    /// A passing check or `✓` mark (bold green).
    #[must_use]
    pub fn ok(&self, text: &str) -> String {
        if self.caps.color {
            text.green().bold().to_string()
        } else {
            text.to_owned()
        }
    }

    /// An advisory `!` / `skipped` / `missing` (yellow).
    #[must_use]
    pub fn warn(&self, text: &str) -> String {
        if self.caps.color {
            text.yellow().to_string()
        } else {
            text.to_owned()
        }
    }

    /// Secondary text: paths, hints, anything a first read can skip (dim on a TTY).
    #[must_use]
    pub fn secondary(&self, text: &str) -> String {
        if self.caps.color {
            text.dimmed().to_string()
        } else {
            text.to_owned()
        }
    }

    /// A lifecycle word: `running` green, `failed` red, anything else secondary.
    #[must_use]
    pub fn state(&self, label: &str) -> String {
        match label {
            "running" => self.running(label),
            "failed" => self.failed(label),
            _ => self.secondary(label),
        }
    }

    /// One short digest: its first [`SHORT_DIGEST`] characters, dimmed on a colored terminal.
    #[must_use]
    pub fn digest(&self, digest: &str) -> String {
        self.secondary(&short_digest(digest))
    }

    /// Pad `text` to `width` visible columns (escape sequences never count as text).
    #[must_use]
    pub fn pad(&self, text: &str, width: usize) -> String {
        let mut padded = String::with_capacity(text.len() + width);
        padded.push_str(text);
        padded.extend(std::iter::repeat_n(
            ' ',
            width.saturating_sub(visible_width(text)),
        ));
        padded
    }

    /// Start the one spinner on stderr; a no-op whenever color is off (piped, `NO_COLOR`, `--json`).
    pub fn start_progress(&self, message: &str) {
        if !self.caps.color {
            return;
        }
        self.spinner.set_message(message.to_owned());
        self.spinner.enable_steady_tick(Duration::from_millis(120));
        self.spinner.tick();
    }

    /// Clear the spinner before the finished report is printed.
    pub fn finish_progress(&self) {
        if self.caps.color {
            self.spinner.finish_and_clear();
        }
    }
}

/// The first [`SHORT_DIGEST`] characters of a full 64-hex digest.
#[must_use]
pub fn short_digest(digest: &str) -> String {
    digest.chars().take(SHORT_DIGEST).collect()
}

/// The visible width of `text`: CSI (SGR) sequences occupy no column.
#[must_use]
pub fn visible_width(text: &str) -> usize {
    let mut width = 0;
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch != '\x1b' {
            width += 1;
            continue;
        }
        // One CSI sequence: `ESC [` then parameters/private markers and a final byte in `@..=~`.
        if chars.next() != Some('[') {
            continue;
        }
        for ch in chars.by_ref() {
            if ('@'..='~').contains(&ch) {
                break;
            }
        }
    }
    width
}

/// Drop the default `:80` from a rendered origin (`http://demo.localhost:80` becomes
/// `http://demo.localhost`); every other port is a real port and stays.
fn without_default_port(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_owned();
    };
    let (authority, tail) = match rest.find('/') {
        Some(index) => rest.split_at(index),
        None => (rest, ""),
    };
    match authority.strip_suffix(":80") {
        Some(host) => format!("{scheme}://{host}{tail}"),
        None => url.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capabilities() -> Capabilities {
        Capabilities {
            tty: true,
            color: true,
            verbose: false,
            json: false,
        }
    }

    fn terminal() -> TerminalUi {
        TerminalUi::new(capabilities())
    }

    fn colorless() -> TerminalUi {
        TerminalUi::new(Capabilities {
            tty: false,
            color: false,
            verbose: false,
            json: false,
        })
    }

    #[test]
    fn palette_is_byte_identical_to_its_input_when_piped_no_color_or_json() {
        for caps in [
            Capabilities {
                tty: false,
                ..capabilities()
            },
            Capabilities {
                color: false,
                ..capabilities()
            },
            Capabilities {
                json: true,
                ..capabilities()
            },
            Capabilities::default(),
        ] {
            let ui = TerminalUi::new(caps);
            let painted = [
                ui.url("http://demo.localhost:80"),
                ui.running("running"),
                ui.failed("failed"),
                ui.ok("✓ Ready"),
                ui.warn("!"),
                ui.secondary("/home/dev/demo"),
                ui.digest("a1b2c3d4e5f6a7b8"),
                ui.state("stopped"),
                ui.pad("state", 10),
            ];
            for text in painted {
                assert!(
                    !text.contains('\x1b'),
                    "no escape bytes without color: {text:?}"
                );
            }
        }
    }

    #[test]
    fn palette_paints_the_exact_sgr_sequences_on_a_terminal() {
        let ui = terminal();
        assert_eq!(
            ui.secondary("a1b2c3d4e5f6"),
            "\u{1b}[2ma1b2c3d4e5f6\u{1b}[0m"
        );
        assert_eq!(
            ui.digest("a1b2c3d4e5f6a7b8"),
            "\u{1b}[2ma1b2c3d4e5f6\u{1b}[0m"
        );
        assert_eq!(ui.running("running"), "\u{1b}[32mrunning\u{1b}[39m");
        assert_eq!(ui.failed("failed"), "\u{1b}[31mfailed\u{1b}[39m");
        assert_eq!(ui.warn("!"), "\u{1b}[33m!\u{1b}[39m");
        assert_eq!(
            ui.url("http://demo.localhost:80"),
            "\u{1b}[36m\u{1b}[1mhttp://demo.localhost\u{1b}[0m\u{1b}[39m"
        );
        assert_eq!(
            ui.ok("✓ Ready"),
            "\u{1b}[1m\u{1b}[32m✓ Ready\u{1b}[39m\u{1b}[0m"
        );
        assert_eq!(visible_width(&ui.digest("a1b2c3d4e5f6a7b8")), 12);
    }

    #[test]
    fn progress_stays_hidden_whenever_color_is_off() {
        for caps in [
            Capabilities {
                tty: true,
                color: false,
                verbose: false,
                json: false,
            },
            Capabilities {
                json: true,
                ..capabilities()
            },
            Capabilities::default(),
        ] {
            let ui = TerminalUi::new(caps);
            assert!(ui.spinner.is_hidden());
            ui.start_progress("probing namespaces and :80 ingress…");
            assert!(
                ui.spinner.is_hidden(),
                "a colorless capability snapshot gets no spinner bytes"
            );
            ui.finish_progress();
        }
        // A colored snapshot drives the live spinner; its draw target hides itself off a TTY.
        let ui = terminal();
        ui.start_progress("probing namespaces and :80 ingress…");
        ui.finish_progress();
    }

    #[test]
    fn url_omits_only_the_default_port() {
        let ui = colorless();
        assert_eq!(ui.url("http://demo.localhost:80"), "http://demo.localhost");
        assert_eq!(
            ui.url("http://demo.localhost:80/"),
            "http://demo.localhost/"
        );
        assert_eq!(
            ui.url("http://demo.localhost:40645"),
            "http://demo.localhost:40645"
        );
        assert_eq!(ui.url("http://demo.localhost"), "http://demo.localhost");
        assert_eq!(
            ui.url("http://demo.localhost:8080/api"),
            "http://demo.localhost:8080/api"
        );
        assert_eq!(ui.url("not a url"), "not a url");
    }

    #[test]
    fn padding_measures_visible_columns_not_escape_bytes() {
        let ui = terminal();
        let padded = ui.pad(&ui.running("running"), 9);
        assert_eq!(visible_width(&padded), 9);
        assert_eq!(visible_width(&ui.pad("running", 9)), 9);
        assert_eq!(ui.pad("long-value", 4), "long-value");
        assert_eq!(colorless().pad("ok", 4), "ok  ");
    }

    #[test]
    fn visible_width_ignores_csi_sequences() {
        assert_eq!(visible_width("\u{1b}[1;36mdemo\u{1b}[0m"), 4);
        assert_eq!(visible_width("plain"), 5);
        assert_eq!(visible_width(""), 0);
    }
}
