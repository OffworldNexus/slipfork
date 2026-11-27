//! Metadata accumulated by `ENV`/`WORKDIR`/`USER`/`EXPOSE`/`CMD`/`ENTRYPOINT`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use slipfork_world::ImageFacts;

use crate::digest::hex;

use super::instruction::{Command, Instruction, Port, Protocol};

/// Runtime metadata of the image produced by a stage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageConfig {
    /// Runtime environment of the final image.
    pub env: BTreeMap<String, String>,
    /// Default working directory (`/` until `WORKDIR` sets another).
    pub workdir: String,
    /// Default user (`uid[:gid]`).
    pub user: Option<String>,
    /// Declared ports.
    pub exposed: Vec<Port>,
    /// Default command.
    pub cmd: Option<Command>,
    /// Entrypoint.
    pub entrypoint: Option<Command>,
}

impl Default for ImageConfig {
    fn default() -> Self {
        Self {
            env: BTreeMap::new(),
            workdir: "/".to_owned(),
            user: None,
            exposed: Vec::new(),
            cmd: None,
            entrypoint: None,
        }
    }
}

impl From<&ImageFacts> for ImageConfig {
    /// Seeds stage metadata from a pulled image's OCI `Env`/`Entrypoint`/`Cmd`.
    ///
    /// A later `ENV`, `CMD`, or `ENTRYPOINT` instruction in the stage overwrites the inherited
    /// value, exactly as Docker layers a child image config over its base.
    fn from(facts: &ImageFacts) -> Self {
        let mut config = Self::default();
        for entry in &facts.env {
            if let Some((key, value)) = entry.split_once('=') {
                config.env.insert(key.to_owned(), value.to_owned());
            }
        }
        if !facts.entrypoint.is_empty() {
            config.entrypoint = Some(Command::Exec(facts.entrypoint.clone()));
        }
        if !facts.cmd.is_empty() {
            config.cmd = Some(Command::Exec(facts.cmd.clone()));
        }
        config
    }
}

impl ImageConfig {
    /// Resolves a `WORKDIR` argument against the current directory and stores it.
    ///
    /// Relative directories join the inherited `WORKDIR`, so `WORKDIR /app` followed by
    /// `WORKDIR src` leaves the stage in `/app/src`.
    #[must_use]
    pub fn set_workdir(&mut self, directory: &str) -> String {
        let resolved = if directory.starts_with('/') {
            directory.to_owned()
        } else {
            format!("{}/{}", self.workdir.trim_end_matches('/'), directory)
        };
        self.workdir.clone_from(&resolved);
        resolved
    }

    /// Applies a metadata instruction; returns `false` for filesystem instructions.
    pub fn apply(&mut self, instruction: &Instruction) -> bool {
        match instruction {
            Instruction::Env(pairs) => {
                for (key, value) in pairs {
                    self.env.insert(key.clone(), value.clone());
                }
                true
            }
            Instruction::User(user) => {
                self.user = Some(user.clone());
                true
            }
            Instruction::Expose(ports) => {
                for port in ports {
                    if !self.exposed.contains(port) {
                        self.exposed.push(*port);
                    }
                }
                true
            }
            Instruction::Cmd(command) => {
                self.cmd = Some(command.clone());
                true
            }
            Instruction::Entrypoint(command) => {
                self.entrypoint = Some(command.clone());
                true
            }
            Instruction::Arg { .. } | Instruction::From { .. } => true,
            Instruction::Workdir(_)
            | Instruction::Copy(_)
            | Instruction::Add(_)
            | Instruction::Run(_) => false,
        }
    }

