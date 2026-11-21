//! Metadata accumulated by `ENV`/`WORKDIR`/`USER`/`EXPOSE`/`CMD`/`ENTRYPOINT`.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use crate::digest::hex;

use super::instruction::{Command, Instruction, Port, Protocol};

/// Runtime metadata of the image produced by a stage.
#[derive(Debug, Clone, PartialEq, Eq)]
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

impl ImageConfig {
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
