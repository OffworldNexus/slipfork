//! Whitespace, comment, and authored-blank-line handling for the parser.

use super::Parser;
use crate::ast::{Block, Comment};
use crate::diag::Diagnostic;
use crate::token::TokenKind;

/// Authored blank-line tracker: two consecutive line breaks make one blank.
pub(crate) struct Gap {
    newlines: u32,
    pending: bool,
}

impl Gap {
    pub(crate) fn new() -> Self {
        Self {
            newlines: 0,
            pending: false,
        }
    }

    /// Records one consumed line break.
    pub(crate) fn break_line(&mut self) {
        self.newlines += 1;
        if self.newlines >= 2 {
            self.pending = true;
        }
    }

    /// Takes the gap before the next element and starts a fresh run.
    pub(crate) fn take_pending(&mut self) -> bool {
        self.newlines = 0;
        std::mem::take(&mut self.pending)
    }

    /// Drops any gap that belongs to the current element.
    pub(crate) fn reset(&mut self) {
        self.newlines = 0;
        self.pending = false;
    }
}

impl Parser<'_> {
    /// Consumes line breaks and comments, recording one authored blank per gap.
    pub(super) fn skip_trivia(
        &mut self,
        comments: &mut Vec<(String, bool)>,
    ) -> Result<(), Diagnostic> {
        self.blank_pending = false;
        let mut gap = Gap::new();
        loop {
            self.fill()?;
            let token = self.peeked.take().expect("token filled");
            if matches!(&token.kind, TokenKind::Newline) {
                self.after_newline = true;
                gap.break_line();
                continue;
            }
            if matches!(&token.kind, TokenKind::Comment(_)) {
                let blank_before = gap.take_pending();
                if let TokenKind::Comment(text) = token.kind {
                    comments.push((text, blank_before));
                }
                continue;
            }
            self.blank_pending = gap.pending;
            self.peeked = Some(token);
            return Ok(());
        }
    }

    /// Appends pending leading comments and their authored blank flags.
    pub(crate) fn flush_comments<T>(block: &mut Block<T>, pending: &mut Vec<(String, bool)>) {
        let index = block.items.len();
        for (text, blank_before) in pending.drain(..) {
            block.blank_lines.push(blank_before);
            block.comments.push(Comment {
                index,
                text,
                trailing: false,
            });
        }
    }

    /// Consumes trailing comments on the item's line and requires a line end.
    pub(crate) fn finish_item<T>(&mut self, block: &mut Block<T>) -> Result<(), Diagnostic> {
        loop {
            self.fill()?;
            if matches!(
                &self.peeked.as_ref().expect("token filled").kind,
                TokenKind::Comment(_)
            ) {
                let token = self.peeked.take().expect("token filled");
                if let TokenKind::Comment(text) = token.kind {
                    block.comments.push(Comment {
                        index: block.items.len().saturating_sub(1),
                        text,
                        trailing: true,
                    });
                }
                continue;
            }
            let token = self.peeked.as_ref().expect("token filled");
            return match &token.kind {
                TokenKind::Newline | TokenKind::RBrace | TokenKind::Eof => Ok(()),
                other => Err(Diagnostic::new(
                    token.span,
                    format!("unexpected {}", other.describe()),
                    "end the declaration with a line break",
                )),
            };
        }
    }
}
