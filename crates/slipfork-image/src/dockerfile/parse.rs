//! Per-instruction parsers: flags, quoting, heredocs, and typed instruction construction.

use super::instruction::{
    CacheMount, Command, CopySpec, Instruction, Port, Protocol, RunSpec, StageBase,
};
use super::{permissive_split, shell_split, split_flags};
use crate::error::{Error, Result};

pub(super) fn parse_instruction(
    text: &str,
    line: usize,
    heredocs: &[(String, bool)],
    bodies: &[Vec<String>],
    aliases: &[String],
) -> Result<Instruction> {
    let text = text.trim();
    let (word, rest) = match text.split_once(char::is_whitespace) {
        Some((word, rest)) => (word, rest.trim_start()),
        None => (text, ""),
    };
    match word.to_ascii_uppercase().as_str() {
        "FROM" => parse_from(rest, line, aliases),
        "ARG" => parse_arg(rest, line),
        "ENV" => parse_env(rest, line),
        "WORKDIR" => parse_workdir(rest, line),
        "COPY" => parse_copy("COPY", rest, line),
        "ADD" => parse_copy("ADD", rest, line),
        "RUN" => parse_run(rest, line, heredocs, bodies),
        "USER" => parse_user(rest, line),
        "EXPOSE" => parse_expose(rest, line),
        "CMD" => parse_command("CMD", rest, line),
        "ENTRYPOINT" => parse_command("ENTRYPOINT", rest, line),
        _ => Err(Error::Dockerfile {
            line,
            message: format!("unknown instruction `{word}`"),
            next: "use FROM, ARG, ENV, WORKDIR, COPY, ADD, RUN, USER, EXPOSE, CMD, or ENTRYPOINT",
        }),
    }
}

fn parse_from(rest: &str, line: usize, aliases: &[String]) -> Result<Instruction> {
    let (_, rest) = split_flags(rest);
    let words = shell_split(rest, line)?;
    if words.is_empty() {
        return Err(missing(line, "FROM"));
    }
    if words.len() > 3 || (words.len() == 3 && !words[1].eq_ignore_ascii_case("as")) {
        return Err(Error::Dockerfile {
            line,
            message: "malformed `FROM`".to_owned(),
            next: "write `FROM <image>` or `FROM <image> AS <name>`",
        });
    }
    let base_word = &words[0];
    let alias = words.get(2).map(|alias| alias.to_ascii_lowercase());
    let base = if aliases
        .iter()
        .any(|known| known == &base_word.to_ascii_lowercase())
    {
        StageBase::Stage(base_word.to_ascii_lowercase())
    } else {
        StageBase::Image(base_word.clone())
    };
    Ok(Instruction::From { base, alias })
}

fn parse_arg(rest: &str, line: usize) -> Result<Instruction> {
    let words = shell_split(rest, line)?;
    if words.len() != 1 {
        return Err(missing(line, "ARG"));
    }
    let (name, default) = match words[0].split_once('=') {
        Some((name, default)) => (name.to_owned(), Some(default.to_owned())),
        None => (words[0].clone(), None),
    };
    Ok(Instruction::Arg { name, default })
}

fn parse_env(rest: &str, line: usize) -> Result<Instruction> {
    let words = shell_split(rest, line)?;
    if words.is_empty() {
        return Err(missing(line, "ENV"));
    }
    if words.iter().all(|word| word.contains('=')) {
        let pairs = words
            .into_iter()
            .map(|word| match word.split_once('=') {
                Some((key, value)) => (key.to_owned(), value.to_owned()),
                None => unreachable!("guarded by `contains('=')`"),
            })
            .collect();
        return Ok(Instruction::Env(pairs));
    }
    let (key, value) = match words.split_first() {
        Some((key, rest)) if !rest.is_empty() => (key.clone(), rest.join(" ")),
        _ => return Err(missing(line, "ENV")),
    };
    Ok(Instruction::Env(vec![(key, value)]))
}

