//! Pure mount resolution shared by the composition root and its lab tests.
//!
//! `/proc/self/mountinfo` field 5 carries the kernel's `mangle_path` `\NNN` octal escapes
//! (`\040` space, `\011` tab, `\012` newline, `\134` backslash). The store root is the longest
//! mount point containing the project, so a nested Btrfs mount always wins over its parent.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

/// Extracts mount points (field 5) in kernel stacking order from `/proc/self/mountinfo` text.
#[must_use]
pub fn parse_mountinfo(text: &str) -> Vec<PathBuf> {
    let mut mounts = Vec::new();
    for line in text.lines() {
        let Some((head, _)) = line.split_once(" - ") else {
            continue;
        };
        let Some(mangled) = head.split_ascii_whitespace().nth(4) else {
            continue;
        };
        mounts.push(unmangle_mount_path(mangled));
    }
    mounts
}

/// Decodes Linux `mangle_path` `\NNN` octal escapes (`\040`, `\011`, `\012`, `\134`).
fn unmangle_mount_path(raw: &str) -> PathBuf {
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\'
            && index + 3 < bytes.len()
            && let (Some(a), Some(b), Some(c)) = (
                octal_digit(bytes[index + 1]),
                octal_digit(bytes[index + 2]),
                octal_digit(bytes[index + 3]),
            )
        {
            decoded.push((a << 6) | (b << 3) | c);
            index += 4;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    PathBuf::from(OsString::from_vec(decoded))
}

const fn octal_digit(byte: u8) -> Option<u8> {
    if byte >= b'0' && byte <= b'7' {
        Some(byte - b'0')
    } else {
        None
    }
}

/// Selects the longest mount point containing `project`, falling back to `/`.
#[must_use]
pub fn mount_for<'a>(mounts: &'a [PathBuf], project: &Path) -> &'a Path {
    let mut best: Option<(&Path, usize)> = None;
    for mount in mounts {
        if project.starts_with(mount) {
            let depth = mount.components().count();
            if best.is_none_or(|(_, best_depth)| depth >= best_depth) {
                best = Some((mount.as_path(), depth));
            }
        }
    }
    best.map_or(Path::new("/"), |(mount, _)| mount)
}
