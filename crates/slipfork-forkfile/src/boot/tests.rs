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
