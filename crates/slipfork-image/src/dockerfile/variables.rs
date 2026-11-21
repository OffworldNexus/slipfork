//! Build-time `ARG` values and `$NAME`/`${NAME}` substitution.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use crate::digest::hex;

/// A resolved build-time argument value (never secret: the `Forkfile` compiler rejects taint).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildArgs {
    values: BTreeMap<String, String>,
}

impl BuildArgs {
    /// No declared arguments.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Declares one already-validated non-secret `arg` value.
    ///
    /// The `Forkfile` compiler is the only producer: it rejects `secret(...)` and tainted
    /// properties for build-time `arg`, so a tainted value can never reach this method.
    pub fn declare(&mut self, name: impl Into<String>, value: impl Into<String>) -> &mut Self {
        self.values.insert(name.into(), value.into());
        self
    }

    /// Every declared `name`/`value` pair.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &String)> {
        self.values.iter()
    }

    /// The declared value for `name`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    /// Stable `name=value` fingerprint for cache keys.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        let mut hasher = Sha256::new();
        for (name, value) in &self.values {
            hasher.update(name.as_bytes());
            hasher.update(b"=");
            hasher.update(value.as_bytes());
            hasher.update(b"\0");
        }
        hex(&hasher.finalize())
    }
}

/// `ARG`/`ENV` substitution for instruction arguments.
#[derive(Debug, Clone, Default)]
pub struct Variables {
    values: BTreeMap<String, String>,
}

impl Variables {
    /// Builds the substitution table from declared args and accumulated `ENV` values.
    #[must_use]
    pub fn new(args: &BuildArgs, env: &BTreeMap<String, String>) -> Self {
        Self {
            values: args
                .values
                .iter()
                .chain(env)
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        }
    }

    /// Replaces `$NAME` / `${NAME}` for known keys; unknown references stay verbatim.
    #[must_use]
    pub fn expand(&self, text: &str) -> String {
        let bytes = text.as_bytes();
        let mut out = String::with_capacity(text.len());
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] != b'$' {
                out.push(bytes[index] as char);
                index += 1;
                continue;
            }
            let (name, end) = if bytes.get(index + 1) == Some(&b'{') {
                match text[index + 2..].find('}') {
                    Some(close) => (&text[index + 2..index + 2 + close], index + 3 + close),
                    None => {
                        out.push('$');
                        index += 1;
                        continue;
                    }
                }
            } else {
                let mut end = index + 1;
                while end < bytes.len()
                    && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_')
                {
                    end += 1;
                }
                if end == index + 1 {
                    out.push('$');
                    index += 1;
                    continue;
                }
                (&text[index + 1..end], end)
            };
            match self.values.get(name) {
                Some(value) => out.push_str(value),
                None => out.push_str(&text[index..end]),
            }
            index = end;
        }
        out
    }
}
