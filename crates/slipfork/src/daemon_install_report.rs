//! The install report and ingress `UnitSpec` view models, split out of the installer module.
//!
//! `slipfork daemon unit` (preview) and `slipfork daemon install` (effect) share these rows and the
//! one ingress derivation, so the printed bytes and the written bytes can never disagree.

use std::path::{Path, PathBuf};

use slipfork_edge::{INGRESS_ADDRS, UnitSpec};

use crate::daemon_handover::ManagedUnit;
use crate::layout::Layout;
use crate::ui::column_width;
use crate::user::ResolvedAccount;

/// The three absolute file paths one install writes, derived from the installer's directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallPaths {
    /// `slipforkd.socket` path.
    pub socket: PathBuf,
    /// `slipforkd.service` path.
    pub service: PathBuf,
    /// `/etc/apparmor.d/slipfork` path.
    pub apparmor: PathBuf,
}

/// Per-file outcome of an install run (`wrote` or `unchanged`; never `skipped`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    /// The file was created or its bytes changed.
    Wrote,
    /// The file already contained the exact rendered bytes.
    Unchanged,
}

impl FileStatus {
    /// The single word the human and `--json` documents use.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wrote => "wrote",
            Self::Unchanged => "unchanged",
        }
    }
}

/// One completed install: the account the daemon runs as plus each file's outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallReport {
    /// The unprivileged developer account rendered into `User=`.
    pub user: String,
    /// `slipforkd.socket` outcome.
    pub socket: FileStatus,
    /// `slipforkd.service` outcome.
    pub service: FileStatus,
    /// `/etc/apparmor.d/slipfork` outcome.
    pub apparmor: FileStatus,
    /// The exact paths this run wrote (the installer's own derivation).
    pub paths: InstallPaths,
}

impl InstallReport {
    /// The three `(path, status)` rows, declared once for the human transcript.
    fn rows(&self) -> [(&Path, FileStatus); 3] {
        [
            (self.paths.socket.as_path(), self.socket),
            (self.paths.service.as_path(), self.service),
            (self.paths.apparmor.as_path(), self.apparmor),
        ]
    }

    /// `true` when every file already carried the rendered bytes.
    fn all_unchanged(&self) -> bool {
        self.rows()
            .iter()
            .all(|(_, status)| *status == FileStatus::Unchanged)
    }

    /// The first-run transcript (`docs/design/dx-0.1.0-ux.md` §2).
    pub fn render_text(&self) -> String {
        if self.all_unchanged() {
            return format!(
                "unchanged {}, {}, {}; enabled\n",
                ManagedUnit::Socket.unit_name(),
                ManagedUnit::Service.unit_name(),
                self.paths.apparmor.display()
            );
        }
        // Size the status column for the widest legal outcome, so every transcript aligns without
        // a literal width and a future longer status cannot shear the path column.
        let status_width = column_width(
            0,
            [FileStatus::Wrote.as_str(), FileStatus::Unchanged.as_str()],
        );
        let mut text = String::new();
        for (path, status) in self.rows() {
            text.push_str(&format!(
                "{:<status_width$} {}\n",
                status.as_str(),
                path.display()
            ));
        }
        text.push_str(&format!(
            "{:<status_width$} slipforkd.socket + slipforkd.service as {}, ingress 127.0.0.1:80 + [::1]:80\n",
            "enabled", self.user
        ));
        text.push_str(&format!("{:<status_width$} slipfork up\n", "next"));
        text
    }
}

/// The one ingress `UnitSpec` derivation shared by `daemon unit` and `daemon install`.
///
/// Both commands resolve the same [`ResolvedAccount`] and [`Layout`] first, so the preview and the
/// installed bytes can never disagree.
pub fn ingress_unit_spec(
    account: &ResolvedAccount,
    layout: &Layout,
    executable: PathBuf,
    forward_port: u16,
) -> Result<UnitSpec, String> {
    let listen_addrs = INGRESS_ADDRS
        .iter()
        .map(|addr| {
            addr.parse()
                .map_err(|err| format!("invalid loopback ingress '{addr}': {err}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(UnitSpec {
        user: account.name.clone(),
        listen_addrs,
        forward_port,
        executable,
        state_dir: layout.state_dir().to_path_buf(),
        runtime_dir: layout.runtime_dir().to_path_buf(),
    })
}
