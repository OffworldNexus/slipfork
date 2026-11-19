//! `slipfork`: the thin CLI client (`H-010` #1, `ADR-0008` §4).
//!
//! The CLI holds **no** `WorldEngine` or `SecretProvider` for world operations: it parses `clap`
//! arguments, resolves `HOME`/`$XDG_*`/`--state-dir` once, and drives `slipforkd` through the typed
//! `<state_dir>/run/daemon.sock` control client. Only `up` and `fork` auto-spawn
//! `slipforkd --listen 127.0.0.1:0` in the background; every read/diagnostic verb fails closed with
//! one actionable line when nothing answers (`ADR-0011` §2–§3).
//!
//! This file is the single composition root: the only place that reads `HOME`, `$XDG_*`, `SHELL`,
//! `PATH`, `DISPLAY`, `WAYLAND_DISPLAY`, `SUDO_USER`, the current directory, `current_exe`, or the
//! terminal, and it snapshots those values into [`Ui`] once so no command body ever probes ambient
//! host state. `snow resolve` runs through `slipforkd` (`DaemonControl::snow_resolve`), so the daemon
//! owns the `0600` local vault and the in-memory OSPP environment. Only `doctor` stays CLI-local: it
//! reads host capabilities without materializing a world.

#![forbid(unsafe_code)]

mod commands;
pub(crate) mod entry;

use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::json;
use slipfork::browser::Browser;
use slipfork::cli::{Cli, Commands};
use slipfork::layout::{Layout, absolute_path};
use slipfork::ui::{Capabilities, TerminalUi};
use slipfork::user::{ResolvedAccount, UserIdentity, account_name, kernel_hostname};
use slipfork_engine::current_uid_gid;

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().collect();
    let tty = std::io::stdout().is_terminal();
    let no_color = std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty());
    let cli = Cli::parse_styled(&args, tty && !no_color);
    let capabilities = Capabilities {
        tty,
        color: !no_color,
        verbose: cli.verbose,
        json: cli.json,
    };
    let json = capabilities.json;
    let outcome = CliApp::compose(cli, capabilities)
        .and_then(|(app, command)| commands::dispatch(&app, command));
    match outcome {
        Ok(code) => code,
        Err(message) => {
            if json {
                println!("{}", json!({ "ok": false, "error": message }));
            } else {
                eprintln!("slipfork: {message}");
            }
            ExitCode::from(1)
        }
    }
}

/// The composed client: every resolved host fact a command body needs, read once.
pub(crate) struct CliApp {
    pub(crate) output_mode: OutputMode,
    pub(crate) layout: Layout,
    pub(crate) terminal: TerminalUi,
    pub(crate) ui: Ui,
}

/// The output contract: `--json` writes only the serialized document.
pub(crate) struct OutputMode {
    pub(crate) json: bool,
}

/// Ambient host facts, snapshotted by [`CliApp::compose`] and never re-read.
pub(crate) struct Ui {
    /// stdin is an interactive terminal (`shell`, `db`, `redis`, `hook run`, `secret set`).
    pub(crate) stdin_tty: bool,
    /// The invoking directory every daemon request carries.
    cwd: Result<PathBuf, String>,
    /// The developer's login shell (`slipfork shell`), or the one-line refusal.
    shell: Result<String, String>,
    /// Directory of this `slipfork` executable (never a `PATH` guess).
    exe_dir: Result<PathBuf, String>,
    /// `DISPLAY`/`WAYLAND_DISPLAY` + `PATH`, resolved into the one desktop launcher.
    browser: Browser,
    /// The one `SUDO_USER` read in the workspace (`ADR-0011` §1).
    invoking_account: Option<String>,
    /// Explicit `--state-dir`, forwarded verbatim to `daemon unit` / `daemon install`.
    pub(crate) state_dir: Option<PathBuf>,
    pub(crate) xdg_cache_home: Option<PathBuf>,
    pub(crate) xdg_data_home: Option<PathBuf>,
}

impl CliApp {
    /// Resolve every ambient host fact once, then hand the parsed verb to the dispatch table.
    fn compose(cli: Cli, capabilities: Capabilities) -> Result<(Self, Option<Commands>), String> {
        let home = home_dir()?;
        let xdg_cache = absolute_path(std::env::var_os("XDG_CACHE_HOME"));
        let xdg_data = absolute_path(std::env::var_os("XDG_DATA_HOME"));
        let layout = Layout::resolve(
            cli.state_dir.clone(),
            &home,
            xdg_cache.as_deref(),
            xdg_data.as_deref(),
        );
        let terminal = TerminalUi::new(capabilities);
        let output_mode = OutputMode {
            json: capabilities.json,
        };
        let ui = Ui {
            stdin_tty: std::io::stdin().is_terminal(),
            cwd: current_dir(),
            shell: shell_from_env(),
            exe_dir: exe_dir(),
            browser: browser_from_env(),
            invoking_account: invoking_account(),
            state_dir: cli.state_dir,
            xdg_cache_home: xdg_cache,
            xdg_data_home: xdg_data,
        };
        Ok((
            Self {
                output_mode,
                layout,
                terminal,
                ui,
            },
            cli.command,
        ))
    }
}

