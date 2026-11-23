//! `.dockerignore` matching: `*`, `**`, `?`, `[...]`, trailing `/`, `!` negation, comments.
//!
//! This is the single glob engine in the crate: `COPY`/`ADD` sources are matched through
//! [`SourcePattern`], which reuses the same path-component matcher as `.dockerignore` rules.

use std::path::Path;

/// One parsed pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pattern {
    segments: Vec<Segment>,
    negated: bool,
    dir_only: bool,
    anchored: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Literal(String),
    /// `*`, `?`, or `[...]` inside one path component.
    Glob(String),
    /// `**`, matching any number of components.
    AnyDepth,
}

/// Ordered `.dockerignore` rules applied to `COPY`/`ADD` sources.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Dockerignore {
    patterns: Vec<Pattern>,
}

impl Dockerignore {
    /// Parses the text of a `.dockerignore` file (missing sections are simply empty).
    #[must_use]
    pub fn parse(source: &str) -> Self {
        let patterns = source
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    return None;
                }
                let (negated, line) = match line.strip_prefix('!') {
                    Some(rest) => (true, rest.trim()),
                    None => (false, line),
                };
                let dir_only = line.ends_with('/');
                let trimmed = line.trim_end_matches('/').trim_start_matches("./");
                let rooted = trimmed.starts_with('/');
                let line = trimmed.trim_start_matches('/');
                if line.is_empty() {
                    return None;
                }
                Some(Pattern {
                    segments: segments(line),
                    negated,
                    dir_only,
                    anchored: rooted || line.contains('/'),
                })
            })
            .collect();
        Self { patterns }
    }

    /// Whether `relative` (a POSIX path from the context root) is excluded from the context.
    ///
    /// Later rules win, so `!keep.log` can re-include one file after `*.log`.
    #[must_use]
    pub fn is_ignored(&self, relative: &Path) -> bool {
        let text = relative.to_string_lossy().replace('\\', "/");
        let text = text.trim_start_matches("./");
        if text.is_empty() || text == "." {
            return false;
        }
        let segments: Vec<&str> = text.split('/').filter(|part| !part.is_empty()).collect();
        let mut ignored = false;
        for pattern in &self.patterns {
            if pattern.matches(&segments) {
                ignored = !pattern.negated;
            }
        }
        ignored
    }
}

/// A `COPY`/`ADD` source expression compiled by the canonical path matcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourcePattern {
    pattern: Pattern,
}

impl SourcePattern {
    /// Parses `src/*.rs`, `app[0-9].txt`, or `**/*.json`; `None` for the context root.
    pub(crate) fn parse(source: &str) -> Option<Self> {
        let line = source.trim().trim_end_matches('/').trim_start_matches("./");
        if line.is_empty() || line == "." {
            return None;
        }
        Some(Self {
            pattern: Pattern {
                segments: segments(line),
                negated: false,
                dir_only: false,
                anchored: true,
            },
        })
    }

    /// Whether the source carries any wildcard and therefore needs glob matching.
    pub(crate) fn is_glob(&self) -> bool {
        self.pattern
            .segments
            .iter()
            .any(|segment| !matches!(segment, Segment::Literal(_)))
    }

    /// Anchored match of the whole relative path, `**` crossing as many components as needed.
    pub(crate) fn matches(&self, relative: &Path) -> bool {
        let text = relative.to_string_lossy().replace('\\', "/");
        let segments: Vec<&str> = text.split('/').filter(|part| !part.is_empty()).collect();
        match_segments(&self.pattern.segments, &segments)
    }
}

fn segments(line: &str) -> Vec<Segment> {
    line.split('/')
        .filter(|part| !part.is_empty())
        .map(|part| {
            if part == "**" {
                Segment::AnyDepth
            } else if part.contains(['*', '?', '[']) {
                Segment::Glob(part.to_owned())
            } else {
                Segment::Literal(part.to_owned())
            }
        })
        .collect()
}

impl Pattern {
    fn matches(&self, path: &[&str]) -> bool {
        if self.dir_only {
            return path.len() >= self.segments.len()
                && match_segments(&self.segments, &path[..self.segments.len()]);
        }
        if !self.anchored {
            // A bare name matches any single path component (`*.pyc`, `build`).
            return path
                .iter()
                .any(|component| match_component(&self.segments[0], component));
        }
        match_segments(&self.segments, path)
    }
}

fn match_segments(pattern: &[Segment], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((Segment::AnyDepth, rest)) => {
            if match_segments(rest, path) {
                return true;
            }
            (1..=path.len()).any(|skip| match_segments(rest, &path[skip..]))
        }
        Some((segment, rest)) => match path.split_first() {
            Some((component, tail)) => {
                match_component(segment, component) && match_segments(rest, tail)
            }
            None => false,
        },
    }
}

fn match_component(segment: &Segment, component: &str) -> bool {
    match segment {
        Segment::Literal(literal) => literal == component,
        Segment::Glob(pattern) => glob(pattern, component),
        Segment::AnyDepth => true,
    }
}

/// `*`, `?`, and `[a-z]` matching inside one path component.
fn glob(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let mut memo = vec![vec![None; text.len() + 1]; pattern.len() + 1];
    walk_glob(&pattern, &text, 0, 0, &mut memo)
}

fn walk_glob(
    pattern: &[char],
    text: &[char],
    p: usize,
    t: usize,
    memo: &mut [Vec<Option<bool>>],
) -> bool {
    if let Some(answer) = memo[p][t] {
        return answer;
    }
    let answer = match pattern.get(p) {
        None => t == text.len(),
        Some('*') => {
            walk_glob(pattern, text, p + 1, t, memo)
                || (t < text.len() && walk_glob(pattern, text, p, t + 1, memo))
        }
        Some('?') => t < text.len() && walk_glob(pattern, text, p + 1, t + 1, memo),
        Some('[') => match class_end(pattern, p) {
            Some(end) => {
                t < text.len()
                    && class_matches(&pattern[p + 1..end], text[t])
                    && walk_glob(pattern, text, end + 1, t + 1, memo)
            }
            None => {
                t < text.len() && text[t] == '[' && walk_glob(pattern, text, p + 1, t + 1, memo)
            }
        },
        Some(literal) => {
            t < text.len() && *literal == text[t] && walk_glob(pattern, text, p + 1, t + 1, memo)
        }
    };
    memo[p][t] = Some(answer);
    answer
}

/// Index of the `]` closing the class that opens at `start`.
fn class_end(pattern: &[char], start: usize) -> Option<usize> {
    (start + 2..pattern.len()).find(|&index| pattern[index] == ']')
}

/// `[abc]`, `[a-z]`, `[!0-9]` / `[^0-9]` matching for one character.
fn class_matches(class: &[char], character: char) -> bool {
    let (negated, class) = match class.first() {
        Some('!') | Some('^') => (true, &class[1..]),
        _ => (false, class),
    };
    let mut matched = false;
    let mut index = 0;
    while index < class.len() {
        if index + 2 < class.len() && class[index + 1] == '-' {
            if class[index] <= character && character <= class[index + 2] {
                matched = true;
            }
            index += 3;
        } else {
            if class[index] == character {
                matched = true;
            }
            index += 1;
        }
    }
    matched != negated
}