fn parse_workdir(rest: &str, line: usize) -> Result<Instruction> {
    let words = shell_split(rest, line)?;
    if words.len() != 1 || words[0].is_empty() {
        return Err(missing(line, "WORKDIR"));
    }
    Ok(Instruction::Workdir(words[0].clone()))
}

fn parse_copy(op: &str, rest: &str, line: usize) -> Result<Instruction> {
    let (flags, rest) = split_flags(rest);
    let mut spec = CopySpec {
        from: None,
        chown: None,
        mode: None,
        sources: Vec::new(),
        dest: String::new(),
    };
    for flag in &flags {
        let (name, value) = match flag.split_once('=') {
            Some((name, value)) => (name, value),
            None => (flag.as_str(), ""),
        };
        match name {
            // Image-reference tags and digests are case-sensitive; aliases match case-insensitively
            // at classification time instead.
            "--from" => spec.from = Some(value.to_owned()),
            "--chown" => spec.chown = Some(value.to_owned()),
            "--chmod" => spec.mode = Some(parse_mode(value, line)?),
            "--link" | "--exclude" | "--parents" | "--checksum" => {
                return Err(Error::Dockerfile {
                    line,
                    message: format!("`{name}` is not supported by the native builder"),
                    next: "drop the flag; use `--from`, `--chown`, or `--chmod` instead",
                });
            }
            _ => {
                return Err(Error::Dockerfile {
                    line,
                    message: format!("unknown `{op}` flag `{name}`"),
                    next: "use `--from=<stage>`, `--chown=<user>`, or `--chmod=<mode>`",
                });
            }
        }
    }
    let mut words = if rest.starts_with('[') {
        serde_json::from_str::<Vec<String>>(rest).map_err(|_| Error::Dockerfile {
            line,
            message: format!("`{op}` JSON form is not a string array"),
            next: "write `[\"src\", \"dest\"]`",
        })?
    } else {
        shell_split(rest, line)?
    };
    if words.len() < 2 {
        return Err(missing(line, op));
    }
    spec.dest = words.pop().unwrap_or_default();
    spec.sources = words;
    Ok(if op == "COPY" {
        Instruction::Copy(spec)
    } else {
        Instruction::Add(spec)
    })
}

fn parse_mode(value: &str, line: usize) -> Result<u32> {
    let digits = value.strip_prefix("0o").unwrap_or(value);
    let mode = u32::from_str_radix(digits, 8)
        .ok()
        .filter(|mode| *mode <= 0o7777 && !digits.is_empty());
    mode.ok_or_else(|| Error::Dockerfile {
        line,
        message: format!("`--chmod={value}` is not an octal mode"),
        next: "write `--chmod=755` (up to `7777`)",
    })
}

fn parse_run(
    rest: &str,
    line: usize,
    heredocs: &[(String, bool)],
    bodies: &[Vec<String>],
) -> Result<Instruction> {
    let (flags, rest) = split_flags(rest);
    let mut mounts = Vec::new();
    for flag in &flags {
        let value = flag
            .strip_prefix("--mount=")
            .ok_or_else(|| Error::Dockerfile {
                line,
                message: format!("unsupported `RUN` flag `{flag}`"),
                next: "only `--mount=type=cache,target=<path>` is supported",
            })?;
        let option = permissive_split(value);
        let kind = option
            .iter()
            .find_map(|part| part.strip_prefix("type="))
            .unwrap_or("cache");
        if kind != "cache" {
            return Err(Error::Dockerfile {
                line,
                message: format!("`RUN --mount=type={kind}` is not supported"),
                next: "use `--mount=type=cache,target=<path>`",
            });
        }
        let target = option
            .iter()
            .find_map(|part| part.strip_prefix("target="))
            .ok_or_else(|| Error::Dockerfile {
                line,
                message: "`RUN --mount=type=cache` has no `target=`".to_owned(),
                next: "write `--mount=type=cache,target=/cache`",
            })?;
        mounts.push(CacheMount {
            id: option
                .iter()
                .find_map(|part| part.strip_prefix("id="))
                .map(str::to_owned),
            target: target.to_owned(),
            readonly: option
                .iter()
                .any(|part| *part == "readonly" || *part == "ro"),
        });
    }
    let heredoc_only =
        !heredocs.is_empty() && super::strip_heredoc_operators(rest).trim().is_empty();
    let mut command_text = rest.to_owned();
    if !heredocs.is_empty() {
        let mut joined = String::new();
        for (body, (delimiter, _)) in bodies.iter().zip(heredocs) {
            joined.push_str(&body.join("\n"));
            joined.push('\n');
            if heredoc_only {
                continue;
            }
            command_text.push('\n');
            command_text.push_str(&body.join("\n"));
            command_text.push('\n');
            command_text.push_str(delimiter);
        }
        if heredoc_only {
            command_text = joined;
        }
    }
    let command = if command_text.trim_start().starts_with('[') {
        Command::Exec(
            serde_json::from_str::<Vec<String>>(command_text.trim()).map_err(|_| {
                Error::Dockerfile {
                    line,
                    message: "`RUN` JSON form is not a string array".to_owned(),
                    next: "write `RUN [\"program\", \"arg\"]`",
                }
            })?,
        )
    } else {
        if command_text.trim().is_empty() {
            return Err(missing(line, "RUN"));
        }
        Command::Shell(command_text.trim().to_owned())
    };
    Ok(Instruction::Run(RunSpec { mounts, command }))
}