/// `HOME` is required before any side effect.
fn home_dir() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            "HOME is not set; slipfork stores CoW state, the 0600 vault, and its runtime socket \
             under the developer account's home directory — set HOME and retry"
                .to_owned()
        })
}

/// The invoking directory, resolved once; a failure surfaces only when a verb needs it.
fn current_dir() -> Result<PathBuf, String> {
    std::env::current_dir().map_err(|err| format!("cannot read the current directory: {err}"))
}

/// The developer's login shell, or the one-line refusal `slipfork shell` prints.
fn shell_from_env() -> Result<String, String> {
    std::env::var("SHELL")
        .ok()
        .filter(|shell| !shell.trim().is_empty())
        .ok_or_else(|| {
            "SHELL is not set; `slipfork shell` enters the world with the developer's shell — set SHELL (for example /bin/sh) and retry".to_owned()
        })
}

/// Directory of this `slipfork` executable (never a `PATH` guess, never a `/usr/bin` fallback).
fn exe_dir() -> Result<PathBuf, String> {
    let executable =
        std::env::current_exe().map_err(|err| format!("cannot resolve this executable: {err}"))?;
    executable.parent().map(Path::to_path_buf).ok_or_else(|| {
        format!(
            "this executable {} has no parent directory",
            executable.display()
        )
    })
}

/// `DISPLAY`/`WAYLAND_DISPLAY` and `PATH`, resolved into the one desktop launcher.
fn browser_from_env() -> Browser {
    Browser::resolve(
        std::env::var_os("PATH").as_deref(),
        ["DISPLAY", "WAYLAND_DISPLAY"]
            .into_iter()
            .any(|var| std::env::var_os(var).is_some_and(|value| !value.is_empty())),
    )
}

/// The one `SUDO_USER` read in the workspace.
fn invoking_account() -> Option<String> {
    std::env::var_os("SUDO_USER")
        .map(|value| value.to_string_lossy().trim().to_owned())
        .filter(|name| !name.is_empty())
}

impl Ui {
    /// The invoking directory every daemon request carries.
    pub(crate) fn cwd(&self) -> Result<PathBuf, String> {
        self.cwd.clone()
    }

    /// The developer's login shell, the one `slipfork shell` enters with.
    pub(crate) fn shell(&self) -> Result<String, String> {
        self.shell.clone()
    }

    /// Directory of this `slipfork` executable.
    pub(crate) fn exe_dir(&self) -> Result<PathBuf, String> {
        self.exe_dir.clone()
    }

    /// The `slipforkd` executable installed next to this CLI (never a `PATH` guess).
    pub(crate) fn daemon_executable(&self) -> Result<PathBuf, String> {
        let candidate = self.exe_dir()?.join("slipforkd");
        if !candidate.is_file() {
            return Err(format!(
                "cannot find slipforkd at {}; build or install the slipforkd binary next to slipfork",
                candidate.display()
            ));
        }
        Ok(candidate)
    }

    /// The one desktop launcher resolved from the snapshotted `PATH`/display facts.
    pub(crate) fn browser(&self) -> Browser {
        self.browser.clone()
    }

    /// The resolved account/host facts `open --remote` and `daemon` render.
    pub(crate) fn identity(&self) -> Result<UserIdentity, String> {
        UserIdentity::resolve()
    }

    /// The account named by a non-root `daemon install` refusal.
    pub(crate) fn account_name(&self) -> Result<String, String> {
        account_name()
    }

    /// The `uname -n` host name (never `$HOSTNAME`).
    pub(crate) fn kernel_hostname(&self) -> String {
        kernel_hostname()
    }

    /// The one `SUDO_USER` read in the workspace (`ADR-0011` §1).
    pub(crate) fn invoking_account(&self) -> Option<String> {
        self.invoking_account.clone()
    }

    /// The invoking uid/gid pair used by `doctor` and the `daemon install` root gate.
    pub(crate) fn uid_gid(&self) -> (u32, u32) {
        current_uid_gid()
    }

    /// The layout of another account, derived from the client's own root overrides.
    pub(crate) fn layout_for(&self, account: &ResolvedAccount) -> Layout {
        Layout::for_account(
            account,
            self.state_dir.clone(),
            self.xdg_cache_home.as_deref(),
            self.xdg_data_home.as_deref(),
        )
    }
}
