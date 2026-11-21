//! `@<tool>` auto-prequel synthesis and `when changed(...)` content stamps.
//!
//! Planning is pure data: callers pass the project root, the plan walks up from
//! each `image` directory for workspace-root lockfiles and hashes the matched
//! inputs against per-node stamps. No ambient process state, no globals.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::ast::*;
use crate::expr::Command;

/// One auto-prequel tool, its install command, and its workspace-root lockfile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrequelTool {
    /// The `@` tool name (`uv`, `pnpm`, ...).
    pub name: &'static str,
    /// The deduplicated install command.
    pub install: &'static str,
    /// The lockfile that marks a workspace root.
    pub lockfile: &'static str,
}

/// The complete `@<tool>` table, in diagnostic order.
pub const PREQUEL_TOOLS: &[PrequelTool] = &[
    PrequelTool {
        name: "uv",
        install: "uv sync",
        lockfile: "uv.lock",
    },
    PrequelTool {
        name: "poetry",
        install: "poetry install",
        lockfile: "poetry.lock",
    },
    PrequelTool {
        name: "pnpm",
        install: "pnpm install",
        lockfile: "pnpm-lock.yaml",
    },
    PrequelTool {
        name: "npm",
        install: "npm install",
        lockfile: "package-lock.json",
    },
    PrequelTool {
        name: "yarn",
        install: "yarn install",
        lockfile: "yarn.lock",
    },
    PrequelTool {
        name: "bun",
        install: "bun install",
        lockfile: "bun.lockb",
    },
    PrequelTool {
        name: "composer",
        install: "composer install",
        lockfile: "composer.lock",
    },
    PrequelTool {
        name: "bundle",
        install: "bundle install",
        lockfile: "Gemfile.lock",
    },
    PrequelTool {
        name: "go",
        install: "go mod download",
        lockfile: "go.sum",
    },
    PrequelTool {
        name: "cargo",
        install: "cargo fetch",
        lockfile: "Cargo.lock",
    },
];

impl PrequelTool {
    /// Looks a tool up by its `@` name.
    #[must_use]
    pub fn lookup(name: &str) -> Option<&'static Self> {
        PREQUEL_TOOLS.iter().find(|tool| tool.name == name)
    }
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

/// Synthesizes the deduplicated `@<tool>` / `when changed(...)` setup nodes.
#[must_use]
pub fn boot_plan(doc: &Forkfile, root: &Path) -> BootPlan {
    let mut planner = Planner {
        root,
        seen: BTreeSet::new(),
        nodes: Vec::new(),
    };
    for item in &doc.items.items {
        planner.top(item);
    }
    BootPlan {
        nodes: planner.nodes,
    }
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
    nodes: Vec<SetupNode>,
    seen: BTreeSet<(String, PathBuf, String, Vec<PathBuf>)>,
}

impl Planner<'_> {
    fn top(&mut self, item: &TopItem) {
        match item {
            TopItem::Image(image) => self.image(image),
            TopItem::On(on) => match &on.body {
                OnBody::Inline(inner) => self.top(inner),
                OnBody::Block(block) => {
                    for item in &block.items {
                        self.top(item);
                    }
                }
            },
            _ => {}
        }
    }

    fn image(&mut self, image: &Image) {
        let directory = match &image.source {
            ImageSource::Dir(dir) => clean(Path::new(dir)),
            ImageSource::From(_) => PathBuf::from("."),
        };
        self.image_items(&image.id, &directory, &image.body.items);
    }

    fn image_items(&mut self, image: &str, directory: &Path, items: &[ImageItem]) {
        for item in items {
            match item {
                ImageItem::BeforeStart(hook)
                | ImageItem::PreDeploy(hook)
                | ImageItem::PostDeploy(hook)
                | ImageItem::FailedDeploy(hook) => self.hook(image, directory, hook, true),
                ImageItem::Service(service) => {
                    self.service_items(image, directory, &service.body.items);
                }
                ImageItem::On(on) => match &on.body {
                    OnBody::Inline(inner) => {
                        self.image_items(image, directory, std::slice::from_ref(inner.as_ref()));
                    }
                    OnBody::Block(block) => self.image_items(image, directory, &block.items),
                },
                _ => {}
            }
        }
    }

    fn service_items(&mut self, image: &str, directory: &Path, items: &[ServiceItem]) {
        for item in items {
            match item {
                ServiceItem::BeforeStart(hook)
                | ServiceItem::PreDeploy(hook)
                | ServiceItem::PostDeploy(hook)
                | ServiceItem::FailedDeploy(hook) => self.hook(image, directory, hook, true),
                ServiceItem::Run(hook) => self.hook(image, directory, hook, false),
                ServiceItem::On(on) => match &on.body {
                    OnBody::Inline(inner) => {
                        self.service_items(image, directory, std::slice::from_ref(inner.as_ref()));
                    }
                    OnBody::Block(block) => self.service_items(image, directory, &block.items),
                },
                _ => {}
            }
        }
    }

    fn hook(&mut self, image: &str, image_dir: &Path, hook: &Hook, always_due: bool) {
        let commands: Vec<&Command> = match &hook.body {
            HookBody::Inline(command) => vec![command],
            HookBody::Block(block) => block.items.iter().collect(),
        };
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
        assert!(PrequelTool::lookup("python").is_none());
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
