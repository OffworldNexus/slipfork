//! Canonical repo-input Merkle digest and segment-wise glob matcher.
//!
//! `slipfork-dag` (`Forkfile` artifact and fixture inputs) and `slipfork-engine` (`L0` build
//! context) hash file trees with the one algorithm defined here: a sorted sequence of
//! `relative path NUL sha256(file)` records folded into a single SHA-256. Living in the shared
//! base crate means neither caller can fork the matcher or the fold again.

use sha2::{Digest, Sha256};

/// Segment-wise glob matcher supporting `**/`, `**`, `*`, and `?`.
pub fn glob_matches(pattern: &str, path: &str) -> bool {
    let pattern: Vec<&str> = pattern
        .trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    let path: Vec<&str> = path
        .trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    match_segments(&pattern, &path)
}

/// Lowercase hex SHA-256 of one file's bytes.
pub fn file_digest(bytes: &[u8]) -> [u8; 32] {
    let mut digest = [0_u8; 32];
    digest.copy_from_slice(&Sha256::digest(bytes));
    digest
}

/// Fold `(relative path, file digest)` pairs into one lowercase hex Merkle root.
///
/// Callers supply the pairs in the tree order they want to commit to (filesystem adapters sort
/// by relative path); the fold itself never reorders so the digest is reproducible.
pub fn merkle_hex<'a>(entries: impl IntoIterator<Item = (&'a str, [u8; 32])>) -> String {
    let mut hasher = Sha256::new();
    for (path, digest) in entries {
        hasher.update(path.as_bytes());
        hasher.update(b"\0");
        hasher.update(digest);
    }
    hex::encode(hasher.finalize())
}

fn match_segments(pattern: &[&str], path: &[&str]) -> bool {
    match (pattern.first(), path.first()) {
        (None, None) => true,
        (Some(&"**"), _) => {
            match_segments(&pattern[1..], path)
                || (!path.is_empty() && match_segments(pattern, &path[1..]))
        }
        (Some(segment), Some(candidate)) if match_segment(segment, candidate) => {
            match_segments(&pattern[1..], &path[1..])
        }
        _ => false,
    }
}

fn match_segment(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let (mut pattern_index, mut text_index) = (0_usize, 0_usize);
    let (mut star, mut resume) = (None, 0_usize);
    while text_index < text.len() {
        if pattern_index < pattern.len()
            && (pattern[pattern_index] == '?' || pattern[pattern_index] == text[text_index])
        {
            pattern_index += 1;
            text_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == '*' {
            star = Some(pattern_index);
            pattern_index += 1;
            resume = text_index;
        } else if let Some(position) = star {
            pattern_index = position + 1;
            resume += 1;
            text_index = resume;
        } else {
            return false;
        }
    }
    while pattern_index < pattern.len() && pattern[pattern_index] == '*' {
        pattern_index += 1;
    }
    pattern_index == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::{file_digest, glob_matches, merkle_hex};

    #[test]
    fn glob_segments_match_the_documented_shapes() {
        assert!(glob_matches("**/*.rs", "src/deep/main.rs"));
        assert!(glob_matches("src/**", "src/deep/main.rs"));
        assert!(!glob_matches("src/*.rs", "src/deep/main.rs"));
        assert!(glob_matches("Docker*.?", "Dockerfile.x"));
    }

    #[test]
    fn merkle_root_is_order_sensitive_and_separates_paths() {
        let a = file_digest(b"a");
        let b = file_digest(b"b");
        let left = merkle_hex([("src/a", a), ("src/b", b)]);
        let right = merkle_hex([("src/b", b), ("src/a", a)]);
        assert_ne!(left, right);
        assert_eq!(left, merkle_hex([("src/a", a), ("src/b", b)]));
        // Golden vector pins the `path NUL sha256(file)` fold: `sha256("src/a" 0 sha256("a")
        // "src/b" 0 sha256("b"))`. A changed separator, digest width, or interleave fails here.
        assert_eq!(
            left,
            "19ccea213ac6b0309fc6dfa56de8c947917807be523bc88b9602dbcf9b24c15a"
        );
        assert_ne!(merkle_hex([("src/a", b)]), merkle_hex([("src/b", b)]));
    }
}
