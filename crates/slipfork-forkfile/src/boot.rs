//! `@<tool>` auto-prequel synthesis and `when changed(...)` content stamps.
//!
//! Planning is pure data: callers pass the project root, the plan walks up from
//! each `image` directory for workspace-root lockfiles and hashes the matched
//! inputs against per-node stamps. No ambient process state, no globals.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::ast::*;
use crate::eval::FeatureSet;
use crate::gate::each_enabled;

/// The direct-binary rewrite one tool documents in `specs/03`: the subcommand that precedes the
/// installed binary and the project-relative directory that holds its shims.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolExec {
    /// The subcommand that precedes the binary (`run` for `uv`, `exec` for node managers).
    pub subcommand: &'static str,
    /// Project-relative directory holding the installed shims.
    pub bin_dir: &'static str,
}

/// One auto-prequel tool, its install command, its workspace-root lockfile, and the direct-bin
/// grammar it supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrequelTool {
    /// The `@` tool name (`uv`, `pnpm`, ...).
    pub name: &'static str,
    /// The deduplicated install command.
    pub install: &'static str,
    /// The lockfile that marks a workspace root.
    pub lockfile: &'static str,
    /// The direct-bin rewrite this tool supports, if `specs/03` documents one.
    pub exec: Option<ToolExec>,
}

/// The complete `@<tool>` table, in diagnostic order.
pub const PREQUEL_TOOLS: &[PrequelTool] = &[
    PrequelTool {
        name: "uv",
        install: "uv sync",
        lockfile: "uv.lock",
        exec: Some(ToolExec {
            subcommand: "run",
            bin_dir: ".venv/bin",
        }),
    },
    PrequelTool {
        name: "poetry",
        install: "poetry install",
        lockfile: "poetry.lock",
        exec: None,
    },
    PrequelTool {
        name: "pnpm",
        install: "pnpm install",
        lockfile: "pnpm-lock.yaml",
        exec: Some(ToolExec {
            subcommand: "exec",
            bin_dir: "node_modules/.bin",
        }),
    },
    PrequelTool {
        name: "npm",
        install: "npm install",
        lockfile: "package-lock.json",
        exec: Some(ToolExec {
            subcommand: "exec",
            bin_dir: "node_modules/.bin",
        }),
    },
    PrequelTool {
        name: "yarn",
        install: "yarn install",
        lockfile: "yarn.lock",
        exec: Some(ToolExec {
            subcommand: "exec",
            bin_dir: "node_modules/.bin",
        }),
    },
    PrequelTool {
        name: "bun",
        install: "bun install",
        lockfile: "bun.lockb",
        exec: None,
    },
    PrequelTool {
        name: "composer",
        install: "composer install",
        lockfile: "composer.lock",
        exec: None,
    },
    PrequelTool {
        name: "bundle",
        install: "bundle install",
        lockfile: "Gemfile.lock",
        exec: None,
    },
    PrequelTool {
        name: "go",
        install: "go mod download",
        lockfile: "go.sum",
        exec: None,
    },
    PrequelTool {
        name: "cargo",
        install: "cargo fetch",
        lockfile: "Cargo.lock",
        exec: None,
    },
];

impl PrequelTool {
    /// Looks a tool up by its `@` name.
    #[must_use]
    pub fn lookup(name: &str) -> Option<&'static Self> {
        PREQUEL_TOOLS.iter().find(|tool| tool.name == name)
    }
}

/// One documented `@<tool> run|exec <bin> …` command split at its installed binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectBin<'a> {
    /// The owning tool row.
    pub tool: &'static PrequelTool,
    /// The direct-bin grammar `self.tool` documents.
    pub exec: ToolExec,
    /// The installed binary token, never starting with `-`.
    pub bin: &'a str,
    /// Verbatim remainder after the binary token, including its leading separator.
    pub rest: &'a str,
}

impl DirectBin<'_> {
    /// The container path of the installed binary under `workdir`.
    #[must_use]
    pub fn program(&self, workdir: &str) -> String {
        format!("{workdir}/{}/{}", self.exec.bin_dir, self.bin)
    }

    /// The container directory that holds the installed shims.
    #[must_use]
    pub fn bin_dir(&self, workdir: &str) -> String {
        format!("{workdir}/{}", self.exec.bin_dir)
    }
}

