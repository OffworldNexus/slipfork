//! `.dockerignore`-filtered build-context Merkle hashing (`ADR-0009` §3).
//!
//! The `L0` cache key covers the build context exactly as `docker buildx` sees it: every file
//! below `context`, minus `.dockerignore` matches and minus `.git/`/`.worktrees/`. The tree is
//! walked in sorted order and hashed as `relative path NUL file sha256`, so a source-only edit that
//! Docker ignores leaves the world rootfs cache hot.

#![forbid(unsafe_code)]

use std::path::Path;

use sha2::{Digest, Sha256};

use crate::rootfs::{BaseImageError, InputHasher};

/// Production [`InputHasher`]: deterministic filesystem walk + `.dockerignore`.
#[derive(Debug, Default, Clone, Copy)]
pub struct FsInputHasher;

impl InputHasher for FsInputHasher {
    fn hash_context(&self, context: &Path) -> Result<String, BaseImageError> {
        let ignore = DockerIgnore::load(context)?;
        let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
        walk(context, context, &ignore, &mut entries)?;
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        let mut hash = Sha256::new();
        for (rel_path, content) in entries {
            hash.update(rel_path.as_bytes());
            hash.update(b"\0");
            hash.update(Sha256::digest(content));
        }
        Ok(hex::encode(hash.finalize()))
    }
}

fn walk(
    context: &Path,
    dir: &Path,
    ignore: &DockerIgnore,
    entries: &mut Vec<(String, Vec<u8>)>,
) -> Result<(), BaseImageError> {
    let mut children: Vec<_> = std::fs::read_dir(dir)
        .map_err(|error| io_error(dir, &error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| io_error(dir, &error))?;
    children.sort_by_key(|entry| entry.file_name());
    for child in children {
        let path = child.path();
        let rel = path
            .strip_prefix(context)
            .map(|rel| rel.to_string_lossy().replace('\\', "/"))
            .map_err(|error| BaseImageError::Io {
                path: path.display().to_string(),
                message: error.to_string(),
            })?;
        if is_always_excluded(&rel) {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| io_error(&path, &error))?;
        let is_dir = metadata.is_dir();
        if ignore.is_ignored(&rel, is_dir) && (!is_dir || !ignore.has_negation()) {
            continue;
        }
        if is_dir {
            walk(context, &path, ignore, entries)?;
        } else if metadata.is_symlink() {
            let target = std::fs::read_link(&path).map_err(|error| io_error(&path, &error))?;
            let mut content = b"symlink\0".to_vec();
            content.extend_from_slice(target.to_string_lossy().as_bytes());
            entries.push((rel, content));
        } else {
            let content = std::fs::read(&path).map_err(|error| io_error(&path, &error))?;
            entries.push((rel, content));
        }
    }
    Ok(())
}

fn is_always_excluded(rel: &str) -> bool {
    rel.split('/')
        .any(|segment| segment == ".git" || segment == ".worktrees")
}

fn io_error(path: &Path, error: &std::io::Error) -> BaseImageError {
    BaseImageError::Io {
        path: path.display().to_string(),
        message: error.to_string(),
    }
}

/// Parsed `.dockerignore` (comments, negations, directory-only patterns).
#[derive(Debug, Default, Clone)]
struct DockerIgnore {
    patterns: Vec<IgnorePattern>,
}

#[derive(Debug, Clone)]
struct IgnorePattern {
    negate: bool,
    directory_only: bool,
    pattern: String,
}

impl DockerIgnore {
    fn load(context: &Path) -> Result<Self, BaseImageError> {
        let path = context.join(".dockerignore");
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Ok(Self::default());
        };
        let mut patterns = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (negate, line) = match line.strip_prefix('!') {
                Some(rest) => (true, rest.trim()),
                None => (false, line),
            };
            let (directory_only, line) = match line.strip_suffix('/') {
                Some(rest) => (true, rest.trim_end_matches('/')),
                None => (false, line),
            };
            let cleaned = line.trim_start_matches("./").trim_start_matches('/');
            if cleaned.is_empty() || cleaned == "." {
                continue;
            }
            patterns.push(IgnorePattern {
                negate,
                directory_only,
                pattern: cleaned.to_owned(),
            });
        }
        Ok(Self { patterns })
    }

    fn has_negation(&self) -> bool {
        self.patterns.iter().any(|pattern| pattern.negate)
    }

    fn is_ignored(&self, rel: &str, is_dir: bool) -> bool {
        let mut ignored = false;
        for pattern in &self.patterns {
            let mut candidate = rel;
            loop {
                let candidate_is_dir = candidate == rel && is_dir || candidate != rel;
                if (!pattern.directory_only || candidate_is_dir)
                    && glob_matches(&pattern.pattern, candidate)
                {
                    ignored = !pattern.negate;
                    break;
                }
                match candidate.rsplit_once('/') {
                    Some((parent, _)) => candidate = parent,
                    None => break,
                }
            }
        }
        ignored
    }
}

/// Segment-wise glob matcher supporting `**/`, `**`, `*`, and `?`.
fn glob_matches(pattern: &str, path: &str) -> bool {
    let pattern: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
    let path: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    match_segments(&pattern, &path)
}

fn match_segments(pattern: &[&str], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => (0..=path.len()).any(|skip| match_segments(rest, &path[skip..])),
        Some((segment, rest)) => match path.split_first() {
            Some((candidate, tail)) if match_segment(segment, candidate) => {
                match_segments(rest, tail)
            }
            _ => false,
        },
    }
}

fn match_segment(pattern: &str, candidate: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let candidate: Vec<char> = candidate.chars().collect();
    let (mut p, mut c) = (0, 0);
    let (mut star, mut resume) = (None, 0);
    while c < candidate.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some(p);
                resume = c;
                p += 1;
            }
            Some('?') => {
                p += 1;
                c += 1;
            }
            Some(expected) if *expected == candidate[c] => {
                p += 1;
                c += 1;
            }
            _ => match star {
                Some(position) => {
                    p = position + 1;
                    resume += 1;
                    c = resume;
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|ch| *ch == '*')
}

/// Lowercase hex SHA-256 helper shared by the tests.
#[cfg(test)]
mod tests {
    use super::DockerIgnore;

    #[test]
    fn dockerignore_honors_negation_and_directory_patterns() {
        let ignore = DockerIgnore {
            patterns: vec![
                super::IgnorePattern {
                    negate: false,
                    directory_only: true,
                    pattern: "target".to_owned(),
                },
                super::IgnorePattern {
                    negate: false,
                    directory_only: false,
                    pattern: "**/*.log".to_owned(),
                },
                super::IgnorePattern {
                    negate: true,
                    directory_only: false,
                    pattern: "keep.log".to_owned(),
                },
            ],
        };
        assert!(ignore.is_ignored("target/debug/app", false));
        assert!(ignore.is_ignored("docs/build.log", false));
        assert!(!ignore.is_ignored("keep.log", false));
        assert!(!ignore.is_ignored("src/main.rs", false));
    }

    #[test]
    fn glob_segments() {
        assert!(super::glob_matches("**/*.rs", "src/deep/main.rs"));
        assert!(super::glob_matches("src/**", "src/deep/main.rs"));
        assert!(!super::glob_matches("src/*.rs", "src/deep/main.rs"));
        assert!(super::glob_matches("Docker*.?", "Dockerfile.x"));
    }
}
