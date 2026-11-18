//! Pure `Dockerfile` stage parser (`ADR-0009` §4.2 rules 1, 3, 4, 6).
//!
//! Slipfork never rewrites the project's `Dockerfile`: it reads the raw bytes, resolves the stage
//! selected by `world.stage` (or the final manual stage when unnamed), checks the static contract
//! rules, and appends one machine-generated stage outside the worktree. This module is pure — no
//! filesystem, no environment.

#![forbid(unsafe_code)]

use slipfork_world::format::quoted;

use crate::rootfs::BaseImageError;

/// One logical `Dockerfile` instruction of the target stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Instruction {
    /// Upper-cased keyword (`ENV`, `EXPOSE`, ...).
    pub keyword: String,
    /// Everything after the keyword.
    pub args: String,
}

/// One `FROM ... AS <name>` stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Stage {
    /// `AS <name>` value when declared.
    pub name: Option<String>,
    /// Base image reference (with `--platform` flags stripped).
    pub from: String,
    /// 1-based line of the `FROM` instruction.
    pub line: u32,
    /// Instructions that belong to this stage (excluding `FROM`).
    pub instructions: Vec<Instruction>,
}

impl Stage {
    /// First instruction value for `keyword`, if the stage declares it.
    pub fn instruction(&self, keyword: &str) -> Option<&str> {
        self.instructions
            .iter()
            .find(|instruction| instruction.keyword == keyword)
            .map(|instruction| instruction.args.as_str())
    }

    /// Every instruction with `keyword`.
    pub fn instructions_for<'a>(
        &'a self,
        keyword: &'a str,
    ) -> impl Iterator<Item = &'a Instruction> {
        self.instructions
            .iter()
            .filter(move |instruction| instruction.keyword == keyword)
    }
}

/// Parsed `Dockerfile`, in declaration order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Dockerfile {
    /// Every build stage.
    pub stages: Vec<Stage>,
}

impl Dockerfile {
    /// Parse raw `Dockerfile` bytes.
    pub fn parse(bytes: &[u8]) -> Result<Self, BaseImageError> {
        let text = String::from_utf8_lossy(bytes);
        let mut stages: Vec<Stage> = Vec::new();
        let mut logical: Option<(u32, String)> = None;

        for (index, physical) in text.lines().enumerate() {
            let line = index as u32 + 1;
            let trimmed = physical.trim_end();
            let continued = trimmed.ends_with('\\');
            let fragment = trimmed.strip_suffix('\\').unwrap_or(trimmed);
            match &mut logical {
                Some((_, buffer)) => {
                    buffer.push(' ');
                    buffer.push_str(fragment.trim());
                }
                None => logical = Some((line, fragment.trim().to_owned())),
            }
            if continued {
                continue;
            }
            let Some((start, raw)) = logical.take() else {
                continue;
            };
            let raw = raw.trim();
            if raw.is_empty() || raw.starts_with('#') {
                continue;
            }
            parse_instruction(&mut stages, start, raw)?;
        }

        if let Some((start, raw)) = logical {
            let raw = raw.trim();
            if !raw.is_empty() && !raw.starts_with('#') {
                parse_instruction(&mut stages, start, raw)?;
            }
        }

        if stages.is_empty() {
            return Err(BaseImageError::DockerfileSyntax {
                line: 1,
                reason: "no `FROM` stage declared".to_owned(),
            });
        }
        Ok(Self { stages })
    }

    /// Resolve the requested stage (`world.stage`), failing closed with `ADR-0009` §5.4/§5.5.
    ///
    /// `None` selects the final stage and requires it to be named; an explicit selector must match
    /// a declared stage name exactly.
    pub fn target(&self, requested: Option<&str>) -> Result<&Stage, BaseImageError> {
        let names: Vec<String> = self
            .stages
            .iter()
            .filter_map(|stage| stage.name.clone())
            .collect();
        match requested {
            Some(stage) => self
                .stages
                .iter()
                .find(|candidate| candidate.name.as_deref() == Some(stage))
                .ok_or_else(|| BaseImageError::UnknownTargetStage {
                    stage: stage.to_owned(),
                    declared: quoted(&names),
                    suggested: names.first().cloned().unwrap_or_default(),
                }),
            None => {
                let final_stage = self.stages.last().expect("parser rejects empty Dockerfile");
                match &final_stage.name {
                    Some(_) => Ok(final_stage),
                    None => Err(BaseImageError::UnnamedTargetStage {
                        line: final_stage.line,
                        from: final_stage.from.clone(),
                    }),
                }
            }
        }
    }
}

