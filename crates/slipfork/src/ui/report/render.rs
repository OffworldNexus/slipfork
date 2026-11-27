//! The one report renderer: typed sections and rows to a terminal, a pipe, or `--json`.
//!
//! It owns the palette, the UI mode, the resolved terminal, and the injected wall clock, so no
//! command formats an SGR code or guesses a width. Human modes write one blank line between
//! non-empty sections and close a failed report with exactly one `next:` line on stderr.

use std::io::{self, BufWriter, Write};

use super::Palette;
use crate::ui::{Terminal, UiMode};

use super::model::{Entry, Report, StreamKind, Verdict};
use super::wall::WallZone;

/// Renders a [`Report`] through one palette, one mode, and one wall clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportRenderer {
    palette: Palette,
    mode: UiMode,
    terminal: Terminal,
    wall: WallZone,
    verbose: bool,
}

impl ReportRenderer {
    /// Builds the renderer from the composition root's terminal and wall clock.
    #[must_use]
    pub fn new(terminal: Terminal, wall: WallZone) -> Self {
        Self {
            palette: Palette::new(terminal.mode.color()),
            mode: terminal.mode,
            terminal,
            wall,
            verbose: false,
        }
    }

    /// Enables the `-v`-only notes and runtime detail.
    #[must_use]
    pub fn with_verbose(mut self, verbose: bool) -> Self {
        self.verbose = verbose;
        self
    }

    /// Whether ANSI styling is allowed.
    #[must_use]
    pub const fn color(&self) -> bool {
        self.palette.color()
    }

    /// The output mode resolved by the composition root.
    #[must_use]
    pub const fn mode(&self) -> UiMode {
        self.mode
    }

    /// The injected local wall clock.
    #[must_use]
    pub const fn wall(&self) -> &WallZone {
        &self.wall
    }

    /// The one palette every streaming surface shares.
    #[must_use]
    pub const fn palette(&self) -> Palette {
        self.palette
    }

    /// Renders one entry immediately, so a live stream never waits for a whole report.
    ///
    /// # Errors
    ///
    /// Returns the first write or flush failure.
    pub fn emit_entry(&self, entry: &Entry) -> io::Result<()> {
        let mut out = io::stdout().lock();
        let mut err = io::stderr().lock();
        self.write_entry(entry, &mut out, &mut err)?;
        out.flush()?;
        err.flush()
    }

    /// Renders one entry into the given writers, so tests share the exact stream bytes.
    ///
    /// # Errors
    ///
    /// Returns the first write failure from either writer.
    pub fn emit_entry_to(
        &self,
        entry: &Entry,
        out: &mut dyn Write,
        err: &mut dyn Write,
    ) -> io::Result<()> {
        self.write_entry(entry, out, err)
    }

    /// Writes one raw, already-rendered chunk to the named stream and flushes it.
    ///
    /// # Errors
    ///
    /// Returns the write or flush failure.
    pub fn emit_text(&self, stream: StreamKind, text: &str) -> io::Result<()> {
        match stream {
            StreamKind::Stdout => {
                let mut out = io::stdout().lock();
                out.write_all(text.as_bytes())?;
                out.flush()
            }
            StreamKind::Stderr => {
                let mut err = io::stderr().lock();
                err.write_all(text.as_bytes())?;
                err.flush()
            }
        }
    }

    /// Writes one `--json` object as a single newline-terminated line.
    ///
    /// # Errors
    ///
    /// Returns the serialization, write, or flush failure.
    pub fn emit_json(&self, value: &impl serde::Serialize) -> io::Result<()> {
        let mut out = io::stdout().lock();
        writeln!(out, "{}", serialized(value)?)?;
        out.flush()
    }

    /// Writes one typed notice to the named stream.
    ///
    /// # Errors
    ///
    /// Returns the write or flush failure.
    pub fn emit_notice(&self, stream: StreamKind, text: &str) -> io::Result<()> {
        self.emit_entry(&Entry::Notice {
            stream,
            text: text.to_owned(),
        })
    }

    /// Writes one dim, indented narration line into `out` without flushing.
    ///
    /// # Errors
    ///
    /// Returns the write failure.
    pub fn write_note(&self, indent: usize, text: &str, out: &mut dyn Write) -> io::Result<()> {
        writeln!(out, "{}{}", " ".repeat(indent), self.palette.meta(text))
    }

