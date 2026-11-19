//! `slipfork secret`: the project's `0600` vault through the daemon, never printing values.
//!
//! A value is read from exactly two places: a hidden interactive prompt on a terminal (echo is
//! turned off with `stty` and always restored), or piped stdin. Neither path ever echoes the value,
//! and an empty read fails closed with the one-line pipe remedy instead of storing `""`.

use std::io::{self, Read, Write};
use std::process::{Command, ExitCode, Stdio};

use serde_json::json;
use slipfork::cli::SecretAction;
use slipfork_edge::DaemonControl;

use crate::CliApp;
use crate::commands::control::connect;
use crate::commands::render_json;

/// Store, remove, or list the project's secret keys.
pub(crate) fn secret(app: &CliApp, action: SecretAction) -> Result<ExitCode, String> {
    let client = connect(&app.layout)?;
    let cwd = app.ui.cwd()?;
    match action {
        SecretAction::Set { key, force } => {
            let value = read_secret(app, &key)?;
            client
                .secret_set(&cwd, &key, &value, force)
                .map_err(|err| err.to_string())?;
            if app.output_mode.json {
                println!("{}", json!({ "ok": true, "key": key }));
            } else {
                println!("[secret] stored secrets::{key} (value never printed)");
            }
        }
        SecretAction::Unset { key } => {
            let removed = client
                .secret_unset(&cwd, &key)
                .map_err(|err| err.to_string())?;
            if app.output_mode.json {
                println!("{}", json!({ "ok": true, "key": key, "removed": removed }));
            } else if removed {
                println!("[secret] removed secrets::{key}");
            } else {
                println!("[secret] secrets::{key} was not stored; nothing to remove");
            }
        }
        SecretAction::Keys => {
            let keys = client.secret_list(&cwd).map_err(|err| err.to_string())?;
            if app.output_mode.json {
                println!("{}", render_json(&keys)?);
            } else {
                for key in keys {
                    println!("{key}");
                }
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Read one secret: a hidden prompt on a terminal, piped stdin otherwise.
fn read_secret(app: &CliApp, key: &str) -> Result<String, String> {
    if app.ui.stdin_tty {
        return read_hidden(key);
    }
    let mut stdin = io::stdin().lock();
    read_piped(&mut stdin, key)
}

/// Prompt on the terminal with echo disabled for the duration of the read.
///
/// The prompt itself is visible (the developer must know what is asked); the value is not echoed
/// by the terminal, and [`EchoGuard`] restores echo on every exit path, including an early return.
fn read_hidden(key: &str) -> Result<String, String> {
    let mut stderr = io::stderr();
    write!(stderr, "Secret value for {key}: ").map_err(|err| err.to_string())?;
    stderr.flush().map_err(|err| err.to_string())?;
    let guard = EchoGuard::disable(key)?;
    let mut line = String::new();
    let read = io::stdin().read_line(&mut line);
    drop(guard);
    // The hidden value never wrote a newline of its own, so finish the prompt line.
    eprintln!();
    read.map_err(|err| format!("cannot read the secret from the terminal: {err}"))?;
    let value = trim_secret(line);
    if value.is_empty() {
        return Err(format!(
            "the secret entered for {key} is empty\n  next: retry and type a value, or pipe it: printf '%s' \"$VALUE\" | slipfork secret set {key}"
        ));
    }
    Ok(value)
}

/// Read and trim one piped secret, refusing an empty stream with the one-line remedy.
fn read_piped(reader: &mut impl Read, key: &str) -> Result<String, String> {
    let mut value = String::new();
    reader
        .read_to_string(&mut value)
        .map_err(|err| format!("cannot read the secret from stdin: {err}"))?;
    let value = trim_secret(value);
    if value.is_empty() {
        return Err(format!(
            "the secret read from stdin is empty\n  next: printf '%s' \"$VALUE\" | slipfork secret set {key}"
        ));
    }
    Ok(value)
}

/// Strip the trailing line ending exactly once (a value may legitimately contain newlines).
fn trim_secret(mut value: String) -> String {
    while value.ends_with('\n') || value.ends_with('\r') {
        value.pop();
    }
    value
}

/// Echo-off for one terminal read: `stty` owns the terminal state, and `Drop` always restores it.
struct EchoGuard;

impl EchoGuard {
    /// Turn terminal echo off, or fail closed naming the pipe alternative (no silent echo).
    fn disable(key: &str) -> Result<Self, String> {
        set_echo(false).map_err(|err| {
            format!(
                "cannot hide the {key} prompt because terminal echo could not be disabled: {err}\n  next: pipe the value instead: printf '%s' \"$VALUE\" | slipfork secret set {key}"
            )
        })?;
        Ok(Self)
    }
}

impl Drop for EchoGuard {
    fn drop(&mut self) {
        let _ = set_echo(true);
    }
}

/// Toggle the controlling terminal's echo through `stty` (which reads this process's stdin TTY).
fn set_echo(on: bool) -> Result<(), String> {
    let flag = if on { "echo" } else { "-echo" };
    let status = Command::new("stty")
        .arg(flag)
        .stdin(Stdio::inherit())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|err| err.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("stty {flag} exited with {status}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_piped_value_is_trimmed_and_never_echoed() {
        let mut input: &[u8] = b"super-secret-value\r\n";
        assert_eq!(
            read_piped(&mut input, "KEY").expect("piped value"),
            "super-secret-value"
        );
        // An interior newline is part of the value; only the trailing line ending is removed.
        let mut multiline: &[u8] = b"a\nb\n";
        assert_eq!(
            read_piped(&mut multiline, "KEY").expect("piped value"),
            "a\nb"
        );
    }

    #[test]
    fn an_empty_pipe_refuses_with_one_line_and_the_next_remedy() {
        let mut empty: &[u8] = b"";
        let error = read_piped(&mut empty, "STRIPE_KEY").expect_err("empty input is refused");
        assert_eq!(error.lines().count(), 2, "one line plus one next: {error}");
        assert!(
            error.starts_with("the secret read from stdin is empty"),
            "{error}"
        );
        assert!(
            error.contains("next: printf '%s' \"$VALUE\" | slipfork secret set STRIPE_KEY"),
            "{error}"
        );
    }
}
