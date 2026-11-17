//! The [`UserIdentity`] port and its OS-account adapter.
//!
//! `slipfork daemon unit` must render `User=<developer-account>` (never a systemd `%`-template
//! specifier), and `slipfork daemon install` must resolve the account the installed daemon runs
//! as (`ADR-0011` §1). The adapter asks the kernel account database through `id -un` and
//! `getent passwd`; it never trusts ambient `$USER` or `$LOGNAME`, and it spawns no shell.
//!
//! [`AccountDatabaseIdentity::invoking_account`] is the **only** reader of `SUDO_USER` in the
//! workspace: a bare `sudo slipfork daemon install` therefore installs for the operator's login
//! account instead of `root`.

use std::path::PathBuf;
use std::process::Command;

/// Fixed executable search path for every account lookup (nothing is inherited).
const ACCOUNT_TOOL_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

/// The one fail-closed remedy when the daemon account would be `root`.
///
/// `slipforkd` runs unprivileged (`User=<account>`, empty `CapabilityBoundingSet=`) and every
/// world relies on unprivileged namespaces and non-`uid 0` PostgreSQL `initdb`, so `root` is never
/// a valid target (`ADR-0011` §1).
pub const UNPRIVILEGED_ACCOUNT_REMEDY: &str = "slipforkd must run as an unprivileged developer account, never root; log in as that account and run `sudo slipfork daemon install`";

/// One resolved OS account (`ADR-0011` §1): the daemon's `User=`, its identity, and its home.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAccount {
    /// Account name systemd places in `User=` (`remy`, `ci`, ...).
    pub name: String,
    /// Numeric uid (`0` is rejected before this type is ever constructed).
    pub uid: u32,
    /// Numeric primary gid.
    pub gid: u32,
    /// Absolute home directory the state/runtime layout derives from.
    pub home: PathBuf,
}

/// Resolve the invoking developer's OS account and any explicitly named account.
pub trait UserIdentity: Send + Sync {
    /// The account name systemd must place in `User=` (`remy`, `ci`, ...).
    fn username(&self) -> Result<String, String>;

    /// Resolve `account` from the host account database.
    ///
    /// Rejects unusable names (`%` specifiers, whitespace, control characters), unknown accounts,
    /// accounts without an absolute home, and `uid == 0` (the daemon must never run as root).
    fn lookup_account(&self, account: &str) -> Result<ResolvedAccount, String>;

    /// The operator's login account behind `sudo`, if any (`SUDO_USER`, validated by
    /// [`Self::lookup_account`]).
    ///
    /// `Ok(None)` means the process was not started through `sudo`; nothing outside this adapter
    /// ever reads `SUDO_USER`.
    fn invoking_account(&self) -> Result<Option<String>, String>;
}

/// [`UserIdentity`] backed by the host account database (`id -un` / `getent passwd`).
#[derive(Debug, Default, Clone, Copy)]
pub struct AccountDatabaseIdentity;

impl AccountDatabaseIdentity {
    /// Run one account-database tool with a cleared environment and the fixed tool path.
    fn run_tool(&self, program: &str, args: &[&str]) -> Result<String, String> {
        let output = Command::new(program)
            .args(args)
            .env_clear()
            .env("PATH", ACCOUNT_TOOL_PATH)
            .output()
            .map_err(|err| format!("cannot run `{program}` to resolve an OS account: {err}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "`{program} {}` exited with {}: {}",
                args.join(" "),
                output.status,
                stderr.trim()
            ));
        }
        String::from_utf8(output.stdout)
            .map_err(|err| format!("`{program}` returned non-UTF-8 account data: {err}"))
    }
}

impl UserIdentity for AccountDatabaseIdentity {
    fn username(&self) -> Result<String, String> {
        let name = self.run_tool("id", &["-un"])?.trim().to_owned();
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

    fn lookup_account(&self, account: &str) -> Result<ResolvedAccount, String> {
        if account.is_empty()
            || account.contains('%')
            || account
                .chars()
                .any(|character| character.is_control() || character.is_whitespace())
        {
            return Err(format!(
                "'{account}' is not a usable OS account name: systemd %-specifiers, whitespace, and empty names are forbidden"
            ));
        }
        let passwd = self
            .run_tool("getent", &["passwd", account])
            .map_err(|err| format!("no OS account '{account}' exists on this host: {err}"))?;
        let line = passwd.lines().next().ok_or_else(|| {
            format!("no OS account '{account}' exists on this host: `getent passwd` was empty")
        })?;
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() < 7 || fields[0] != account {
            return Err(format!(
                "the host account database returned an unusable `getent passwd {account}` row"
            ));
        }
        if fields[0].contains('%') {
            return Err(format!(
                "'{account}' is not a usable OS account name: systemd %-specifiers are forbidden"
            ));
        }
        let uid: u32 = fields[2]
            .parse()
            .map_err(|err| format!("account '{account}' has an unreadable uid: {err}"))?;
        let gid: u32 = fields[3]
            .parse()
            .map_err(|err| format!("account '{account}' has an unreadable gid: {err}"))?;
        let home = PathBuf::from(fields[5]);
        if !home.is_absolute() {
            return Err(format!(
                "account '{account}' has no absolute home directory (got '{}')",
                home.display()
            ));
        }
        if uid == 0 {
            return Err(UNPRIVILEGED_ACCOUNT_REMEDY.to_owned());
        }
        Ok(ResolvedAccount {
            name: fields[0].to_owned(),
            uid,
            gid,
            home,
        })
    }

    fn invoking_account(&self) -> Result<Option<String>, String> {
        let Some(raw) = std::env::var_os("SUDO_USER") else {
            return Ok(None);
        };
        let name = raw.to_string_lossy();
        let name = name.trim();
        if name.is_empty() {
            return Ok(None);
        }
        // Validation is the lookup itself: unknown accounts, `%` specifiers, and `uid == 0` all
        // fail closed here instead of rendering an unusable `User=`.
        let account = self.lookup_account(name)?;
        Ok(Some(account.name))
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

    #[test]
    fn lookup_returns_the_invoking_accounts_home_and_identity() {
        let identity = AccountDatabaseIdentity;
        let name = identity.username().expect("account name");
        let account = identity.lookup_account(&name).expect("host lookup");
        assert_eq!(account.name, name);
        assert_ne!(account.uid, 0, "the test runner is never uid 0");
        assert!(account.home.is_absolute());
    }

    #[test]
    fn root_and_template_and_unknown_accounts_fail_closed() {
        let identity = AccountDatabaseIdentity;
        assert_eq!(
            identity
                .lookup_account("root")
                .expect_err("root is refused"),
            UNPRIVILEGED_ACCOUNT_REMEDY
        );
        for refused in ["%i", "%u", "", "dev user", "dev\tuser"] {
            let error = identity
                .lookup_account(refused)
                .expect_err("unusable names are refused");
            assert!(
                error.contains("%") || error.contains("usable"),
                "the refusal must name the rule: {error}"
            );
        }
        let unknown = identity
            .lookup_account("slipfork-no-such-account")
            .expect_err("unknown accounts are refused");
        assert!(
            unknown.contains("no OS account") && unknown.contains("slipfork-no-such-account"),
            "the refusal must name the missing account: {unknown}"
        );
    }
}