/// Splits a documented `@<tool> run|exec <bin> …` command into its installed binary plus the
/// verbatim remainder.
///
/// This is the **only** tokenizer of the direct-bin grammar: callers never re-scan argv for
/// `run`/`exec`, and tools without a documented `exec` row (`bun`, `poetry`, `cargo`, …) never
/// match.
#[must_use]
pub fn direct_bin(command: &str) -> Option<DirectBin<'_>> {
    let (name, rest) = crate::expr::tool_prefix(command)?;
    let tool = PrequelTool::lookup(name)?;
    let exec = tool.exec?;
    let (sub_start, sub_end) = nth_token(rest, 0)?;
    if &rest[sub_start..sub_end] != exec.subcommand {
        return None;
    }
    let (bin_start, bin_end) = nth_token(rest, 1)?;
    let bin = &rest[bin_start..bin_end];
    if bin.starts_with('-') {
        return None;
    }
    Some(DirectBin {
        tool,
        exec,
        bin,
        rest: &rest[bin_end..],
    })
}

/// Byte range of the `n`-th whitespace-separated token, used to splice a prefix without
/// rewriting the remainder.
fn nth_token(text: &str, n: usize) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut index = 0;
    let mut count = 0;
    while index < bytes.len() {
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= bytes.len() {
            return None;
        }
        let start = index;
        while index < bytes.len() && !bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if count == n {
            return Some((start, index));
        }
        count += 1;
    }
    None
}

/// One deduplicated setup step in the boot DAG.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupNode {
    /// Owning image id.
    pub image: String,
    /// Workspace directory, relative to the project root.
    pub directory: PathBuf,
    /// The install/prep command line.
    pub command: String,
    /// The `@<tool>` that produced this node, when it is an auto-prequel.
    pub tool: Option<String>,
    /// Input files (root-relative) whose contents feed the stamp.
    pub patterns: Vec<PathBuf>,
    /// Content hash of `patterns` when the plan was built.
    pub stamp: String,
    /// Whether the command interpolates `$env.<key>` at runtime.
    pub uses_env: bool,
}

impl SetupNode {
    /// Whether every input exists and still hashes to this node's stamp.
    ///
    /// Nodes with no patterns are always due, and a missing or unreadable input
    /// can never be current.
    #[must_use]
    pub fn is_current(&self, root: &Path) -> bool {
        if self.patterns.is_empty() {
            return false;
        }
        let inputs = read_inputs(root, &self.patterns);
        if inputs.iter().any(|(_, bytes)| bytes.is_none()) {
            return false;
        }
        self.stamp == stamp_of(&inputs)
    }
}

/// Every deduplicated setup step for one document, in declaration order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BootPlan {
    /// Setup nodes in boot order.
    pub nodes: Vec<SetupNode>,
}

