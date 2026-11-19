//! The inline `Forkfile:<line>` + trailing-log block a failed hook prints.
//!
//! A hook failure is the one place the developer needs the *manifest line* that declared the hook
//! and the *last lines* the hook actually wrote before exiting. The spans come from the canonical
//! [`slipfork_forkfile`] parser (this module never re-scans the manifest text) and the tail is read
//! back from the daemon's retained log file, so the CLI prints both without a second round trip.
//!
//! [`HookFailure::render`] is the single renderer: one `error:` line, the last
//! [`TAIL_LINES`] lines prefixed `│ `, then one `next:` remedy. `--json` never calls it.

use std::fmt::Write as _;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use slipfork_forkfile::{ServiceDirective, Stmt, parse_forkfile};
use slipfork_wire::control::BootEvent;

use crate::ui::TerminalUi;

/// How many trailing hook-log lines the failure block prints inline.
pub const TAIL_LINES: usize = 15;

/// Bytes read from the end of a retained log: enough for the tail without slurping a huge file.
const TAIL_WINDOW: u64 = 16 * 1024;

/// One failed `<service>:<phase>` hook, as the daemon reported it.
#[derive(Debug, Clone)]
pub struct HookFailure {
    id: String,
    argv: Vec<String>,
    exit_code: i32,
    world: String,
    log_path: PathBuf,
}

impl HookFailure {
    /// Keep the last streamed `HookFailed` event (the only failure the rescue block renders).
    pub fn from_event(event: &BootEvent) -> Option<Self> {
        let BootEvent::HookFailed {
            id,
            argv,
            exit_code,
            world,
            log_path,
        } = event
        else {
            return None;
        };
        Some(Self {
            id: id.clone(),
            argv: argv.clone(),
            exit_code: *exit_code,
            world: world.clone(),
            log_path: log_path.clone(),
        })
    }

    /// The `<service>` half of `<service>:<phase>` (the `slipfork logs <service>` target).
    fn service(&self) -> &str {
        self.id
            .split_once(':')
            .map_or(self.id.as_str(), |(svc, _)| svc)
    }

    /// The manifest line that declared this hook, resolved from the canonical parser's spans.
    pub fn site_line(&self, worktree: &Path) -> Option<u32> {
        hook_site_line(worktree, &self.id)
    }

    /// The last [`TAIL_LINES`] lines the daemon retained for this hook.
    pub fn tail(&self) -> Vec<String> {
        tail_lines(&self.log_path, TAIL_LINES)
    }

    /// Render the human rescue block for one failed boot.
    pub fn render(&self, worktree: Option<&Path>, ui: &TerminalUi) -> String {
        let site = worktree
            .and_then(|worktree| self.site_line(worktree))
            .map(|line| format!(" (Forkfile:{line})"))
            .unwrap_or_default();
        let mut text = String::new();
        writeln!(
            text,
            "{} hook '{}' exited with code {} in world '{}'{}",
            ui.failed("error:"),
            ui.bold(&self.id),
            self.exit_code,
            self.world,
            site
        )
        .ok();
        if ui.verbose() {
            writeln!(text, "  argv: {:?}", self.argv).ok();
        }
        let tail = self.tail();
        for line in &tail {
            writeln!(text, "  {} {line}", ui.secondary("│")).ok();
        }
        if tail.is_empty() {
            writeln!(text, "  {} no retained output", ui.secondary("│")).ok();
        }
        writeln!(
            text,
            "  next: slipfork logs {} --world {} | slipfork shell {} --world {}",
            self.service(),
            self.world,
            self.service(),
            self.world
        )
        .ok();
        writeln!(
            text,
            "  log:  {}",
            ui.secondary(&self.log_path.display().to_string())
        )
        .ok();
        text
    }
}

/// Resolve `<service>:<phase>` to its declaration line through the canonical `Forkfile` parser.
///
/// The parser owns every token and span; this function only walks the already-built statement list
/// (including directives nested in `on <expr> { … }` blocks) looking for the one `Hook` directive
/// whose phase matches. An unreadable or unparsable manifest yields `None` — the failure block then
/// prints without a line, never with a guessed one.
pub fn hook_site_line(worktree: &Path, hook_id: &str) -> Option<u32> {
    let (service_id, phase) = hook_id.split_once(':')?;
    let source = std::fs::read_to_string(worktree.join("Forkfile")).ok()?;
    let manifest = parse_forkfile(&source).ok()?;
    manifest.statements.iter().find_map(|statement| {
        let Stmt::Service(service) = statement else {
            return None;
        };
        if service.id.value != service_id {
            return None;
        }
        directive_line(&service.directives, phase)
    })
}

