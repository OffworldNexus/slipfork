//! The YAML subset a `snow.yml` actually uses: indentation maps/sequences, quoted and plain
//! scalars, inline flow sequences/maps, and `#` comments.
//!
//! `Forkfile` is the only runtime manifest; this reader exists solely so
//! [`crate::migrate_snow_yaml`] can consume an upstream file without a second runtime format and
//! without a YAML crate dependency.

use std::fmt;

/// One node of the parsed subset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Node {
    Scalar(String),
    Map(Vec<(String, Node)>),
    Seq(Vec<Node>),
}

impl Node {
    pub(crate) fn as_scalar(&self) -> Option<&str> {
        match self {
            Self::Scalar(value) => Some(value),
            Self::Map(_) | Self::Seq(_) => None,
        }
    }

    pub(crate) fn get(&self, key: &str) -> Option<&Node> {
        match self {
            Self::Map(entries) => entries
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, node)| node),
            Self::Scalar(_) | Self::Seq(_) => None,
        }
    }

    pub(crate) fn as_map(&self) -> Option<&[(String, Node)]> {
        match self {
            Self::Map(entries) => Some(entries),
            Self::Scalar(_) | Self::Seq(_) => None,
        }
    }

    pub(crate) fn as_seq(&self) -> Option<&[Node]> {
        self.seq()
    }

    pub(crate) fn seq(&self) -> Option<&[Node]> {
        match self {
            Self::Seq(nodes) => Some(nodes),
            Self::Scalar(_) | Self::Map(_) => None,
        }
    }
}

/// A structural `snow.yml` failure with the 1-based line it was detected on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct YamlError {
    pub(crate) line: usize,
    pub(crate) message: String,
}

impl fmt::Display for YamlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "snow.yml:{}: {}", self.line, self.message)
    }
}

#[derive(Debug)]
struct Line {
    number: usize,
    indent: usize,
    content: String,
}

/// Parse one YAML-subset document into a [`Node`].
pub(crate) fn parse(source: &str) -> Result<Node, YamlError> {
    let lines = preprocess(source)?;
    if lines.is_empty() {
        return Ok(Node::Map(Vec::new()));
    }
    let mut index = 0;
    let node = parse_block(&lines, &mut index, lines[0].indent)?;
    if index < lines.len() {
        return Err(error(
            &lines[index],
            format!("unexpected indentation before `{}`", lines[index].content),
        ));
    }
    Ok(node)
}

fn error(line: &Line, message: impl Into<String>) -> YamlError {
    YamlError {
        line: line.number,
        message: message.into(),
    }
}

fn preprocess(source: &str) -> Result<Vec<Line>, YamlError> {
    let mut lines = Vec::new();
    for (offset, raw) in source.lines().enumerate() {
        let number = offset + 1;
        let trimmed = strip_comment(raw);
        let content = trimmed.trim();
        if content.is_empty() || content == "---" || content == "..." {
            continue;
        }
        if content.contains('\t') {
            return Err(YamlError {
                line: number,
                message: "tabs are not valid indentation; use spaces".to_owned(),
            });
        }
        let indent = trimmed.len() - trimmed.trim_start_matches(' ').len();
        lines.push(Line {
            number,
            indent,
            content: content.to_owned(),
        });
    }
    Ok(lines)
}

fn strip_comment(raw: &str) -> &str {
    let mut quote: Option<char> = None;
    for (index, ch) in raw.char_indices() {
        match quote {
            Some(active) if ch == active => quote = None,
            Some(_) => {}
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch == '#' => return &raw[..index],
            None => {}
        }
    }
    raw
}

fn parse_block(lines: &[Line], index: &mut usize, indent: usize) -> Result<Node, YamlError> {
    if lines[*index].content.starts_with("- ") || lines[*index].content == "-" {
        parse_sequence(lines, index, indent)
    } else {
        parse_map(lines, index, indent)
    }
}

fn parse_sequence(lines: &[Line], index: &mut usize, indent: usize) -> Result<Node, YamlError> {
    let mut nodes = Vec::new();
    while *index < lines.len() && lines[*index].indent == indent {
        let line = &lines[*index];
        let Some(rest) = line.content.strip_prefix('-') else {
            return Err(error(line, "expected a `- ` sequence entry"));
        };
        let rest = rest.trim_start();
        if rest.is_empty() {
            *index += 1;
            if *index < lines.len() && lines[*index].indent > indent {
                nodes.push(parse_block(lines, index, lines[*index].indent)?);
            } else {
                nodes.push(Node::Scalar(String::new()));
            }
            continue;
        }
        if let Some((key, value)) = split_key(rest) {
            // `- key: value` starts a map entry; sibling keys are indented under the dash.
            let entry_indent = indent + (line.content.len() - rest.len());
            let mut entries = vec![(
                key,
                parse_inline_or_nested(lines, index, value, entry_indent)?,
            )];
            while *index < lines.len()
                && lines[*index].indent == entry_indent
                && !lines[*index].content.starts_with("- ")
            {
                let current = &lines[*index];
                let Some((key, value)) = split_key(&current.content) else {
                    return Err(error(current, "expected a `key: value` map entry"));
                };
                let key = key.to_owned();
                entries.push((
                    key,
                    parse_inline_or_nested(lines, index, value, current.indent)?,
                ));
            }
            nodes.push(Node::Map(entries));
            continue;
        }
        nodes.push(parse_scalar(rest));
        *index += 1;
    }
    Ok(Node::Seq(nodes))
}