    /// Renders one report to the real stdout and stderr.
    ///
    /// # Errors
    ///
    /// Returns the first write or flush failure.
    pub fn emit(&self, report: &Report) -> io::Result<()> {
        let stdout = io::stdout();
        let stderr = io::stderr();
        let mut out = BufWriter::new(stdout.lock());
        let mut err = BufWriter::new(stderr.lock());
        let rendered = self.emit_to(report, &mut out, &mut err);
        let flushed_out = out.flush();
        let flushed_err = err.flush();
        rendered.and(flushed_out).and(flushed_err)
    }

    /// Renders one report into the given writers, so callers and tests share the exact bytes.
    ///
    /// # Errors
    ///
    /// Returns the first write failure from either writer.
    pub fn emit_to(
        &self,
        report: &Report,
        out: &mut dyn Write,
        err: &mut dyn Write,
    ) -> io::Result<()> {
        if self.mode.json() {
            let failure = match &report.verdict {
                Some(Verdict::Failed(failure) | Verdict::Refused(failure)) => Some(failure),
                _ => None,
            };
            if let Some(failure) = failure
                && !self.has_visible_sections(report)
            {
                writeln!(out, "{}", serialized(&failure.json())?)?;
            } else {
                writeln!(out, "{}", serialized(report)?)?;
            }
            return Ok(());
        }
        let mut wrote_section = false;
        for section in &report.sections {
            let visible: Vec<&Entry> = section
                .entries
                .iter()
                .filter(|entry| self.visible(entry))
                .collect();
            if visible.is_empty() {
                continue;
            }
            if wrote_section {
                writeln!(out)?;
            }
            wrote_section = true;
            if let Some(header) = &section.header {
                writeln!(out, "{}", self.palette.label(header))?;
            }
            for entry in visible {
                self.write_entry(entry, out, err)?;
            }
        }
        if let Some(Verdict::Ok(kind)) = &report.verdict {
            writeln!(
                out,
                "{} {}",
                self.palette.icon_ok(),
                self.palette.ok(kind.word())
            )?;
        }
        if let Some(Verdict::Failed(failure) | Verdict::Refused(failure)) = &report.verdict {
            for line in failure.human(self.palette, self.verbose) {
                writeln!(err, "{line}")?;
            }
        }
        if let Some(remedy) = &report.remedy {
            writeln!(
                out,
                "{} {}",
                self.palette.meta("next:"),
                self.palette.next(remedy)
            )?;
        }
        Ok(())
    }

    fn has_visible_sections(&self, report: &Report) -> bool {
        report
            .sections
            .iter()
            .any(|section| section.entries.iter().any(|entry| self.visible(entry)))
    }

    /// Writes one trailing blank line on stdout (clap help output ends with it).
    pub fn blank_line(&self) {
        let mut out = io::stdout().lock();
        let _ = writeln!(out);
    }

    fn visible(&self, entry: &Entry) -> bool {
        match entry {
            Entry::Note(_) => self.verbose,
            _ => true,
        }
    }

    fn write_entry(
        &self,
        entry: &Entry,
        out: &mut dyn Write,
        err: &mut dyn Write,
    ) -> io::Result<()> {
        match entry {
            Entry::Row(row) => writeln!(out, "{}", row.line(self.palette))?,
            Entry::Stream {
                indent,
                stream,
                line,
                ts_ms,
            } => {
                let indent = " ".repeat(*indent);
                let gutter = self.palette.gutter(stream.name());
                if *ts_ms == 0 {
                    writeln!(out, "{indent}{gutter} {line}")?;
                } else {
                    writeln!(
                        out,
                        "{indent}{} {gutter} {line}",
                        self.palette.meta(&self.wall.clock(*ts_ms))
                    )?;
                }
            }
            Entry::Notice { stream, text } => match stream {
                StreamKind::Stdout => writeln!(out, "{text}")?,
                StreamKind::Stderr => writeln!(err, "{text}")?,
            },
            Entry::Note(text) => writeln!(out, "{text}")?,
        }
        Ok(())
    }
}

fn serialized(value: &impl serde::Serialize) -> io::Result<String> {
    serde_json::to_string(value).map_err(io::Error::other)
}

#[cfg(test)]
mod tests;