/// The first `Hook` directive with `phase` in one directive list (recursing into `on` blocks).
fn directive_line(directives: &[ServiceDirective], phase: &str) -> Option<u32> {
    directives.iter().find_map(|directive| match directive {
        ServiceDirective::Hook {
            phase: declared,
            span,
            ..
        } if declared.as_str() == phase => Some(span.line),
        ServiceDirective::On { body, .. } => directive_line(body, phase),
        _ => None,
    })
}

/// The last `count` lines of `path` (a missing or unreadable log yields no tail, never an error).
fn tail_lines(path: &Path, count: usize) -> Vec<String> {
    let Ok(mut file) = File::open(path) else {
        return Vec::new();
    };
    let end = file.seek(SeekFrom::End(0)).unwrap_or(0);
    let start = end.saturating_sub(TAIL_WINDOW);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<&str> = text.lines().collect();
    // A mid-file window starts inside a line; that partial fragment is not a captured line.
    if start > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    let from = lines.len().saturating_sub(count);
    lines[from..]
        .iter()
        .map(|line| (*line).to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::Capabilities;

    fn plain() -> TerminalUi {
        TerminalUi::new(Capabilities {
            tty: false,
            color: false,
            verbose: false,
            json: false,
        })
    }

    const FORKFILE: &str = "\
project demo

service api ./ {
    port 8000
    setup: sh -c 'make db'
    pre-deploy: sh -c 'npm run build'
}
";

    fn failure(log_path: &Path) -> HookFailure {
        HookFailure::from_event(&BootEvent::HookFailed {
            id: "api:pre-deploy".to_owned(),
            argv: vec!["sh".to_owned(), "-c".to_owned(), "npm run build".to_owned()],
            exit_code: 2,
            world: "demo".to_owned(),
            log_path: log_path.to_path_buf(),
        })
        .expect("a HookFailed event")
    }

    #[test]
    fn the_manifest_line_comes_from_the_canonical_parser_including_on_blocks() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("Forkfile"), FORKFILE).expect("write Forkfile");
        assert_eq!(hook_site_line(tmp.path(), "api:setup"), Some(5));
        assert_eq!(hook_site_line(tmp.path(), "api:pre-deploy"), Some(6));
        assert_eq!(hook_site_line(tmp.path(), "api:post-deploy"), None);
        assert_eq!(hook_site_line(tmp.path(), "front:setup"), None);
    }

    #[test]
    fn a_missing_or_unparsable_manifest_yields_no_line() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert_eq!(hook_site_line(tmp.path(), "api:setup"), None);
        std::fs::write(tmp.path().join("Forkfile"), "service { broken")
            .expect("write broken Forkfile");
        assert_eq!(hook_site_line(tmp.path(), "api:setup"), None);
        assert_eq!(hook_site_line(tmp.path(), "no-separator"), None);
    }

    #[test]
    fn the_block_prints_only_the_last_fifteen_lines_and_cites_the_line() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("Forkfile"), FORKFILE).expect("write Forkfile");
        let log = tmp.path().join("api-pre-deploy.log");
        let body: String = (1..=20).map(|n| format!("line {n}\n")).collect();
        std::fs::write(&log, body).expect("write log");

        let failed = failure(&log);
        assert_eq!(failed.tail().len(), TAIL_LINES);
        assert_eq!(failed.tail().first().map(String::as_str), Some("line 6"));
        assert_eq!(failed.tail().last().map(String::as_str), Some("line 20"));

        let text = failed.render(Some(tmp.path()), &plain());
        assert!(text.starts_with("error: hook 'api:pre-deploy'"), "{text}");
        assert!(text.contains("(Forkfile:6)"), "{text}");
        assert!(text.contains("│ line 20\n"), "{text}");
        assert!(
            !text.contains("│ line 5\n"),
            "no more than 15 lines: {text}"
        );
        assert!(
            text.contains("next: slipfork logs api --world demo | slipfork shell api --world demo"),
            "{text}"
        );
        assert!(
            !text.contains('\x1b'),
            "a piped block carries no escape bytes"
        );
    }

    #[test]
    fn a_missing_log_renders_a_crisp_placeholder_instead_of_an_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let failed = failure(&tmp.path().join("gone.log"));
        let text = failed.render(None, &plain());
        assert!(text.contains("│ no retained output"), "{text}");
        assert!(!text.contains("Forkfile:"), "no guessed line: {text}");
    }
}