fn parse_map(lines: &[Line], index: &mut usize, indent: usize) -> Result<Node, YamlError> {
    let mut entries = Vec::new();
    while *index < lines.len() && lines[*index].indent == indent {
        let current = &lines[*index];
        if current.content.starts_with("- ") {
            break;
        }
        let Some((key, value)) = split_key(&current.content) else {
            return Err(error(current, "expected a `key: value` map entry"));
        };
        entries.push((key, parse_inline_or_nested(lines, index, value, indent)?));
    }
    Ok(Node::Map(entries))
}

/// Parse the value part of one `key:` entry, advancing past nested blocks.
fn parse_inline_or_nested(
    lines: &[Line],
    index: &mut usize,
    value: &str,
    key_indent: usize,
) -> Result<Node, YamlError> {
    let value = value.trim();
    if value.is_empty() {
        *index += 1;
        if *index < lines.len() && lines[*index].indent > key_indent {
            return parse_block(lines, index, lines[*index].indent);
        }
        return Ok(Node::Scalar(String::new()));
    }
    *index += 1;
    Ok(parse_scalar(value))
}

fn split_key(content: &str) -> Option<(String, &str)> {
    let mut quote: Option<char> = None;
    for (index, ch) in content.char_indices() {
        match quote {
            Some(active) if ch == active => quote = None,
            Some(_) => {}
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch == ':' => {
                let after = &content[index + 1..];
                if after.is_empty() || after.starts_with(' ') {
                    return Some((unquote(content[..index].trim()), after));
                }
            }
            None => {}
        }
    }
    None
}

fn unquote(raw: &str) -> String {
    match parse_scalar(raw) {
        Node::Scalar(value) => value,
        Node::Map(_) | Node::Seq(_) => raw.to_owned(),
    }
}

fn parse_scalar(raw: &str) -> Node {
    let raw = raw.trim();
    if raw.starts_with('[') && raw.ends_with(']') {
        let inner = &raw[1..raw.len() - 1];
        if inner.trim().is_empty() {
            return Node::Seq(Vec::new());
        }
        return Node::Seq(
            split_flow(inner)
                .iter()
                .map(|item| parse_scalar(item))
                .collect(),
        );
    }
    if raw.starts_with('{') && raw.ends_with('}') {
        let inner = &raw[1..raw.len() - 1];
        if inner.trim().is_empty() {
            return Node::Map(Vec::new());
        }
        return Node::Map(
            split_flow(inner)
                .into_iter()
                .filter_map(|entry| {
                    split_key(&entry).map(|(key, value)| (key, parse_scalar(value)))
                })
                .collect(),
        );
    }
    Node::Scalar(unquote_scalar(raw))
}

fn unquote_scalar(raw: &str) -> String {
    let bytes = raw.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"' {
        let mut out = String::new();
        let mut chars = raw[1..raw.len() - 1].chars();
        while let Some(ch) = chars.next() {
            if ch != '\\' {
                out.push(ch);
                continue;
            }
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(other) => out.push(other),
                None => {}
            }
        }
        return out;
    }
    if bytes.len() >= 2 && bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\'' {
        return raw[1..raw.len() - 1].replace("''", "'");
    }
    raw.to_owned()
}

fn split_flow(inner: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut depth = 0_usize;
    let mut quote: Option<char> = None;
    let mut start = 0_usize;
    for (index, ch) in inner.char_indices() {
        match quote {
            Some(active) if ch == active => quote = None,
            Some(_) => {}
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch == '[' || ch == '{' => depth += 1,
            None if ch == ']' || ch == '}' => depth = depth.saturating_sub(1),
            None if ch == ',' && depth == 0 => {
                items.push(inner[start..index].trim().to_owned());
                start = index + 1;
            }
            None => {}
        }
    }
    items.push(inner[start..].trim().to_owned());
    items
}

#[cfg(test)]
mod tests {
    use super::{Node, parse};

    #[test]
    fn parses_the_upstream_subset() {
        let node = parse(
            r#"
images:
  - id: api
    context: ./api
    envs: [common, shared]
components:
  - id: api
    image: api
    run_command: "uv run gunicorn app:app --bind 0.0.0.0:8000"
modes:
  all:
    envs:
      common:
        A: "${{ vars::A ?? \"x\" }}"
        B: plain
"#,
        )
        .expect("parse");
        let images = node.get("images").and_then(Node::seq).expect("images");
        assert_eq!(
            images[0].get("context").and_then(Node::as_scalar),
            Some("./api")
        );
        assert_eq!(
            images[0].get("envs").and_then(Node::seq),
            Some(
                [
                    Node::Scalar("common".to_owned()),
                    Node::Scalar("shared".to_owned())
                ]
                .as_slice()
            )
        );
        assert_eq!(
            node.get("modes")
                .and_then(|modes| modes.get("all"))
                .and_then(|all| all.get("envs"))
                .and_then(|envs| envs.get("common"))
                .and_then(|common| common.get("B"))
                .and_then(Node::as_scalar),
            Some("plain")
        );
    }
}
