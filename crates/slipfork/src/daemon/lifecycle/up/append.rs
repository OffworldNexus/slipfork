//! Append-only boot log: a read-only historical file rotates to `<stem>.<n>.log`.
//!
//! `chmod -R ga-w .` leaves `.slipfork/supervisor.log` readable but not appendable. An unwritable
//! history must never brick `up`, so the daemon continues in the next free sibling; a directory
//! that truly refuses creation fails with the file, the reason, and the one command that fixes it —
//! never a bare `Permission denied (os error 13)`.

use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

use crate::daemon::Failure;

/// Environment key the daemon uses to hand one narration line to `__supervise`.
///
/// Mirrors `crate::world::runtime::frames::UP_NOTE_ENV`; both sides live in different private
/// module trees, so the literal is stated once per side.
pub(crate) const UP_NOTE_ENV: &str = "SLIPFORK_UP_NOTE";

/// One append target for boot output, rotated to a sibling when the historical file denies append.
#[derive(Debug)]
pub(crate) struct AppendLog {
    /// Open handle every child stderr write appends to.
    file: File,
    /// Effective path written: the canonical file or its rotated sibling.
    path: PathBuf,
    /// One informational sentence when the canonical file had to be skipped.
    note: Option<String>,
}

impl AppendLog {
    /// Opens `<dir>/<stem>.log` for append, or the next free `<dir>/<stem>.<n>.log` if denied.
    pub(crate) fn open_or_rotate(dir: &Path, stem: &str) -> Result<Self, Failure> {
        let canonical = dir.join(format!("{stem}.log"));
        match append_file(&canonical) {
            Ok(file) => Ok(Self {
                file,
                path: canonical,
                note: None,
            }),
            Err(error) if denies_append(&error) => {
                // Only an existing log gets the file remedy; a denied create means the directory
                // itself refuses writes and must be named instead of a file that never existed.
                if !canonical.exists() {
                    return Err(dir_refusal(dir, &error));
                }
                match rotate(dir, stem) {
                    Ok((file, path)) => {
                        let note = format!(
                            "could not append to `{}` ({}); writing `{}`",
                            canonical.display(),
                            reason(&error),
                            path.display()
                        );
                        Ok(Self {
                            file,
                            path,
                            note: Some(note),
                        })
                    }
                    Err(rotation) => Err(refusal(dir, stem, &rotation)),
                }
            }
            Err(error) => Err(refusal(dir, stem, &error)),
        }
    }

    /// The effective path this log appends to.
    #[cfg(test)]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// The single rotation sentence, when the canonical file denied append.
    #[cfg(test)]
    pub(crate) fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    /// Splits the log into the handle a child's stderr takes, its effective path, and its note.
    pub(crate) fn into_parts(self) -> (File, PathBuf, Option<String>) {
        (self.file, self.path, self.note)
    }
}

/// The one message for an existing `.slipfork/<stem>.log` that cannot be appended to.
pub(crate) fn refusal(dir: &Path, stem: &str, error: &io::Error) -> Failure {
    let path = dir.join(format!("{stem}.log"));
    Failure::new(
        "log-open",
        format!(
            "cannot write `{}`: {} — the daemon appends boot output to this file\nnext: chmod u+w {} {} (or `chmod -R u+w {}`)",
            path.display(),
            reason(error),
            dir.display(),
            path.display(),
            dir.display()
        ),
    )
}

/// The one message when `.slipfork` itself refuses to create the boot log.
pub(crate) fn dir_refusal(dir: &Path, error: &io::Error) -> Failure {
    Failure::new(
        "log-dir",
        format!(
            "cannot create the boot log in `{}`: {} — the daemon appends boot output there\nnext: chmod u+w {} (or `chmod -R u+w {}`)",
            dir.display(),
            reason(error),
            dir.display(),
            dir.display()
        ),
    )
}

/// Whether the error means "this file may be read but not appended to".
fn denies_append(error: &io::Error) -> bool {
    error.kind() == ErrorKind::PermissionDenied
        || matches!(error.raw_os_error(), Some(libc::EACCES | libc::EROFS))
}

/// The plain-language cause every message and note reuses, never a raw `io::Error`.
fn reason(error: &io::Error) -> &'static str {
    match error.raw_os_error() {
        Some(libc::EACCES) => "permission denied",
        Some(libc::EROFS) => "read-only filesystem",
        _ if error.kind() == ErrorKind::PermissionDenied => "permission denied",
        _ => "the log could not be opened",
    }
}

