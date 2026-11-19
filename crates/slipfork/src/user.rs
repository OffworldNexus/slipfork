//! The concrete [`UserIdentity`] value and the OS-account / kernel probes behind it.
//!
//! `slipfork daemon unit` must render `User=<developer-account>` (never a systemd `%`-template
//! specifier), `slipfork daemon install` must resolve the account the installed daemon runs as
//! (`ADR-0011` §1), and `slipfork open --remote` must name the machine the developer is SSH'd
//! into. The probes ask the kernel account database through `id -un` / `getent passwd` and the
//! kernel node name through `uname -n`; they never trust ambient `$USER`, `$LOGNAME`, or
//! `$HOSTNAME`, and they spawn no shell.
//!
//! [`UserIdentity::resolve`] and [`account_name`] run **only** at a Composition Root
//! (`crates/slipfork/src/main.rs`); every other caller receives the resolved value. The one
//! `SUDO_USER` read lives in `main.rs` for the same reason.

use std::path::PathBuf;
use std::process::Command;

use slipfork_engine::current_uid_gid;

/// Fixed executable search path for every account/host lookup (nothing is inherited).
const ACCOUNT_TOOL_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

/// The one fail-closed remedy when the daemon account would be `root`.
///
/// `slipforkd` runs unprivileged (`User=<account>`, empty `CapabilityBoundingSet=`) and every
/// world relies on unprivileged namespaces and non-`uid 0` PostgreSQL `initdb`, so `root` is never
/// a valid target (`ADR-0011` §1).
pub const UNPRIVILEGED_ACCOUNT_REMEDY: &str = "slipforkd must run as an unprivileged developer account, never root; log in as that account and run `sudo slipfork daemon install`";

/// Run one account/host probe with a cleared environment and the fixed tool path.
///
/// Every probe below uses this single runner, so none inherits a `PATH`, `USER`, or `HOSTNAME`
/// from the ambient process.
fn run_probe_tool(program: &str, args: &[&str]) -> Result<String, String> {
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

/// The one account-name rule: a value systemd can place in `User=` unchanged.
///
/// `%` starts a specifier, whitespace cannot survive a unit line, and an empty name is no account.
fn validate_account_name(account: &str) -> Result<(), String> {
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
    Ok(())
}

/// The invoking process's account name (`id -un`), validated as a usable `User=` value.
///
/// Composition-root only: the value never comes from `$USER`/`$LOGNAME`, and an unusable name
/// fails closed in one line.
pub fn account_name() -> Result<String, String> {
    let name = run_probe_tool("id", &["-un"])?.trim().to_owned();
    validate_account_name(&name).map_err(|_| {
        format!(
            "cannot resolve the developer account name: `id -un` returned an unusable value '{name}'"
        )
    })?;
    Ok(name)
}

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

/// The invoking process's real OS identity, resolved once at a Composition Root.
///
/// `uid`/`gid` are the running process's (`geteuid`/`getegid`), `account` is `id -un`, and `home`
/// is that account's passwd row — so `root` running `sudo slipfork daemon install` resolves as
/// `root` here while the *target* account still comes from `SUDO_USER`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserIdentity {
    /// Effective uid of the running process.
    pub uid: u32,
    /// Effective gid of the running process.
    pub gid: u32,
    /// Absolute home directory from this account's passwd row.
    pub home: PathBuf,
    /// Account name systemd must place in `User=` (`remy`, `ci`, ...).
    pub account: String,
}

impl UserIdentity {
    /// Resolve this process's real identity: kernel uid/gid, `id -un`, and the passwd home.
    ///
    /// Composition-root only. Fails closed in one line when the account name is unusable or the
    /// host account database has no absolute home for it.
    pub fn resolve() -> Result<Self, String> {
        let account = account_name()?;
        let (uid, gid) = current_uid_gid();
        let (_, _, home) = passwd_row(&account)?;
        Ok(Self {
            uid,
            gid,
            home,
            account,
        })
    }

    /// Resolve the account the units run as: `--user`, else the sudo invoking account, else self.
    ///
    /// Shared by `daemon unit --user` and `daemon install`, so both render from one derivation.
    /// Direct root (no `--user`, no `SUDO_USER`) has no target account and fails closed.
    pub fn target_account(
        &self,
        is_root: bool,
        explicit_user: Option<&str>,
        invoking_user: Option<&str>,
    ) -> Result<ResolvedAccount, String> {
        if let Some(account) = explicit_user {
            return lookup_account(account);
        }
        if !is_root {
            return Ok(self.as_target());
        }
        match invoking_user {
            Some(account) => lookup_account(account),
            None => Err(UNPRIVILEGED_ACCOUNT_REMEDY.to_owned()),
        }
    }