fn parse_user(rest: &str, line: usize) -> Result<Instruction> {
    let words = shell_split(rest, line)?;
    if words.len() != 1 || words[0].is_empty() {
        return Err(missing(line, "USER"));
    }
    Ok(Instruction::User(words[0].clone()))
}

fn parse_expose(rest: &str, line: usize) -> Result<Instruction> {
    let words = shell_split(rest, line)?;
    if words.is_empty() {
        return Err(missing(line, "EXPOSE"));
    }
    let mut ports = Vec::new();
    for word in words {
        let (number, protocol) = match word.split_once('/') {
            Some((number, protocol)) => (number, protocol.to_ascii_lowercase()),
            None => (word.as_str(), "tcp".to_owned()),
        };
        let protocol = match protocol.as_str() {
            "tcp" => Protocol::Tcp,
            "udp" => Protocol::Udp,
            other => {
                return Err(Error::Dockerfile {
                    line,
                    message: format!("unknown `EXPOSE` protocol `{other}`"),
                    next: "use `/tcp` or `/udp`",
                });
            }
        };
        let number = number.parse::<u16>().map_err(|_| Error::Dockerfile {
            line,
            message: format!("`EXPOSE {word}` is not a valid port"),
            next: "use a port between 1 and 65535",
        })?;
        ports.push(Port { number, protocol });
    }
    Ok(Instruction::Expose(ports))
}

fn parse_command(op: &str, rest: &str, line: usize) -> Result<Instruction> {
    let rest = rest.trim();
    if rest.is_empty() {
        return Err(missing(line, op));
    }
    let command = if rest.starts_with('[') {
        Command::Exec(
            serde_json::from_str::<Vec<String>>(rest).map_err(|_| Error::Dockerfile {
                line,
                message: format!("`{op}` JSON form is not a string array"),
                next: "write `[\"program\", \"arg\"]`",
            })?,
        )
    } else {
        Command::Shell(rest.to_owned())
    };
    Ok(if op == "CMD" {
        Instruction::Cmd(command)
    } else {
        Instruction::Entrypoint(command)
    })
}

fn missing(line: usize, op: &str) -> Error {
    Error::Dockerfile {
        line,
        message: format!("`{op}` needs an argument"),
        next: match op {
            "FROM" => "write `FROM <image> [AS <name>]`",
            "RUN" => "write `RUN <command>`",
            "ARG" => "write `ARG <NAME>[=<default>]`",
            "WORKDIR" => "write `WORKDIR <absolute path>`",
            "ENV" => "write `ENV KEY=VALUE`",
            "EXPOSE" => "write `EXPOSE <port>[/tcp|udp]`",
            "USER" => "write `USER <uid>[:<gid>]`",
            _ => "add the missing argument",
        },
    }
}