/// Opens one append target without truncating anything that already exists.
fn append_file(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

/// The first `<stem>.<n>.log` that does not exist yet, opened append+create.
fn rotate(dir: &Path, stem: &str) -> io::Result<(File, PathBuf)> {
    let mut index = 1_u32;
    loop {
        let path = dir.join(format!("{stem}.{index}.log"));
        if !path.exists() {
            return append_file(&path).map(|file| (file, path));
        }
        index += 1;
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use super::AppendLog;

    /// Restores one directory to `0o755` before its tempdir drops, so cleanup can unlink entries.
    struct Restore(PathBuf);

    impl Restore {
        fn deny_writes(dir: &Path) -> Self {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o555)).expect("deny writes");
            Self(dir.to_path_buf())
        }
    }

    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
        }
    }

    /// A disk-backed temp directory; `/tmp` is RAM-backed on Linux.
    fn disk_dir() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("slipfork-up-append-")
            .tempdir_in("/var/tmp")
            .expect("create a disk temp directory")
    }

    /// Mode bits only deny a non-root writer, so the permission tests skip when elevated.
    fn can_deny_writes() -> bool {
        // SAFETY: `geteuid(2)` only reads this process's credentials.
        unsafe { libc::geteuid() != 0 }
    }

    /// A `0444` history rotates past existing siblings, stays byte-identical, and its successor
    /// appends fine with one note naming both paths.
    #[test]
    fn read_only_history_rotates_and_never_truncates() {
        if !can_deny_writes() {
            return;
        }
        let dir = disk_dir();
        let canonical = dir.path().join("supervisor.log");
        let earlier = dir.path().join("supervisor.1.log");
        fs::write(&canonical, "old boot\n").expect("write history");
        fs::write(&earlier, "even older\n").expect("write earlier rotation");
        fs::set_permissions(&canonical, fs::Permissions::from_mode(0o444)).expect("deny append");

        let log = AppendLog::open_or_rotate(dir.path(), "supervisor").expect("rotate");
        let rotated = log.path().to_path_buf();
        let note = log.note().expect("a rotation narrates itself").to_owned();
        assert_eq!(
            rotated.file_name().and_then(|name| name.to_str()),
            Some("supervisor.2.log"),
            "the next free sibling is chosen"
        );
        assert!(note.contains("supervisor.log"), "{note}");
        assert!(note.contains("supervisor.2.log"), "{note}");
        assert!(note.contains("permission denied"), "{note}");

        let (mut file, path, _note) = log.into_parts();
        writeln!(file, "new boot").expect("append to the rotated log");
        drop(file);
        assert_eq!(fs::read_to_string(&canonical).unwrap(), "old boot\n");
        assert_eq!(fs::read_to_string(&earlier).unwrap(), "even older\n");
        assert_eq!(path, rotated);
        assert_eq!(fs::read_to_string(&rotated).unwrap(), "new boot\n");
    }

    /// A `0555` `.slipfork` fails closed with the path and the `chmod u+w` remedy, never a bare
    /// `Permission denied (os error 13)`.
    #[test]
    fn read_only_directory_fails_closed_with_a_remedy() {
        if !can_deny_writes() {
            return;
        }
        let dir = disk_dir();
        let canonical = dir.path().join("supervisor.log");
        fs::write(&canonical, "old boot\n").expect("write history");
        fs::set_permissions(&canonical, fs::Permissions::from_mode(0o444)).expect("deny append");
        let _restore = Restore::deny_writes(dir.path());

        let failure = AppendLog::open_or_rotate(dir.path(), "supervisor")
            .expect_err("an unwritable directory must fail closed");
        let message = &failure.message;
        assert!(
            message.contains(&dir.path().display().to_string()),
            "{message}"
        );
        assert!(message.contains("supervisor.log"), "{message}");
        assert!(message.contains("chmod u+w"), "{message}");
        assert!(
            !message.contains("os error 13") && message != "Permission denied (os error 13)",
            "the raw EACCES must never be the whole diagnosis: {message}"
        );
    }

    /// The read-only `logs` surface is unaffected: a `0444` log still reads after a rotation.
    #[test]
    fn rotated_history_stays_readable_for_logs() {
        if !can_deny_writes() {
            return;
        }
        let dir = disk_dir();
        let canonical = dir.path().join("supervisor.log");
        fs::write(&canonical, "served\n").expect("write history");
        fs::set_permissions(&canonical, fs::Permissions::from_mode(0o444)).expect("deny append");

        let _log = AppendLog::open_or_rotate(dir.path(), "supervisor").expect("rotate");
        assert_eq!(fs::read_to_string(&canonical).unwrap(), "served\n");
    }

    /// A `0555` `.slipfork` with no log names the directory, never a file that never existed.
    #[test]
    fn read_only_directory_without_a_log_names_only_the_directory() {
        if !can_deny_writes() {
            return;
        }
        let dir = disk_dir();
        let _restore = Restore::deny_writes(dir.path());

        let failure = AppendLog::open_or_rotate(dir.path(), "supervisor")
            .expect_err("an unwritable directory must fail closed");
        let message = &failure.message;
        assert!(
            message.contains(&dir.path().display().to_string()),
            "{message}"
        );
        assert!(message.contains("chmod u+w"), "{message}");
        assert!(!message.contains("supervisor.log"), "{message}");
    }
}