impl BootPlan {
    /// The number of setup nodes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the document needs no setup step at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

/// Synthesizes the deduplicated `@<tool>` / `when changed(...)` setup nodes under `features`.
#[must_use]
pub fn boot_plan_with(doc: &Forkfile, root: &Path, features: &FeatureSet) -> BootPlan {
    let mut planner = Planner {
        root,
        features,
        seen: BTreeSet::new(),
        nodes: Vec::new(),
    };
    each_enabled(&doc.items.items, features, &mut |item| {
        if let TopItem::Image(image) = item {
            planner.image(image);
        }
    });
    BootPlan {
        nodes: planner.nodes,
    }
}

/// Synthesizes the deduplicated `@<tool>` / `when changed(...)` setup nodes under default features.
#[must_use]
pub fn boot_plan(doc: &Forkfile, root: &Path) -> BootPlan {
    boot_plan_with(doc, root, &doc.default_features())
}

/// Hashes input paths and their contents with a stable FNV-1a 64 mix.
///
/// Pure: the same inputs always produce the same stamp, and any content change
/// produces a different one.
#[must_use]
pub fn stamp_of(inputs: &[(PathBuf, Option<Vec<u8>>)]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for (path, bytes) in inputs {
        hash = mix(hash, path.to_string_lossy().as_bytes());
        match bytes {
            Some(bytes) => hash = mix(hash, bytes),
            None => hash = mix(hash, b"<missing>"),
        }
    }
    format!("{hash:016x}")
}

fn mix(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in (bytes.len() as u64).to_le_bytes().iter().chain(bytes) {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn read_inputs(root: &Path, patterns: &[PathBuf]) -> Vec<(PathBuf, Option<Vec<u8>>)> {
    patterns
        .iter()
        .map(|pattern| (pattern.clone(), fs::read(root.join(pattern)).ok()))
        .collect()
}

struct Planner<'a> {
    root: &'a Path,
    features: &'a FeatureSet,
    nodes: Vec<SetupNode>,
    seen: BTreeSet<(String, PathBuf, String, Vec<PathBuf>)>,
}

impl Planner<'_> {
    fn image(&mut self, image: &Image) {
        let directory = match &image.source {
            ImageSource::Dir(dir) => clean(Path::new(dir)),
            ImageSource::From(_) => PathBuf::from("."),
        };
        self.image_items(&image.id, &directory, &image.body.items);
    }

    fn image_items(&mut self, image: &str, directory: &Path, items: &[ImageItem]) {
        let features = self.features;
        each_enabled(items, features, &mut |item| {
            self.image_item(image, directory, item);
        });
    }

    fn image_item(&mut self, image: &str, directory: &Path, item: &ImageItem) {
        match item {
            ImageItem::BeforeStart(hook)
            | ImageItem::PreDeploy(hook)
            | ImageItem::PostDeploy(hook)
            | ImageItem::FailedDeploy(hook) => self.hook(image, directory, hook, true),
            ImageItem::Service(service) => {
                self.service_items(image, directory, &service.body.items);
            }
            _ => {}
        }
    }

    fn service_items(&mut self, image: &str, directory: &Path, items: &[ServiceItem]) {
        let features = self.features;
        each_enabled(items, features, &mut |item| {
            self.service_item(image, directory, item);
        });
    }

    fn service_item(&mut self, image: &str, directory: &Path, item: &ServiceItem) {
        match item {
            ServiceItem::BeforeStart(hook)
            | ServiceItem::PreDeploy(hook)
            | ServiceItem::PostDeploy(hook)
            | ServiceItem::FailedDeploy(hook) => self.hook(image, directory, hook, true),
            ServiceItem::Run(hook) => self.hook(image, directory, hook, false),
            _ => {}
        }
    }

    fn hook(&mut self, image: &str, image_dir: &Path, hook: &Hook, always_due: bool) {
        let commands = hook.commands(self.features);
        let changed = hook.when_changed.as_deref().unwrap_or(&[]);
        for command in commands {
            let tool = command
                .tool
                .as_ref()
                .and_then(|tool| PrequelTool::lookup(&tool.name));
            if tool.is_none() && changed.is_empty() && !always_due {
                continue;
            }
            let mut patterns = Vec::new();
            let directory = match tool {
                Some(tool) => {
                    let (directory, lockfile) = workspace_dir(self.root, image_dir, tool.lockfile);
                    patterns.push(lockfile);
                    directory
                }
                None => image_dir.to_path_buf(),
            };
            for pattern in changed {
                patterns.push(clean(&image_dir.join(pattern)));
            }
            patterns.dedup();
            let uses_env = tool.is_none() && command.uses_env();
            let command = tool.map_or_else(|| command.text(), |tool| tool.install.to_string());
            let stamp = stamp_of(&read_inputs(self.root, &patterns));
            let key = (
                image.to_string(),
                directory.clone(),
                command.clone(),
                patterns.clone(),
            );
            if !self.seen.insert(key) {
                continue;
            }
            self.nodes.push(SetupNode {
                image: image.to_string(),
                directory,
                command,
                tool: tool.map(|tool| tool.name.to_string()),
                patterns,
                stamp,
                uses_env,
            });
        }
    }
}

/// Walks up from `image_dir` for `lockfile`, returning the workspace directory
/// (root-relative) and the root-relative lockfile path.
fn workspace_dir(root: &Path, image_dir: &Path, lockfile: &str) -> (PathBuf, PathBuf) {
    let start = root.join(image_dir);
    let mut cursor = start.as_path();
    loop {
        if cursor.join(lockfile).is_file() {
            return (
                relative(root, cursor),
                relative(root, &cursor.join(lockfile)),
            );
        }
        if cursor == root {
            break;
        }
        match cursor.parent() {
            Some(parent) => cursor = parent,
            None => break,
        }
    }
    (image_dir.to_path_buf(), clean(&image_dir.join(lockfile)))
}

fn relative(root: &Path, path: &Path) -> PathBuf {
    match path.strip_prefix(root) {
        Ok(rest) if rest.as_os_str().is_empty() => PathBuf::from("."),
        Ok(rest) => rest.to_path_buf(),
        Err(_) => path.to_path_buf(),
    }
}

fn clean(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_matches_the_declared_at_tools_and_installs() {
        let names: Vec<&str> = PREQUEL_TOOLS.iter().map(|tool| tool.name).collect();
        assert_eq!(
            names,
            [
                "uv", "poetry", "pnpm", "npm", "yarn", "bun", "composer", "bundle", "go", "cargo"
            ]
        );
        assert_eq!(
            PrequelTool::lookup("uv").map(|tool| tool.install),
            Some("uv sync")
        );
        assert_eq!(
            PrequelTool::lookup("go").map(|tool| tool.install),
            Some("go mod download")
        );
        assert_eq!(
            PrequelTool::lookup("cargo").map(|tool| tool.lockfile),
            Some("Cargo.lock")
        );
        assert_eq!(
            PrequelTool::lookup("uv").and_then(|tool| tool.exec),
            Some(ToolExec {
                subcommand: "run",
                bin_dir: ".venv/bin",
            })
        );
        assert_eq!(
            PrequelTool::lookup("yarn").and_then(|tool| tool.exec),
            Some(ToolExec {
                subcommand: "exec",
                bin_dir: "node_modules/.bin",
            })
        );
        assert!(PrequelTool::lookup("bun").is_some_and(|tool| tool.exec.is_none()));
        assert!(PrequelTool::lookup("python").is_none());
    }

    #[test]
    fn direct_bin_is_the_only_tokenizer_for_the_documented_grammar() {
        let uv = direct_bin("@uv run uvicorn app.main:app --reload").expect("uv run resolves");
        assert_eq!(uv.tool.name, "uv");
        assert_eq!(uv.bin, "uvicorn");
        assert_eq!(uv.rest, " app.main:app --reload");
        assert_eq!(uv.program("/project"), "/project/.venv/bin/uvicorn");
        assert_eq!(uv.bin_dir("/project/front"), "/project/front/.venv/bin");

        let pnpm = direct_bin("@pnpm exec vite dev").expect("pnpm exec resolves");
        assert_eq!(pnpm.bin, "vite");
        assert_eq!(pnpm.rest, " dev");
        assert_eq!(
            pnpm.program("/project/front"),
            "/project/front/node_modules/.bin/vite"
        );

        assert_eq!(
            direct_bin("@npm exec esbuild --bundle"),
            Some(DirectBin {
                tool: PrequelTool::lookup("npm").expect("npm row"),
                exec: ToolExec {
                    subcommand: "exec",
                    bin_dir: "node_modules/.bin"
                },
                bin: "esbuild",
                rest: " --bundle",
            })
        );

        assert!(
            direct_bin("@bun exec vite").is_none(),
            "bun has no documented exec"
        );
        assert!(direct_bin("@poetry run pytest").is_none());
        assert!(
            direct_bin("@uv run -m uvicorn").is_none(),
            "flag-first never resolves"
        );
        assert!(direct_bin("@uv sync").is_none());
        assert!(direct_bin("uv run uvicorn").is_none());
        assert!(direct_bin("@uv").is_none());
    }

    #[test]
    fn stamps_are_pure_and_track_every_byte() {
        let lock = PathBuf::from("uv.lock");
        let first = stamp_of(&[(lock.clone(), Some(b"version = 1\n".to_vec()))]);
        let again = stamp_of(&[(lock.clone(), Some(b"version = 1\n".to_vec()))]);
        assert_eq!(first, again);
        let changed = stamp_of(&[(lock.clone(), Some(b"version = 2\n".to_vec()))]);
        assert_ne!(first, changed);
        let other = stamp_of(&[(PathBuf::from("other.lock"), Some(b"version = 1\n".to_vec()))]);
        assert_ne!(first, other);
        assert_ne!(
            stamp_of(&[(lock, None)]),
            stamp_of(&[(PathBuf::from("uv.lock"), Some(Vec::new()))])
        );
    }
}
