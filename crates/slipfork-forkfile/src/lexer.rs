//! Byte-offset-preserving lexer for the `Forkfile` grammar.
//!
//! The lexer is deliberately permissive: any character it does not recognise becomes
//! [`Tok::Other`] so that raw command lines (`run`, hooks, tasks) can be sliced verbatim from
//! the source by the parser instead of being forced through the expression token set.

use crate::diag::{Diagnostic, LineIndex};

/// One lexical token.
#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    /// An identifier (`api`, `pre-deploy`, `app_db`).
    Ident(String),
    /// A double-quoted string with escapes already decoded.
    Str(String),
    /// A single-quoted literal (never interpolated).
    RawStr(String),
    /// A decimal integer literal.
    Int(i64),
    /// A `$`-prefixed reference such as `$port`, `$env.KEY`, or `$app_db.url`.
    Dollar(String),
    /// A relative or absolute path fragment (`./api`, `/api`).
    Path(String),
    /// `{`
    LBrace,
    /// `}`
    RBrace,
    /// `[`
    LBracket,
    /// `]`
    RBracket,
    /// `(`
    LParen,
    /// `)`
    RParen,
    /// `,`
    Comma,
    /// `:`
    Colon,
    /// `;`
    Semi,
    /// `=`
    Equals,
    /// `.`
    Dot,
    /// `+`
    Plus,
    /// `&`
    Amp,
    /// `|`
    Pipe,
    /// `!`
    Bang,
    /// `??`
    Nullish,
    /// `*`
    Star,
    /// A statement separator.
    Newline,
    /// End of input.
    Eof,
    /// Anything else (only meaningful inside raw command text).
    Other(char),
}

/// A token together with its byte range in the source.
#[derive(Debug, Clone)]
pub struct Token {
    /// The token kind.
    pub tok: Tok,
    /// Byte offset of the token start.
    pub offset: usize,
    /// Byte length of the token.
    pub len: usize,
}

fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn is_ident_body(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'
}

fn char_at(source: &str, offset: usize) -> Option<(char, usize)> {
    source
        .get(offset..)
        .and_then(|rest| rest.chars().next())
        .map(|ch| (ch, ch.len_utf8()))
}

