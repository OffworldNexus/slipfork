//! `Dockerfile` front end: continuations, comments, heredocs, exec/shell forms.
//!
//! Everything that understands Dockerfile syntax lives here. Later stages only ever see typed
//! [`Instruction`] values, so no downstream module re-scans `$`, `<<`, or flag text.

mod config;
mod dev;
mod instruction;
mod parse;
mod variables;

pub use config::ImageConfig;
pub(crate) use instruction::CopyOptions;
pub use instruction::{
    CacheMount, Command, CopySpec, Instruction, Owner, ParsedInstruction, Port, Protocol, RunSpec,
    Stage, StageBase,
};
pub use variables::{BuildArgs, Variables};

use crate::error::{Error, Result};
use parse::parse_instruction;

/// Line continuations, comments, heredocs, and exec/shell forms.
#[derive(Debug, Clone, Copy)]
pub struct DockerfileParser;

impl DockerfileParser {
    /// Parses every stage, resolving `FROM <stage>` against earlier aliases.
    pub fn parse(source: &str) -> Result<Vec<Stage>> {
        let lines: Vec<&str> = source.lines().collect();
        let mut stages: Vec<Stage> = Vec::new();
        let mut aliases: Vec<String> = Vec::new();
        let mut index = 0;
        while index < lines.len() {
            let first = lines[index].trim_start();
            if first.is_empty() || first.starts_with('#') {
                index += 1;
                continue;
            }
            let line = index + 1;
            let mut text = String::new();
            loop {
                let raw = lines[index];
                index += 1;
                let mut piece = raw.trim_end();
                let continued = piece.ends_with('\\');
                if continued {
                    piece = piece[..piece.len() - 1].trim_end();
                }
                if !text.is_empty() {
                    text.push(' ');
                }
                text.push_str(piece.trim_start());
                if !continued || index >= lines.len() {
                    break;
                }
                while index < lines.len() {
                    let peek = lines[index].trim_start();
                    if peek.is_empty() || peek.starts_with('#') {
                        index += 1;
                    } else {
                        break;
                    }
                }
            }
            let heredocs = find_heredocs(&text);
            let mut bodies = Vec::new();
            for (delimiter, strip_tabs) in &heredocs {
                bodies.push(read_heredoc(
                    &lines,
                    &mut index,
                    delimiter,
                    *strip_tabs,
                    line,
                )?);
            }
            let instruction = parse_instruction(&text, line, &heredocs, &bodies, &aliases)?;
            if let Instruction::From { base, alias } = &instruction {
                stages.push(Stage {
                    line,
                    alias: alias.clone(),
                    base: base.clone(),
                    instructions: Vec::new(),
                });
                if let Some(alias) = alias {
                    aliases.push(alias.clone());
                }
                continue;
            }
            match stages.last_mut() {
                Some(stage) => stage
                    .instructions
                    .push(ParsedInstruction { line, instruction }),
                None => {
                    return Err(Error::Dockerfile {
                        line,
                        message: "instruction appears before any `FROM`".to_owned(),
                        next: "start the file with `FROM <image>`",
                    });
                }
            }
        }
        if stages.is_empty() {
            return Err(Error::Dockerfile {
                line: 1,
                message: "the Dockerfile has no `FROM` instruction".to_owned(),
                next: "start the file with `FROM <image>`",
            });
        }
        Ok(stages)
    }
}

/// Splits leading `--flag=value` words off an instruction tail.
fn split_flags(rest: &str) -> (Vec<String>, &str) {
    let bytes = rest.as_bytes();
    let mut index = 0;
    let mut flags = Vec::new();
    loop {
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index + 1 < bytes.len() && bytes[index] == b'-' && bytes[index + 1] == b'-' {
            let start = index;
            while index < bytes.len() && !bytes[index].is_ascii_whitespace() {
                index += 1;
            }
            flags.push(rest[start..index].to_owned());
        } else {
            break;
        }
    }
    (flags, rest[index..].trim_start())
}

/// White-space split that strips one layer of single or double quotes.
fn shell_split(text: &str, line: usize) -> Result<Vec<String>> {
    let bytes = text.as_bytes();
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote: Option<u8> = None;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        match quote {
            Some(q) if byte == q => {
                quote = None;
                index += 1;
            }
            Some(_) => {
                current.push(byte as char);
                index += 1;
            }
            None if byte == b'\'' || byte == b'"' => {
                quote = Some(byte);
                index += 1;
            }
            None if byte.is_ascii_whitespace() => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
                index += 1;
            }
            None => {
                current.push(byte as char);
                index += 1;
            }
        }
    }
    if quote.is_some() {
        return Err(Error::Dockerfile {
            line,
            message: "unterminated quote".to_owned(),
            next: "close the quote or escape it",
        });
    }
    if !current.is_empty() {
        words.push(current);
    }
    Ok(words)
}

/// Splits a `--mount` option list on commas.
fn permissive_split(value: &str) -> Vec<&str> {
    value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect()
}

/// Heredoc `<<WORD` / `<<-WORD` operators outside quotes.
fn find_heredocs(text: &str) -> Vec<(String, bool)> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut quote: Option<u8> = None;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        match quote {
            Some(q) => {
                if byte == q {
                    quote = None;
                }
                index += 1;
            }
            None if byte == b'\'' || byte == b'"' => {
                quote = Some(byte);
                index += 1;
            }
            None if byte == b'<' && bytes.get(index + 1) == Some(&b'<') => {
                let mut cursor = index + 2;
                let strip_tabs = bytes.get(cursor) == Some(&b'-');
                if strip_tabs {
                    cursor += 1;
                }
                let start = cursor;
                while cursor < bytes.len()
                    && (bytes[cursor].is_ascii_alphanumeric() || bytes[cursor] == b'_')
                {
                    cursor += 1;
                }
                if cursor > start {
                    found.push((text[start..cursor].to_owned(), strip_tabs));
                }
                index = cursor.max(index + 2);
            }
            None => index += 1,
        }
    }
    found
}

/// Replaces heredoc operators with spaces so callers can detect a script-only `RUN`.
fn strip_heredoc_operators(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'<' && bytes.get(index + 1) == Some(&b'<') {
            index += 2;
            if bytes.get(index) == Some(&b'-') {
                index += 1;
            }
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
            {
                index += 1;
            }
            out.push(' ');
        } else {
            out.push(bytes[index] as char);
            index += 1;
        }
    }
    out
}

fn read_heredoc(
    lines: &[&str],
    index: &mut usize,
    delimiter: &str,
    strip_tabs: bool,
    line: usize,
) -> Result<Vec<String>> {
    let mut body = Vec::new();
    while *index < lines.len() {
        let raw = lines[*index];
        *index += 1;
        let candidate = if strip_tabs {
            raw.trim_start_matches('\t')
        } else {
            raw
        };
        if candidate.trim_end() == delimiter {
            return Ok(body);
        }
        body.push(candidate.to_owned());
    }
    Err(Error::Dockerfile {
        line,
        message: format!("unterminated heredoc `{delimiter}`"),
        next: "close the heredoc with a line containing only its delimiter",
    })
}
