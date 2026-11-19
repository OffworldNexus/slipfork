//! Canonical `Forkfile` formatter: one deterministic, idempotent byte transform.
//!
//! `fmt` never re-derives grammar. It asks the canonical parser/checker for a clean manifest and
//! re-indents the original bytes from the canonical lexer's brace tokens: comment text and literal
//! spelling survive, while indentation is canonicalized.

use crate::diag::Diagnostic;
use crate::lexer::{Tok, lex};
use crate::parser::parse_forkfile;

/// Four spaces per nesting level, matching every hand-written `Forkfile` in the repository.
const INDENT: &str = "    ";

/// Format `source` into canonical `Forkfile` text.
///
/// An invalid manifest returns the checker's multi-error [`Diagnostic`] vector unchanged. A valid
/// manifest is re-indented to four spaces per level, stripped of trailing whitespace, limited to
/// one blank line between statements, and terminated by exactly one newline. The transform is
/// idempotent: `fmt(&fmt(source)?)?` equals `fmt(source)?`.
pub fn fmt(source: &str) -> Result<String, Vec<Diagnostic>> {
    parse_forkfile(source)?;
    Ok(reindent(source))
}

fn reindent(source: &str) -> String {
    let lines = source.split('\n').count().max(1);
    let mut opens = vec![0usize; lines + 2];
    let mut closes = vec![0usize; lines + 2];
    let mut seen = vec![false; lines + 2];
    let mut first_closes = vec![false; lines + 2];
    let (tokens, index, _) = lex(source);
    for token in &tokens {
        if matches!(token.tok, Tok::Eof) {
            continue;
        }
        let line = index.span(source, token.offset, token.len).line as usize;
        if line == 0 || line > lines {
            continue;
        }
        if !seen[line] {
            seen[line] = true;
            first_closes[line] = matches!(token.tok, Tok::RBrace);
        }
        match token.tok {
            Tok::LBrace => opens[line] += 1,
            Tok::RBrace => closes[line] += 1,
            _ => {}
        }
    }
    let mut depth = 0usize;
    let mut out: Vec<String> = Vec::new();
    for (offset, raw) in source.split('\n').enumerate() {
        let line = offset + 1;
        let trimmed = raw.trim_end_matches('\r').trim();
        if trimmed.is_empty() {
            if out.last().is_some_and(|previous| !previous.is_empty()) {
                out.push(String::new());
            }
            continue;
        }
        let dedent = usize::from(first_closes.get(line).copied().unwrap_or(false));
        out.push(format!(
            "{}{trimmed}",
            INDENT.repeat(depth.saturating_sub(dedent))
        ));
        depth = (depth + opens.get(line).copied().unwrap_or(0))
            .saturating_sub(closes.get(line).copied().unwrap_or(0));
    }
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    let mut text = out.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatting_reindents_and_is_idempotent() {
        let source = "service   api ./api {\n      port 3000\n\n\n  run cargo run   \n}\n";
        let once = fmt(source).expect("valid Forkfile");
        assert_eq!(
            once,
            "service   api ./api {\n    port 3000\n\n    run cargo run\n}\n"
        );
        assert_eq!(fmt(&once).expect("valid again"), once);
    }

    #[test]
    fn comments_and_literals_survive_verbatim() {
        let source = "features {\n # keep me\n default = [local]\n local\n}\n";
        assert_eq!(
            fmt(source).expect("valid Forkfile"),
            "features {\n    # keep me\n    default = [local]\n    local\n}\n"
        );
    }

    #[test]
    fn nested_on_blocks_with_trailing_comments_are_idempotent() {
        let source = "features {\n  default = [local]\n  local\n}\n\
                      on local {\n\
                          service api ./api {\n\
                              port 3000 # keep me\n\
                                on local {\n\
                          run cargo run\n\
                                }\n\
                        }\n\
                      }\n";
        let once = fmt(source).expect("valid Forkfile");
        assert!(once.contains("port 3000 # keep me"));
        assert!(once.contains("\n        on local {"));
        assert_eq!(fmt(&once).expect("valid again"), once);
    }

    #[test]
    fn invalid_manifests_return_the_checker_diagnostics() {
        let diagnostics =
            fmt("postgres pg {\n    database app_db\n}\nenv c {\n    U = pg.url\n}\n")
                .expect_err("`pg` is a cluster, not a database");
        assert!(!diagnostics.is_empty());
        assert!(diagnostics[0].message.contains("postgres cluster"));
    }
}