/// Lex `source` into tokens, the line index, and any lexical diagnostics.
pub fn lex(source: &str) -> (Vec<Token>, LineIndex, Vec<Diagnostic>) {
    let index = LineIndex::new(source);
    let bytes = source.as_bytes();
    let mut tokens: Vec<Token> = Vec::new();
    let mut diags: Vec<Diagnostic> = Vec::new();

    macro_rules! push {
        ($tok:expr, $start:expr, $end:expr) => {
            tokens.push(Token {
                tok: $tok,
                offset: $start,
                len: $end - $start,
            })
        };
    }

    let mut i = 0usize;
    while i < bytes.len() {
        let start = i;
        match bytes[i] {
            b'\n' => {
                i += 1;
                push!(Tok::Newline, start, i);
            }
            b'\r' | b' ' | b'\t' => i += 1,
            b'#' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'"' => {
                i += 1;
                let mut value = String::new();
                let mut closed = false;
                while let Some((ch, width)) = char_at(source, i) {
                    match ch {
                        '"' => {
                            i += 1;
                            closed = true;
                            break;
                        }
                        '\n' => break,
                        '\\' => {
                            i += 1;
                            if let Some((escaped, escaped_width)) = char_at(source, i) {
                                value.push(escaped);
                                i += escaped_width;
                            }
                        }
                        other => {
                            value.push(other);
                            i += width;
                        }
                    }
                }
                if !closed {
                    diags.push(Diagnostic::new(
                        index.span(source, start, i - start),
                        "unterminated string literal",
                    ));
                }
                push!(Tok::Str(value), start, i);
            }
            b'\'' => {
                i += 1;
                let literal_start = i;
                while i < bytes.len() && bytes[i] != b'\'' && bytes[i] != b'\n' {
                    i += 1;
                }
                if i >= bytes.len() || bytes[i] != b'\'' {
                    diags.push(Diagnostic::new(
                        index.span(source, literal_start, i - literal_start),
                        "unterminated `'...'` literal",
                    ));
                }
                let value = source.get(literal_start..i).unwrap_or_default().to_owned();
                if i < bytes.len() && bytes[i] == b'\'' {
                    i += 1;
                }
                push!(Tok::RawStr(value), start, i);
            }
            byte if byte.is_ascii_digit() => {
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                let value = source
                    .get(start..i)
                    .and_then(|digits| digits.parse::<i64>().ok())
                    .unwrap_or(i64::MAX);
                push!(Tok::Int(value), start, i);
            }
            byte if is_ident_start(byte) => {
                i += 1;
                while i < bytes.len() && is_ident_body(bytes[i]) {
                    i += 1;
                }
                push!(Tok::Ident(source[start..i].to_owned()), start, i);
            }
            b'$' => {
                i += 1;
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'.')
                {
                    i += 1;
                }
                push!(Tok::Dollar(source[start + 1..i].to_owned()), start, i);
            }
            b'.' if bytes.get(i + 1) == Some(&b'/') => {
                i += 2;
                while i < bytes.len()
                    && matches!(
                        bytes[i],
                        b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'.' | b'/' | b'-' | b'*'
                    )
                {
                    i += 1;
                }
                push!(Tok::Path(source[start..i].to_owned()), start, i);
            }
            b'{' => {
                i += 1;
                push!(Tok::LBrace, start, i);
            }
            b'}' => {
                i += 1;
                push!(Tok::RBrace, start, i);
            }
            b'[' => {
                i += 1;
                push!(Tok::LBracket, start, i);
            }
            b']' => {
                i += 1;
                push!(Tok::RBracket, start, i);
            }
            b'(' => {
                i += 1;
                push!(Tok::LParen, start, i);
            }
            b')' => {
                i += 1;
                push!(Tok::RParen, start, i);
            }
            b',' => {
                i += 1;
                push!(Tok::Comma, start, i);
            }
            b':' => {
                i += 1;
                push!(Tok::Colon, start, i);
            }
            b';' => {
                i += 1;
                push!(Tok::Semi, start, i);
            }
            b'=' => {
                i += 1;
                push!(Tok::Equals, start, i);
            }
            b'.' => {
                i += 1;
                push!(Tok::Dot, start, i);
            }
            b'+' => {
                i += 1;
                push!(Tok::Plus, start, i);
            }
            b'&' => {
                i += 1;
                push!(Tok::Amp, start, i);
            }
            b'|' => {
                i += 1;
                push!(Tok::Pipe, start, i);
            }
            b'!' => {
                i += 1;
                push!(Tok::Bang, start, i);
            }
            b'*' => {
                i += 1;
                push!(Tok::Star, start, i);
            }
            b'?' if bytes.get(i + 1) == Some(&b'?') => {
                i += 2;
                push!(Tok::Nullish, start, i);
            }
            _ => match char_at(source, i) {
                Some((ch, width)) => {
                    i += width;
                    push!(Tok::Other(ch), start, i);
                }
                None => break,
            },
        }
    }

    push!(Tok::Eof, source.len(), source.len());
    (tokens, index, diags)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dollar_references_and_dotted_paths_stay_distinct() {
        let (tokens, _, diags) = lex("run uvicorn --port $port --url $app_db.url app.urls\n");
        assert!(diags.is_empty());
        let kinds: Vec<&Tok> = tokens.iter().map(|token| &token.tok).collect();
        assert!(kinds.contains(&&Tok::Dollar("port".to_owned())));
        assert!(kinds.contains(&&Tok::Dollar("app_db.url".to_owned())));
        assert!(kinds.contains(&&Tok::Dot));
        assert!(kinds.contains(&&Tok::Ident("app".to_owned())));
    }

    #[test]
    fn comments_do_not_produce_tokens() {
        let (tokens, _, diags) = lex("# trailing note\nrun echo hi\n");
        assert!(diags.is_empty());
        assert_eq!(tokens[0].tok, Tok::Newline);
        assert!(
            tokens
                .iter()
                .any(|token| token.tok == Tok::Ident("run".to_owned()))
        );
    }
}