    /// This already-resolved identity as a target (no second account-database read).
    fn as_target(&self) -> ResolvedAccount {
        ResolvedAccount {
            name: self.account.clone(),
            uid: self.uid,
            gid: self.gid,
            home: self.home.clone(),
        }
    }
}

/// Resolve one install-target account from the host account database (`getent passwd`).
///
/// Rejects unusable names (`%` specifiers, whitespace, control characters), unknown accounts,
/// accounts without an absolute home, and `uid == 0` (the daemon must never run as root).
pub fn lookup_account(account: &str) -> Result<ResolvedAccount, String> {
    validate_account_name(account)?;
    let (uid, gid, home) = passwd_row(account)?;
    if uid == 0 {
        return Err(UNPRIVILEGED_ACCOUNT_REMEDY.to_owned());
    }
    Ok(ResolvedAccount {
        name: account.to_owned(),
        uid,
        gid,
        home,
    })
}

/// Read one `getent passwd` row as `(uid, gid, home)`, failing closed on an unusable row.
fn passwd_row(account: &str) -> Result<(u32, u32, PathBuf), String> {
    validate_account_name(account)?;
    let passwd = run_probe_tool("getent", &["passwd", account])
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
    Ok((uid, gid, home))
}

/// This machine's kernel node name (`uname -n`) for the `open --remote` recipe.
///
/// The value is the kernel's, never the ambient `$HOSTNAME`. An unusable node name becomes the
/// empty string, which [`remote_recipe`](crate::open::remote_recipe) refuses in one line, so a
/// recipe can never name a host the developer did not get.
pub fn kernel_hostname() -> String {
    let name = run_probe_tool("uname", &["-n"]).unwrap_or_default();
    let name = name.trim();
    if name.is_empty()
        || name.contains('@')
        || name
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return String::new();
    }
    name.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The kernel's own answer, so an assertion never restates the code under test.
    fn real_id(flag: &str) -> u32 {
        let output = Command::new("id").arg(flag).output().expect("run `id`");
        String::from_utf8(output.stdout)
            .expect("utf8 id output")
            .trim()
            .parse()
            .expect("a numeric id")
    }

    #[test]
    fn resolves_the_real_invoking_identity_from_the_kernel() {
        let identity = UserIdentity::resolve().expect("host identity");
        assert_eq!(identity.account, account_name().expect("account name"));
        assert_eq!(identity.uid, real_id("-u"));
        assert_eq!(identity.gid, real_id("-g"));
        assert!(identity.home.is_absolute());
    }

    #[test]
    fn kernel_hostname_is_the_real_node_name() {
        let output = Command::new("uname").arg("-n").output().expect("uname -n");
        let expected = String::from_utf8(output.stdout).expect("utf8 node name");
        assert_eq!(kernel_hostname(), expected.trim());
    }

    #[test]
    fn lookup_returns_the_real_accounts_home_and_identity() {
        let identity = UserIdentity::resolve().expect("host identity");
        let account = lookup_account(&identity.account).expect("host lookup");
        assert_eq!(account.name, identity.account);
        assert_eq!(account.uid, real_id("-u"));
        assert_eq!(account.gid, real_id("-g"));
        assert_eq!(account.home, identity.home);
    }

    #[test]
    fn a_second_real_row_is_distinct_from_the_runner() {
        let identity = UserIdentity::resolve().expect("host identity");
        let row = lookup_account("nobody")
            .unwrap_or_else(|err| panic!("this suite needs the real `nobody` passwd row: {err}"));
        assert_ne!(row.uid, identity.uid, "the target row is never the runner");
        assert!(row.home.is_absolute());
    }

    #[test]
    fn root_and_template_and_unknown_accounts_fail_closed() {
        assert_eq!(
            lookup_account("root").expect_err("root is refused"),
            UNPRIVILEGED_ACCOUNT_REMEDY
        );
        for refused in ["%i", "%u", "", "dev user", "dev\tuser"] {
            let error = lookup_account(refused).expect_err("unusable names are refused");
            assert!(
                error.contains("%") || error.contains("usable"),
                "the refusal must name the rule: {error}"
            );
        }
        let unknown =
            lookup_account("slipfork-no-such-account").expect_err("unknown accounts are refused");
        assert!(
            unknown.contains("no OS account") && unknown.contains("slipfork-no-such-account"),
            "the refusal must name the missing account: {unknown}"
        );
    }
}
