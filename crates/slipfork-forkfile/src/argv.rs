//! Word-split-safe `argv` parsing for `run`, lifecycle hooks, and tasks.
//!
//! `$port`, `$env.KEY`, and `$<entity>.<prop>` resolve into exactly one argument slot, so a value
//! containing spaces is never split; single quotes are literal, double quotes allow interpolation,
//! and leading `FOO=bar` words become per-command environment prefixes.

use crate::diag::{Diagnostic, LineIndex, Span};
use crate::syntax::{ArgPart, ArgRef, ArgRefKind, Argv, ArgvSlot, EnvPrefix, Spanned};

fn flush(parts: &mut Vec<ArgPart>, buffer: &mut String) {
    if !buffer.is_empty() {
        parts.push(ArgPart::Text(std::mem::take(buffer)));
    }
}

fn reference(
    text: &str,
    base: usize,
    start: usize,
    index: &LineIndex,
    source: &str,
    diags: &mut Vec<Diagnostic>,
) -> (ArgPart, usize) {
    let bytes = text.as_bytes();
    let mut end = start + 1;
    while end < bytes.len()
        && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_' || bytes[end] == b'.')
    {
        end += 1;
    }
    let name = text.get(start + 1..end).unwrap_or_default();
    let span = index.span(source, base + start, end - start);
    if name.is_empty() {
        return (ArgPart::Text("$".to_owned()), end);
    }
    let kind = if name == "port" {
        ArgRefKind::Port
    } else if let Some(key) = name.strip_prefix("env.") {
        ArgRefKind::Env(key.to_owned())
    } else {
        let segments: Vec<String> = name.split('.').map(str::to_owned).collect();
        if segments.len() < 2 {
            diags.push(
                Diagnostic::new(span, format!("unknown `$` reference `${name}`"))
                    .with_hint("use `$port`, `$env.KEY`, or `$<entity>.<prop>`"),
            );
        }
        ArgRefKind::Property(segments)
    };
    (ArgPart::Ref(ArgRef { kind, span }), end)
}

fn is_assignment(word: &str) -> Option<(&str, &str)> {
    let (key, value) = word.split_once('=')?;
    let mut chars = key.chars();
    let first = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    if !chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
        return None;
    }
    Some((key, value))
}

/// Split one raw command line into an [`Argv`], recording unknown `$` references as diagnostics.
pub fn parse_argv(
    text: &str,
    base: usize,
    source: &str,
    index: &LineIndex,
) -> (Argv, Vec<Diagnostic>) {
    let bytes = text.as_bytes();
    let mut diags = Vec::new();
    let mut slots: Vec<ArgvSlot> = Vec::new();
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if bytes[i] == b'#' {
            break;
        }
        let slot_start = i;
        let mut parts: Vec<ArgPart> = Vec::new();
        let mut buffer = String::new();

        while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
            match bytes[i] {
                b'#' => {
                    i = bytes.len();
                    flush(&mut parts, &mut buffer);
                }
                b'\'' => {
                    flush(&mut parts, &mut buffer);
                    i += 1;
                    let literal_start = i;
                    while i < bytes.len() && bytes[i] != b'\'' {
                        i += 1;
                    }
                    if i >= bytes.len() {
                        diags.push(Diagnostic::new(
                            index.span(source, base + literal_start, i - literal_start),
                            "unterminated `'...'` literal",
                        ));
                    }
                    let literal = text.get(literal_start..i).unwrap_or_default();
                    parts.push(ArgPart::Text(literal.to_owned()));
                    if i < bytes.len() {
                        i += 1;
                    }
                }
                b'"' => {
                    flush(&mut parts, &mut buffer);
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'$' {
                            flush(&mut parts, &mut buffer);
                            let (part, next) = reference(text, base, i, index, source, &mut diags);
                            parts.push(part);
                            i = next;
                        } else {
                            let width = text[i..].chars().next().map_or(1, char::len_utf8);
                            if let Some(ch) = text[i..].chars().next() {
                                buffer.push(ch);
                            }
                            i += width;
                        }
                    }
                    flush(&mut parts, &mut buffer);
                    if i >= bytes.len() {
                        diags.push(Diagnostic::new(
                            index.span(source, base + slot_start, i - slot_start),
                            "unterminated `\"...\"` argument",
                        ));
                    } else {
                        i += 1;
                    }
                }
                b'$' => {
                    flush(&mut parts, &mut buffer);
                    let (part, next) = reference(text, base, i, index, source, &mut diags);
                    parts.push(part);
                    i = next;
                }
                _ => {
                    let ch = text[i..].chars().next().unwrap_or(' ');
                    buffer.push(ch);
                    i += ch.len_utf8();
                }
            }
        }

        flush(&mut parts, &mut buffer);
        if parts.is_empty() {
            parts.push(ArgPart::Text(String::new()));
        }
        let span = index.span(source, base + slot_start, i - slot_start);
        slots.push(ArgvSlot { parts, span });
    }

    let mut assignments = Vec::new();
    let mut first_command = 0usize;
    while first_command < slots.len() {
        let candidate = &slots[first_command];
        let assignment = match candidate.parts.as_slice() {
            [ArgPart::Text(word)] => is_assignment(word),
            _ => None,
        };
        match assignment {
            Some((key, value)) => {
                assignments.push(EnvPrefix {
                    key: Spanned::new(key.to_owned(), candidate.span),
                    value: value.to_owned(),
                });
                first_command += 1;
            }
            None => break,
        }
    }
    let slots: Vec<ArgvSlot> = slots.into_iter().skip(first_command).collect();
    let span = match (slots.first(), slots.last()) {
        (Some(first), Some(last)) => first.span.merge(last.span),
        _ => Span::new(base, 0, 1, 1),
    };
    (
        Argv {
            assignments,
            slots,
            span,
        },
        diags,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> (Argv, Vec<Diagnostic>) {
        let source = text.to_owned();
        let index = LineIndex::new(&source);
        parse_argv(&source, 0, &source, &index)
    }

    #[test]
    fn interpolated_values_stay_in_one_slot() {
        let (argv, diags) =
            parse("FOO=bar uv run app --bind 0.0.0.0:$port \"hello world\" 'literal $port'");
        assert!(diags.is_empty());
        assert_eq!(argv.assignments.len(), 1);
        assert_eq!(argv.assignments[0].key.value, "FOO");
        assert_eq!(argv.assignments[0].value, "bar");
        assert_eq!(argv.slots.len(), 7);
        assert!(matches!(
            argv.slots[4].parts.as_slice(),
            [ArgPart::Text(prefix), ArgPart::Ref(ArgRef { kind: ArgRefKind::Port, .. })]
                if prefix == "0.0.0.0:"
        ));
        assert!(matches!(
            argv.slots[5].parts.as_slice(),
            [ArgPart::Text(value)] if value == "hello world"
        ));
        assert!(matches!(
            argv.slots[6].parts.as_slice(),
            [ArgPart::Text(value)] if value == "literal $port"
        ));
    }

    #[test]
    fn unknown_reference_is_reported_with_its_own_span() {
        let (_, diags) = parse("run app --token $wat");
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("unknown `$` reference `$wat`"));
        assert_eq!(diags[0].span.col, 17);
    }
}
