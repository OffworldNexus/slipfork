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
                let line = line.trim_end_matches('/').trim_start_matches("./");
                let line = line.trim_start_matches('/');
                if line.is_empty() {
                    return None;
                }
                Some(Pattern {
                    segments: segments(line),
                    negated,
                    dir_only,
                    anchored: line.contains('/'),
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{Dockerignore, SourcePattern};

    #[test]
    fn excludes_globs_directories_and_negations() {
        let ignore = Dockerignore::parse(
            "# dependencies\n\
             node_modules/\n\
             *.log\n\
             build\n\
             **/*.tmp\n\
             !keep.log\n\
             !node_modules/keep.js\n",
        );
        assert!(ignore.is_ignored(Path::new("node_modules/react/index.js")));
        assert!(ignore.is_ignored(Path::new("app/debug.log")));
        assert!(!ignore.is_ignored(Path::new("app/keep.log")));
        assert!(ignore.is_ignored(Path::new("build")));
        assert!(ignore.is_ignored(Path::new("src/a/b/c.tmp")));
        assert!(!ignore.is_ignored(Path::new("src/main.rs")));
        assert!(!ignore.is_ignored(Path::new(".")));
        assert!(!ignore.is_ignored(Path::new("")));
    }

    #[test]
    fn question_mark_and_anchored_paths() {
        let ignore = Dockerignore::parse("?.txt\nlogs/**\n");
        assert!(ignore.is_ignored(Path::new("a.txt")));
        assert!(!ignore.is_ignored(Path::new("ab.txt")));
        assert!(ignore.is_ignored(Path::new("logs/one/two.txt")));
        assert!(!ignore.is_ignored(Path::new("mylogs/one.txt")));
    }

    #[test]
    fn copy_sources_match_character_classes_and_cross_directories() {
        let class = SourcePattern::parse("app[0-9].txt").expect("glob source");
        assert!(class.is_glob());
        assert!(class.matches(Path::new("app7.txt")));
        assert!(!class.matches(Path::new("appx.txt")));

        let deep = SourcePattern::parse("assets/**").expect("recursive glob");
        assert!(deep.matches(Path::new("assets/css/app.css")));
        assert!(!deep.matches(Path::new("other/css/app.css")));

        let any = SourcePattern::parse("**/*.json").expect("globstar prefix");
        assert!(any.matches(Path::new("a/b/c.json")));
        assert!(!any.matches(Path::new("a/b/c.txt")));

        let literal = SourcePattern::parse("src/main.rs").expect("literal source");
        assert!(!literal.is_glob());
        assert!(literal.matches(Path::new("src/main.rs")));
    }
}