fn parse_instruction(stages: &mut Vec<Stage>, line: u32, raw: &str) -> Result<(), BaseImageError> {
    let (keyword, args) = match raw.split_once(char::is_whitespace) {
        Some((keyword, args)) => (keyword, args.trim()),
        None => (raw, ""),
    };
    let keyword = keyword.to_ascii_uppercase();

    if keyword == "FROM" {
        let mut tokens = args.split_whitespace();
        let mut base = tokens.next().unwrap_or_default();
        while base.starts_with("--") {
            base = tokens.next().unwrap_or_default();
        }
        if base.is_empty() {
            return Err(BaseImageError::DockerfileSyntax {
                line,
                reason: "`FROM` needs a base image".to_owned(),
            });
        }
        let mut name = None;
        let rest: Vec<&str> = tokens.collect();
        if rest.len() >= 2 && rest[0].eq_ignore_ascii_case("as") {
            name = Some(rest[1].to_owned());
        }
        stages.push(Stage {
            name,
            from: base.to_owned(),
            line,
            instructions: Vec::new(),
        });
        return Ok(());
    }

    let Some(stage) = stages.last_mut() else {
        if keyword == "ARG" || keyword == "COMMENT" {
            return Ok(());
        }
        return Err(BaseImageError::DockerfileSyntax {
            line,
            reason: format!("`{keyword}` appears before any `FROM`"),
        });
    };
    stage.instructions.push(Instruction {
        keyword,
        args: args.to_owned(),
    });
    Ok(())
}

/// Split `ENV`/`ARG` arguments into key/value pairs, honoring quotes.
pub(crate) fn key_value_pairs(args: &str) -> Vec<(String, String)> {
    let tokens = shell_tokens(args);
    if tokens.is_empty() {
        return Vec::new();
    }
    if tokens.iter().any(|token| token.contains('=')) {
        return tokens
            .into_iter()
            .filter_map(|token| {
                token
                    .split_once('=')
                    .map(|(key, value)| (key.to_owned(), value.to_owned()))
            })
            .collect();
    }
    let key = tokens[0].clone();
    let value = tokens[1..].join(" ");
    vec![(key, value)]
}

/// Argv of an `ENTRYPOINT`/`CMD`: exec-form JSON array, else shell-form whitespace split.
pub(crate) fn argv(args: &str) -> Vec<String> {
    let trimmed = args.trim();
    if trimmed.starts_with('[')
        && let Ok(parsed) = serde_json::from_str::<Vec<String>>(trimmed)
    {
        return parsed;
    }
    shell_tokens(trimmed)
}

/// Minimal quote-aware whitespace tokenizer (single, double quotes and backslash escapes).
fn shell_tokens(args: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for ch in args.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' if quote != Some('\'') => escaped = true,
            '"' | '\'' => {
                if quote == Some(ch) {
                    quote = None;
                } else if quote.is_none() {
                    quote = Some(ch);
                } else {
                    current.push(ch);
                }
            }
            ch if ch.is_whitespace() && quote.is_none() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            ch => current.push(ch),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::{Dockerfile, argv, key_value_pairs};

    #[test]
    fn parses_named_stages_and_continuations() {
        let dockerfile = Dockerfile::parse(
            b"ARG BASE=python:3.14-slim-trixie\nFROM ${BASE} AS builder\nRUN echo one \\\n  two\nFROM builder AS runtime\nEXPOSE 8000\n",
        )
        .expect("valid Dockerfile");
        assert_eq!(
            dockerfile
                .stages
                .iter()
                .map(|stage| stage.name.as_deref())
                .collect::<Vec<_>>(),
            [Some("builder"), Some("runtime")]
        );
        assert_eq!(dockerfile.stages[1].from, "builder");
        assert_eq!(dockerfile.stages[1].instruction("EXPOSE"), Some("8000"));
        assert_eq!(
            dockerfile.stages[0].instructions[0].args, "echo one two",
            "continuations join into one logical instruction"
        );
    }

    #[test]
    fn target_selection_fails_closed() {
        let bare = Dockerfile::parse(b"FROM alpine:3.21\n").expect("valid");
        assert!(bare.target(None).is_err());
        let named = Dockerfile::parse(b"FROM alpine:3.21 AS app\n").expect("valid");
        assert_eq!(
            named.target(None).expect("final named").name.as_deref(),
            Some("app")
        );
        assert!(named.target(Some("dev")).is_err());
    }

    #[test]
    fn env_and_argv_forms() {
        assert_eq!(
            key_value_pairs("BASE_URL=http://localhost:8000 PATH=/usr/bin"),
            [
                ("BASE_URL".to_owned(), "http://localhost:8000".to_owned()),
                ("PATH".to_owned(), "/usr/bin".to_owned()),
            ]
        );
        assert_eq!(
            key_value_pairs("MESSAGE hello world"),
            [("MESSAGE".to_owned(), "hello world".to_owned())]
        );
        assert_eq!(
            argv(r#"["docker-entrypoint.sh", "gosu", "appuser"]"#),
            ["docker-entrypoint.sh", "gosu", "appuser"]
        );
    }
}
