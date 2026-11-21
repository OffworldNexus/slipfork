//! The `Forkfile` language front end: lexer, parser, AST, caret diagnostics,
//! and a comment-preserving canonical formatter (`format` / `format_check`).

/// Implements span-insensitive structural equality for an AST struct.
///
/// Spans are source metadata; two nodes are equal when their syntax matches.
macro_rules! ast_eq {
    ($type:ident { $($field:ident),+ $(,)? }) => {
        impl PartialEq for $type {
            fn eq(&self, other: &Self) -> bool {
                true $(&& self.$field == other.$field)+
            }
        }
    };
}

pub mod ast;
mod decls;
pub mod diag;
mod diff;
mod expr;
mod expr_parse;
pub mod fmt;
mod image;
mod lexer;
mod parser;
mod resources;
mod service;
mod span;
mod token;

pub use ast::*;
pub use diag::{Diagnostic, Diagnostics};
pub use expr::*;
pub use fmt::FormatCheck;
pub use span::{Position, Span};

/// Parses a `Forkfile`, returning every diagnostic when it is invalid.
pub fn parse(source: &str) -> Result<Forkfile, Diagnostics> {
    parser::parse(source)
}

/// Renders a parsed document canonically.
#[must_use]
pub fn format(doc: &Forkfile) -> String {
    fmt::format(doc)
}

/// Parses and renders a source document, returning the canonical text plus a
/// line diff (empty when the source is already canonical).
pub fn format_check(source: &str) -> Result<FormatCheck, Diagnostics> {
    let document = parse(source)?;
    let formatted = fmt::format(&document);
    let diff = diff::line_diff(source, &formatted);
    Ok(FormatCheck { formatted, diff })
}

#[cfg(test)]
mod tests;