    /// The argv a `service` without `run:` execs: OCI `ENTRYPOINT` then `CMD`.
    ///
    /// Shell-form `ENTRYPOINT` owns the whole runtime command, so `CMD` is ignored, exactly as
    /// the OCI runtime spec requires.
    #[must_use]
    pub fn exec_argv(&self) -> Vec<String> {
        match &self.entrypoint {
            Some(crate::Command::Exec(argv)) => {
                let mut argv = argv.clone();
                if let Some(cmd) = &self.cmd {
                    argv.extend(cmd.to_argv());
                }
                argv
            }
            Some(crate::Command::Shell(script)) => {
                vec!["/bin/sh".to_owned(), "-c".to_owned(), script.clone()]
            }
            None => self
                .cmd
                .as_ref()
                .map(crate::Command::to_argv)
                .unwrap_or_default(),
        }
    }

    /// Stable fingerprint so metadata changes invalidate later filesystem steps.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        let mut hasher = Sha256::new();
        for (key, value) in &self.env {
            hasher.update(key.as_bytes());
            hasher.update(b"=");
            hasher.update(value.as_bytes());
            hasher.update(b"\0");
        }
        hasher.update(self.workdir.as_bytes());
        hasher.update(b"\0");
        if let Some(user) = &self.user {
            hasher.update(user.as_bytes());
        }
        hasher.update(b"\0");
        for port in &self.exposed {
            hasher.update(port.number.to_be_bytes());
            hasher.update(match port.protocol {
                Protocol::Tcp => b"tcp",
                Protocol::Udp => b"udp",
            });
        }
        if let Some(cmd) = &self.cmd {
            hasher.update(cmd.render().as_bytes());
        }
        if let Some(entrypoint) = &self.entrypoint {
            hasher.update(entrypoint.render().as_bytes());
        }
        hex(&hasher.finalize())
    }
}

#[cfg(test)]
mod tests {
    use slipfork_world::ImageFacts;

    use super::{Command, ImageConfig, Instruction};
    use crate::dockerfile::{BuildArgs, Variables};

    fn base_facts() -> ImageFacts {
        ImageFacts {
            env: vec!["PATH=/usr/bin:/bin".to_owned(), "LANG=C.UTF-8".to_owned()],
            entrypoint: vec!["/usr/bin/env".to_owned()],
            cmd: vec![
                "python".to_owned(),
                "-m".to_owned(),
                "app.worker".to_owned(),
            ],
        }
    }

    /// `ENV PATH="/app/.venv/bin:$PATH"` must expand against the base image's real `PATH`, the
    /// inherited `CMD` must survive until the stage overrides it, and a stage `ENTRYPOINT` must
    /// replace the inherited one.
    #[test]
    fn base_image_facts_seed_env_and_inherited_entrypoint_and_cmd() {
        let mut config = ImageConfig::from(&base_facts());
        assert_eq!(
            config.env.get("PATH").map(String::as_str),
            Some("/usr/bin:/bin")
        );
        assert_eq!(
            config.exec_argv(),
            ["/usr/bin/env", "python", "-m", "app.worker"]
        );

        let variables = Variables::new(&BuildArgs::new(), &config.env);
        let expanded = variables.expand("/app/.venv/bin:$PATH");
        assert_eq!(expanded, "/app/.venv/bin:/usr/bin:/bin");
        config.env.insert("PATH".to_owned(), expanded);

        assert!(!config.exec_argv().is_empty(), "the inherited CMD survives");
        config.apply(&Instruction::Cmd(Command::Exec(vec!["node".to_owned()])));
        assert_eq!(config.exec_argv(), ["/usr/bin/env", "node"]);

        let mut overridden = ImageConfig::from(&base_facts());
        overridden.apply(&Instruction::Entrypoint(Command::Shell(
            "exec python3".to_owned(),
        )));
        assert_eq!(overridden.exec_argv(), ["/bin/sh", "-c", "exec python3"]);
    }

    /// `WORKDIR` joins relative directories and never resets a later instruction's view of it.
    #[test]
    fn workdir_joins_relative_directories() {
        let mut config = ImageConfig::default();
        assert_eq!(config.set_workdir("/app"), "/app");
        assert_eq!(config.workdir, "/app");
        assert_eq!(config.set_workdir("src"), "/app/src");
        assert_eq!(config.set_workdir("/opt"), "/opt");
    }
}
