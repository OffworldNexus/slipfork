//! `slipfork(1)` from the same `clap` tree that parses argv.
//!
//! Packaging asks the built binary for `--manpage`; the integration test in `tests/manpage_drift.rs`
//! keeps the checked-in page byte-identical to that shipped output.

use clap::CommandFactory;

/// Renders the roff manpage for the shipped CLI.
///
/// Only `slipfork(1)` and `slipfork-lsp(1)` are installed, so the generated per-verb
/// `slipfork-<verb>(1)` cross-references are rewritten into plain `slipfork <verb>` names.
#[must_use]
pub fn render() -> String {
    let command = super::verbs::Cli::command();
    let mut buffer = Vec::new();
    clap_mangen::Man::new(command)
        .render(&mut buffer)
        .expect("the clap tree renders a roff manpage");
    let roff = String::from_utf8(buffer).expect("roff manpages are UTF-8");
    without_dangling_pages(&roff)
}

/// Replaces `slipfork\-<verb>(1)` entries with `\fBslipfork <verb>\fR`.
fn without_dangling_pages(roff: &str) -> String {
    let mut out = String::with_capacity(roff.len());
    for line in roff.lines() {
        match line
            .strip_prefix("slipfork\\-")
            .and_then(|rest| rest.strip_suffix("(1)"))
        {
            Some(verb) => {
                out.push_str("\\fBslipfork ");
                out.push_str(verb);
                out.push_str("\\fR");
            }
            None => out.push_str(line),
        }
        out.push('\n');
    }
    out
}
