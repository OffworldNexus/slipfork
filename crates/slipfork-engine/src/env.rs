//! Self-contained, redacting child-process environment for a world (`ADR-0008` §7).
//!
//! A world never inherits the daemon's ambient environment wholesale. It starts from a
//! **restricted inherited allowlist** ([`InheritedVar`]: `PATH`, `TERM`, `LANG`) plus a
//! **world-scoped** `HOME`, and the rest of the environment is composed of plain `vars` and
//! `secrets` resolved by `slipforkd`. Secret values leave this type through exactly two explicitly
//! named boundaries:
//!
//! 1. `ExecEnv::expose_to_child_process` — the `Command::env_clear().envs(..)` hand-off
//!    (crate-private, so only the engine's child-process spawners can call it).
//! 2. `serde` — the `0600` `<runtime_dir>/daemon.sock` wire form carried only inside
//!    `DaemonControl::exec_target`'s `WorldExecTarget`.
//!
//! Every other surface (including [`std::fmt::Debug`]) redacts every secret value.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::EngineError;

/// Environment variables a world may inherit from the process that materializes it.
///
/// The allowlist is a closed enum (never a free-form string set) so no caller can widen the
/// inherited surface by passing a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum InheritedVar {
    /// Executable search path.
    Path,
    /// Terminal type.
    Term,
    /// Locale.
    Lang,
}

impl InheritedVar {
    /// Every inherit-allowed variable, in stable order.
    pub const ALL: [Self; 3] = [Self::Path, Self::Term, Self::Lang];

    /// Canonical environment variable name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Path => "PATH",
            Self::Term => "TERM",
            Self::Lang => "LANG",
        }
    }

    /// Parse an environment variable name, returning `None` for anything outside the allowlist.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|var| var.name() == name)
    }
}

/// Placeholder printed instead of every secret value.
pub const REDACTED: &str = "[REDACTED]";

/// Effective child environment for a world.
///
/// `effective = base ⊕ overlay` (the overlay wins for every class of entry), overlays never mix a
/// plain and a secret binding of the same name, and the `HOME` override is world-scoped.
///
/// # Wire form
///
/// `Serialize`/`Deserialize` exist **solely** for `DaemonControl::exec_target`, whose JSON-line
/// frame is the only message permitted to carry plaintext secrets and only over the `0600`
/// `<runtime_dir>/daemon.sock`. Never log, print, or persist the serialized form.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "ExecEnvWire", into = "ExecEnvWire")]
pub struct ExecEnv {
    home: Option<PathBuf>,
    inherited: BTreeMap<InheritedVar, String>,
    plain: BTreeMap<String, String>,
    secrets: BTreeMap<String, String>,
}

impl ExecEnv {
    /// Start a world-scoped environment whose `HOME` is `home` (never the developer's real home).
    pub fn world_scoped(home: impl Into<PathBuf>) -> Self {
        Self {
            home: Some(home.into()),
            ..Self::default()
        }
    }

    /// World-scoped `HOME`, if set.
    pub fn home(&self) -> Option<&Path> {
        self.home.as_deref()
    }

    /// Record the value of an inherit-allowed variable explicitly.
    pub fn inherit(&mut self, var: InheritedVar, value: impl Into<String>) -> &mut Self {
        self.inherited.insert(var, value.into());
        self
    }

    /// Bind a non-secret variable.
    pub fn set(&mut self, key: &str, value: impl Into<String>) -> Result<&mut Self, EngineError> {
        validate_env_name(key)?;
        if matches!(key, "HOME") || InheritedVar::parse(key).is_some() {
            return Err(EngineError::InvalidEnvName {
                name: key.to_owned(),
                reason: "is reserved; use ExecEnv::world_scoped(HOME) or ExecEnv::inherit(..)"
                    .to_owned(),
            });
        }
        self.secrets.remove(key);
        self.plain.insert(key.to_owned(), value.into());
        Ok(self)
    }

    /// Bind a secret variable (its value is only released at a documented boundary).
    pub fn set_secret(
        &mut self,
        key: &str,
        value: impl Into<String>,
    ) -> Result<&mut Self, EngineError> {
        validate_env_name(key)?;
        if matches!(key, "HOME") || InheritedVar::parse(key).is_some() {
            return Err(EngineError::InvalidEnvName {
                name: key.to_owned(),
                reason: "is reserved; use ExecEnv::world_scoped(HOME) or ExecEnv::inherit(..)"
                    .to_owned(),
            });
        }
        self.plain.remove(key);
        self.secrets.insert(key.to_owned(), value.into());
        Ok(self)
    }

