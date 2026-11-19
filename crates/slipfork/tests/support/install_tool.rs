//! Real `systemctl`/`apparmor_parser` stand-ins for the `daemon install` lab suites.
//!
//! Each tool is a real executable shell script that records its argv on disk. `show` answers
//! `not-found`, so a unit this installer never wrote is skipped; every other verb succeeds. On a
//! host without a system manager the suite still drives the same real subprocess path.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// One recording tool executable named `name` inside `dir`, appending argv lines to `log`.
pub fn recording_tool(dir: &Path, name: &str, log: &Path) -> PathBuf {
    let path = dir.join(name);
    let body = format!(
        "#!/bin/sh\nprintf '%s' \"$*\" >> {log}\necho >> {log}\nif [ \"$1\" = show ]; then echo not-found; fi\nexit 0\n",
        log = log.display()
    );
    fs::write(&path, body).expect("write the tool script");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
        .expect("make the tool executable");
    path
}
