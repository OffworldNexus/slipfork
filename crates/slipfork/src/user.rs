//! The [`UserIdentity`] port and its OS-account adapter.
//!
//! `slipfork daemon unit` must render `User=<developer-account>` (never a systemd `%`-template
//! specifier), so the invoking account is resolved exactly once at the CLI composition boundary.
//! The adapter asks the kernel account database through `id -un`; it never trusts ambient `$USER`
//! or `$LOGNAME`, and it spawns no shell.

use std::process::Command;

/// Fixed executable search path for the account lookup (nothing is inherited).
const ACCOUNT_TOOL_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

/// Resolve the invoking developer's OS account name exactly once.
pub trait UserIdentity: Send + Sync {
    /// The account name systemd must place in `User=` (`remy`, `ci`, ...).
    fn username(&self) -> Result<String, String>;
}

/// [`UserIdentity`] backed by the host account database (`id -un` for the effective uid).
#[derive(Debug, Default, Clone, Copy)]
pub struct AccountDatabaseIdentity;

impl UserIdentity for AccountDatabaseIdentity {
    fn username(&self) -> Result<String, String> {
        let output = Command::new("id")
            .arg("-un")
            .env_clear()
            .env("PATH", ACCOUNT_TOOL_PATH)
            .output()
            .map_err(|err| {
                format!("cannot resolve the developer account name: failed to run `id -un`: {err}")
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "cannot resolve the developer account name: `id -un` exited with {}: {}",
                output.status,
                stderr.trim()
            ));
        }
        let name = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if name.is_empty()
            || name.contains('%')
            || name
                .chars()
                .any(|character| character.is_control() || character.is_whitespace())
        {
            return Err(format!(
                "cannot resolve the developer account name: `id -un` returned an unusable value '{name}'"
            ));
        }
        Ok(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_a_non_template_account_name() {
        // The test runner itself is an account in the host database.
        let name = AccountDatabaseIdentity.username().expect("account name");
        assert!(!name.is_empty());
        assert!(!name.contains('%'));
        assert!(!name.chars().any(char::is_whitespace));
    }
}
