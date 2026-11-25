//! Exit-code settlement and the one non-report stdout writer.
//!
//! Every CLI write funnels through [`settle`]: a reader that stops early (`| head`) closes the
//! pipe cleanly, every other write error prints one `failed to write output:` block and fails.

use std::io::{self, Write};
use std::process::ExitCode;

use super::Palette;

use super::failure::Failure;
use super::model::{Entry, Report, Section, StreamKind};
use super::render::ReportRenderer;

impl ReportRenderer {
    /// Renders one report and maps a closed pipe to a clean success.
    pub fn emit_exit(&self, report: &Report) -> ExitCode {
        settle(self.emit(report))
    }

    /// Writes one raw chunk to the named stream and maps a closed pipe to a clean success.
    pub fn emit_text_exit(&self, stream: StreamKind, text: &str) -> ExitCode {
        settle(self.emit_text(stream, text))
    }

    /// Writes one `--json` object and maps a closed pipe to a clean success.
    pub fn json_exit(&self, value: &impl serde::Serialize) -> ExitCode {
        settle(self.emit_json(value))
    }

    /// Writes already-rendered stdout lines as one section of notices, then settles the write.
    pub fn emit_lines_exit(&self, lines: impl IntoIterator<Item = String>) -> ExitCode {
        let report = Report {
            sections: vec![Section {
                header: None,
                entries: lines
                    .into_iter()
                    .map(|text| Entry::Notice {
                        stream: StreamKind::Stdout,
                        text,
                    })
                    .collect(),
            }],
            verdict: None,
            remedy: None,
        };
        let mut out = io::stdout().lock();
        let mut err = io::stderr().lock();
        let rendered = self.emit_to(&report, &mut out, &mut err);
        settle(
            rendered
                .and_then(|()| out.flush())
                .and_then(|()| err.flush()),
        )
    }
}

/// Writes one already-rendered chunk to stdout; a reader that stops early exits cleanly.
///
/// This is the one non-report stdout writer left for the legacy streaming verbs; the report
/// layer owns it so `commands` never owns a direct write.
pub fn emit_stdout(text: &str) -> ExitCode {
    let mut stdout = io::stdout().lock();
    settle(
        stdout
            .write_all(text.as_bytes())
            .and_then(|()| stdout.flush()),
    )
}

/// Maps one write result to the shipped exit code.
pub(crate) fn settle(result: io::Result<()>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(error) => write_failure(&error),
    }
}

/// One stderr diagnostic when stdout itself is unwritable.
fn write_failure(error: &io::Error) -> ExitCode {
    for line in Failure::new(
        "write",
        format!("failed to write output: {error}"),
        "check that stdout is writable",
    )
    .human(Palette::new(false), false)
    {
        eprintln!("{line}");
    }
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::process::ExitCode;

    use super::settle;

    #[test]
    fn settle_maps_ok_and_broken_pipe_to_success() {
        assert_eq!(settle(Ok(())), ExitCode::SUCCESS);
        assert_eq!(
            settle(Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))),
            ExitCode::SUCCESS
        );
    }
}