    /// `true` when nothing is bound (an overlay with no entries).
    pub fn is_empty(&self) -> bool {
        self.home.is_none()
            && self.inherited.is_empty()
            && self.plain.is_empty()
            && self.secrets.is_empty()
    }

    /// Compose this base environment with an overlay (the overlay wins, including a plain/secret
    /// rebinding of the same name).
    pub fn overlaid(&self, overlay: &Self) -> Self {
        let mut out = self.clone();
        if overlay.home.is_some() {
            out.home = overlay.home.clone();
        }
        out.inherited.extend(overlay.inherited.clone());
        for (key, value) in &overlay.plain {
            out.secrets.remove(key);
            out.plain.insert(key.clone(), value.clone());
        }
        for (key, value) in &overlay.secrets {
            out.plain.remove(key);
            out.secrets.insert(key.clone(), value.clone());
        }
        out
    }

    /// Materialize the values handed to `Command::env_clear().envs(..)`.
    ///
    /// **SECURITY:** this is one of the two sanctioned secret-release boundaries. It is
    /// `pub(crate)` so only the engine's child-process spawners ([`crate::child_env`]) can reach
    /// it; every other consumer (including the `exec_target` IPC path) sees redacted `Debug` only.
    pub(crate) fn expose_to_child_process(&self) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        for (var, value) in &self.inherited {
            out.insert(var.name().to_owned(), value.clone());
        }
        if let Some(home) = &self.home {
            out.insert("HOME".to_owned(), home.display().to_string());
        }
        out.extend(self.plain.clone());
        out.extend(self.secrets.clone());
        out
    }
}

impl fmt::Debug for ExecEnv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let inherited: BTreeMap<&'static str, &str> = self
            .inherited
            .iter()
            .map(|(var, value)| (var.name(), value.as_str()))
            .collect();
        let secrets: BTreeMap<&str, &str> = self
            .secrets
            .keys()
            .map(|key| (key.as_str(), REDACTED))
            .collect();
        f.debug_struct("ExecEnv")
            .field("home", &self.home)
            .field("inherited", &inherited)
            .field("plain", &self.plain)
            .field("secrets", &secrets)
            .finish()
    }
}

fn validate_env_name(key: &str) -> Result<(), EngineError> {
    let mut chars = key.chars();
    let valid_start = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    let valid_rest = chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if key.is_empty() || !valid_start || !valid_rest {
        return Err(EngineError::InvalidEnvName {
            name: key.to_owned(),
            reason: "must be a non-empty [A-Za-z_][A-Za-z0-9_]* shell identifier".to_owned(),
        });
    }
    Ok(())
}

/// IPC-only wire shape; validated on the way in so peer invariants cannot be bypassed.
#[derive(Clone, Default, Serialize, Deserialize)]
struct ExecEnvWire {
    home: Option<PathBuf>,
    inherited: BTreeMap<String, String>,
    plain: BTreeMap<String, String>,
    secrets: BTreeMap<String, String>,
}

impl From<ExecEnv> for ExecEnvWire {
    fn from(env: ExecEnv) -> Self {
        Self {
            home: env.home,
            inherited: env
                .inherited
                .into_iter()
                .map(|(var, value)| (var.name().to_owned(), value))
                .collect(),
            plain: env.plain,
            secrets: env.secrets,
        }
    }
}

impl TryFrom<ExecEnvWire> for ExecEnv {
    type Error = String;

    fn try_from(wire: ExecEnvWire) -> Result<Self, Self::Error> {
        let mut inherited = BTreeMap::new();
        for (name, value) in wire.inherited {
            let var = InheritedVar::parse(&name)
                .ok_or_else(|| format!("'{name}' is outside the inherited allowlist"))?;
            inherited.insert(var, value);
        }
        for key in wire.plain.keys().chain(wire.secrets.keys()) {
            validate_env_name(key).map_err(|err| err.to_string())?;
            if matches!(key.as_str(), "HOME") || InheritedVar::parse(key).is_some() {
                return Err(format!(
                    "'{key}' is reserved and cannot be sent as a plain/secret var"
                ));
            }
        }
        if let Some(key) = wire
            .plain
            .keys()
            .find(|key| wire.secrets.contains_key(*key))
        {
            return Err(format!("'{key}' is bound as both a plain and a secret var"));
        }
        Ok(Self {
            home: wire.home,
            inherited,
            plain: wire.plain,
            secrets: wire.secrets,
        })
    }
}
