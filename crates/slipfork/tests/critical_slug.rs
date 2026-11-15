//! Protected end-to-end CLI critical tests for canonical VCS project identity (`H-005`),
//! fail-closed Git/Jujutsu topologies (owner ruling), and `--snow-file` / `dag plan` diagnostics.

use std::ffi::OsStr;
use std::fs::{self, File, Permissions};
use std::io::Write;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

const MINIMAL_SNOW_YML: &str = r#"
modes:
  local:
    envs:
      app:
        APP_URL: '${{ resources::app.url }}'
        SECRET_KEY: '${{ secrets::SECRET_KEY !! random_string(32) }}'
"#;

/// Allocate a disk-backed temporary directory under `$HOME/.cache/slipfork-critical-tests`
/// (never `/tmp` `tmpfs`), cleaned up automatically when `TempDir` drops.
fn disk_tempdir() -> TempDir {
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".cache")
        .join("slipfork-critical-tests");
    fs::create_dir_all(&base).expect("create disk test cache base");
    tempfile::Builder::new()
        .prefix("slug-e2e-")
        .tempdir_in(base)
        .expect("create disk tempdir")
}

fn find_host_executable(name: &str) -> PathBuf {
    let path_var = std::env::var_os("PATH").expect("PATH must be set");
    std::env::split_paths(&path_var)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| panic!("expected executable '{name}' on host PATH"))
}

fn write_executable_script(path: &Path, contents: &str) {
    let mut file = File::create(path).expect("create executable script");
    file.write_all(contents.as_bytes())
        .expect("write executable script");
    file.set_permissions(Permissions::from_mode(0o755))
        .expect("chmod 0755 script");
    file.sync_all().expect("sync script");
}

fn run_git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=Slipfork Test",
            "-c",
            "user.email=test@localhost",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "protocol.file.allow=always",
        ])
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {:?} failed in {}: {}",
        args,
        cwd.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn run_slipfork_with_env(
    scratch: &Path,
    cwd: &Path,
    env_overrides: &[(&str, &OsStr)],
    args: &[&str],
) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_slipfork"));
    cmd.env("GIT_CEILING_DIRECTORIES", scratch)
        .env("XDG_DATA_HOME", scratch.join("xdg-data"))
        .arg("--state-dir")
        .arg(scratch.join("state"))
        .args(args)
        .current_dir(cwd);
    for &(key, val) in env_overrides {
        cmd.env(key, val);
    }
    cmd.output().expect("spawn slipfork binary")
}

fn run_slipfork(scratch: &Path, cwd: &Path, args: &[&str]) -> Output {
    run_slipfork_with_env(scratch, cwd, &[], args)
}

#[test]
fn worktree_resolves_to_canonical_repo_slug_and_honors_only_vcs_root_slipfork_toml() {
    let tmp = disk_tempdir();
    let scratch = tmp.path();
    let repo = scratch.join("acme-app");
    fs::create_dir_all(repo.join("src")).expect("create repo/src");
    fs::write(repo.join("src/lib.txt"), "v1\n").expect("write input");
    run_git(&repo, &["init", "-q"]);
    fs::write(repo.join("snow.yml"), MINIMAL_SNOW_YML).expect("write snow.yml");
    run_git(&repo, &["add", "snow.yml", "src/lib.txt"]);
    run_git(&repo, &["commit", "-q", "-m", "init"]);

    let wt = repo.join(".worktrees").join("feat-auth");
    run_git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feat-auth",
            wt.to_str().expect("utf8 path"),
        ],
    );

    // (a) Inside `.worktrees/<fork>`, project slug resolves to canonical `acme-app`, never `feat-auth`.
    // Place a rogue worktree-local `slipfork.toml` (`project = "rogue-wt"` + rogue artifact):
    // the worktree-local file MUST be ignored by both `snow resolve` and `dag plan`.
    fs::write(
        wt.join("slipfork.toml"),
        "project = \"rogue-wt\"\n\n[artifacts.rogue]\ninputs = [\"src/**\"]\nbuild = \"echo rogue\"\nmount = \"dist\"\n",
    )
    .expect("write rogue toml");
    let wt_snow = wt.join("snow.yml");
    let out = run_slipfork(
        scratch,
        &wt,
        &[
            "snow",
            "resolve",
            "--file",
            wt_snow.to_str().expect("utf8"),
            "--mode",
            "local",
        ],
    );
    assert!(
        out.status.success(),
        "snow resolve failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("APP_URL=http://acme-app.localhost\n"),
        "expected canonical repo slug, got:\n{stdout}"
    );
    assert!(
        scratch
            .join("xdg-data/slipfork/vaults/acme-app.json")
            .is_file(),
        "vault must be keyed by canonical ProjectSlug"
    );

    // (b) Setting `project = "custom-slug"`, `[artifacts.front]`, and typed `[test]` coverage in
    // `<vcs_root>/slipfork.toml` wins cleanly in both `snow resolve` and default `dag plan`
    // even when `.worktrees/<fork>/slipfork.toml` says `rogue-wt`.
    fs::write(
        repo.join("slipfork.toml"),
        "project = \"custom-slug\"\n\n[artifacts.front]\ninputs = [\"src/**\"]\nbuild = \"echo ok\"\nmount = \"dist\"\n\n[test]\ncoverage = true\ncoverage_report = [\"text\"]\n",
    )
    .expect("write root slipfork.toml");

    let out_override = run_slipfork(
        scratch,
        &wt,
        &[
            "snow",
            "resolve",
            "--file",
            wt_snow.to_str().expect("utf8"),
            "--mode",
            "local",
        ],
    );
    assert!(
        out_override.status.success(),
        "snow resolve with root slipfork.toml failed: {}",
        String::from_utf8_lossy(&out_override.stderr)
    );
    let stdout_override = String::from_utf8_lossy(&out_override.stdout);
    assert!(
        stdout_override.contains("APP_URL=http://custom-slug.localhost\n"),
        "expected <vcs_root>/slipfork.toml override, got:\n{stdout_override}"
    );

    // `dag plan` (without `--file`) invoked from inside `.worktrees/<fork>` must default to
    // `<vcs_root>/slipfork.toml` (`artifacts.front`), never the rogue worktree-local file.
    let out_dag = run_slipfork(scratch, &wt, &["dag", "plan"]);
    assert!(
        out_dag.status.success(),
        "dag plan inside worktree failed: {}",
        String::from_utf8_lossy(&out_dag.stderr)
    );
    let dag_stdout = String::from_utf8_lossy(&out_dag.stdout);
    assert!(
        dag_stdout.contains("artifact front") && !dag_stdout.contains("rogue"),
        "expected default `dag plan` to read <vcs_root>/slipfork.toml, got:\n{dag_stdout}"
    );

    // (c) Explicit `--file` + `--repo` divergence must fail closed (both inside a VCS repo and
    // outside any VCS repo), whereas a consistent explicit pair outside any VCS repo works cleanly.
    let repo_canonical = fs::canonicalize(&repo).expect("canonicalize repo");
    let repo_toml = repo.join("slipfork.toml");
    let empty_dir = scratch.join("empty-input-dir");
    fs::create_dir_all(&empty_dir).expect("create empty-input-dir");

    let divergent_out = run_slipfork(
        scratch,
        scratch,
        &[
            "--json",
            "dag",
            "plan",
            "--file",
            repo_toml.to_str().expect("utf8"),
            "--repo",
            empty_dir.to_str().expect("utf8"),
        ],
    );
    assert!(
        !divergent_out.status.success(),
        "divergent --file (in repo) and --repo (outside repo) must fail closed"
    );
    let divergent_json: Value =
        serde_json::from_slice(&divergent_out.stdout).expect("divergent error json");
    let divergent_err = divergent_json["error"].as_str().expect("error string");
    assert!(
        divergent_err.contains(empty_dir.to_str().expect("utf8"))
            && divergent_err.contains(repo_toml.to_str().expect("utf8"))
            && divergent_err.contains(repo_canonical.to_str().expect("utf8"))
            && divergent_err.contains("VCS root"),
        "expected actionable divergence refusal naming both paths and VCS-root rule, got: {divergent_err}"
    );

    // Consistent explicit pair outside any VCS repository (`--file <dir>/slipfork.toml --repo <dir>`)
    // succeeds, while mismatched directories outside any VCS repository or cross-boundary pairs fail closed.
    let empty_toml = empty_dir.join("slipfork.toml");
    fs::copy(&repo_toml, &empty_toml).expect("copy slipfork.toml into standalone empty_dir");

    let other_standalone = scratch.join("other-standalone");
    fs::create_dir_all(&other_standalone).expect("create other-standalone");
    let inconsistent_outside_out = run_slipfork(
        scratch,
        scratch,
        &[
            "dag",
            "plan",
            "--file",
            empty_toml.to_str().expect("utf8"),
            "--repo",
            other_standalone.to_str().expect("utf8"),
        ],
    );
    assert!(
        !inconsistent_outside_out.status.success(),
        "mismatched --file and --repo outside any VCS repo must fail closed"
    );
    let inconsistent_outside_stderr = String::from_utf8_lossy(&inconsistent_outside_out.stderr);
    assert!(
        inconsistent_outside_stderr.contains(other_standalone.to_str().expect("utf8"))
            && inconsistent_outside_stderr.contains(empty_toml.to_str().expect("utf8")),
        "expected both paths in outside-repo mismatch refusal, got: {inconsistent_outside_stderr}"
    );

    let cross_boundary_out = run_slipfork(
        scratch,
        scratch,
        &[
            "dag",
            "plan",
            "--file",
            empty_toml.to_str().expect("utf8"),
            "--repo",
            repo.to_str().expect("utf8"),
        ],
    );
    assert!(
        !cross_boundary_out.status.success(),
        "--file outside VCS paired with --repo inside VCS must fail closed"
    );

    let empty_plan_out = run_slipfork(
        scratch,
        scratch,
        &[
            "--json",
            "dag",
            "plan",
            "--file",
            empty_toml.to_str().expect("utf8"),
            "--repo",
            empty_dir.to_str().expect("utf8"),
        ],
    );
    assert!(
        empty_plan_out.status.success(),
        "consistent explicit --file + --repo outside any VCS repo must succeed: {}",
        String::from_utf8_lossy(&empty_plan_out.stderr)
    );
    let empty_plan: Value = serde_json::from_slice(&empty_plan_out.stdout).expect("empty json");
    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    assert_eq!(
        empty_plan["artifacts"][0]["input_merkle"].as_str(),
        Some(EMPTY_SHA256)
    );
    let empty_artifact_digest = empty_plan["artifacts"][0]["digest"]
        .as_str()
        .expect("empty digest")
        .to_owned();
    // Poison the artifact cache with the empty-input digest: a buggy `cwd`-rooted planner in
    // `repo/nested/sub` (which has no `src/**`) would match zero files and report `cached`!
    fs::create_dir_all(
        scratch
            .join("state")
            .join("artifacts")
            .join(&empty_artifact_digest),
    )
    .expect("create poisoned empty artifact cache entry");

    let subdir = repo.join("nested").join("sub");
    fs::create_dir_all(&subdir).expect("create repo/nested/sub");
    let root_plan_out = run_slipfork(scratch, &repo, &["--json", "dag", "plan"]);
    assert!(root_plan_out.status.success());
    let root_plan: Value = serde_json::from_slice(&root_plan_out.stdout).expect("root plan json");

    let explicit_consistent_repo_out = run_slipfork(
        scratch,
        &subdir,
        &[
            "--json",
            "dag",
            "plan",
            "--file",
            repo_toml.to_str().expect("utf8"),
            "--repo",
            repo.to_str().expect("utf8"),
        ],
    );
    assert!(explicit_consistent_repo_out.status.success());
    let explicit_consistent_plan: Value =
        serde_json::from_slice(&explicit_consistent_repo_out.stdout)
            .expect("explicit consistent plan json");
    assert_eq!(explicit_consistent_plan, root_plan);

    let sub_plan_out = run_slipfork(scratch, &subdir, &["--json", "dag", "plan"]);
    assert!(
        sub_plan_out.status.success(),
        "dag plan in subdirectory failed: {}",
        String::from_utf8_lossy(&sub_plan_out.stderr)
    );
    let sub_plan: Value = serde_json::from_slice(&sub_plan_out.stdout).expect("sub plan json");
    assert_eq!(
        sub_plan, root_plan,
        "dag plan in subdirectory must match VCS-toplevel plan identically"
    );
    assert_ne!(
        sub_plan["artifacts"][0]["input_merkle"].as_str(),
        Some(EMPTY_SHA256),
        "subdirectory dag plan must hash <vcs_root>/src/**, never empty cwd"
    );
    assert_ne!(
        sub_plan["artifacts"][0]["digest"].as_str(),
        Some(empty_artifact_digest.as_str())
    );
    assert_eq!(
        sub_plan["artifacts"][0]["action"].as_str(),
        Some("rebuild"),
        "poisoned empty-input digest must never make real unmaterialized inputs report Cached"
    );

    // Editing `<vcs_root>/src/lib.txt` invalidates the digest observed from `repo/nested/sub`.
    fs::write(repo.join("src/lib.txt"), "v2\n").expect("update src/lib.txt");
    let sub_plan_v2_out = run_slipfork(scratch, &subdir, &["--json", "dag", "plan"]);
    assert!(sub_plan_v2_out.status.success());
    let sub_plan_v2: Value =
        serde_json::from_slice(&sub_plan_v2_out.stdout).expect("sub plan v2 json");
    assert_ne!(
        sub_plan_v2["artifacts"][0]["digest"], sub_plan["artifacts"][0]["digest"],
        "editing <vcs_root>/src/lib.txt must change the digest observed from a subdirectory"
    );

    // Passing `--repo .` explicitly in a subdirectory without `--file` must fail closed.
    let narrow_repo_out = run_slipfork(scratch, &subdir, &["dag", "plan", "--repo", "."]);
    assert!(
        !narrow_repo_out.status.success(),
        "explicit narrower --repo without --file must fail closed"
    );
    let narrow_stderr = String::from_utf8_lossy(&narrow_repo_out.stderr);
    assert!(
        narrow_stderr.contains("narrower than VCS root")
            && narrow_stderr.contains(repo_canonical.to_str().expect("utf8")),
        "expected actionable narrower --repo refusal, got: {narrow_stderr}"
    );

    // (d) Ambient `GIT_DIR` / `GIT_WORK_TREE` / `GIT_COMMON_DIR` in parent env must be scrubbed by
    // the thin git adapter so slug resolution cannot be hijacked.
    let hijack_git_dir = scratch.join("hijack-nonexistent-git-dir");
    let hijack_wt = scratch.join("hijack-nonexistent-wt");
    let hijack_common = scratch.join("hijack-nonexistent-common");
    let hijack_out = run_slipfork_with_env(
        scratch,
        &subdir,
        &[
            ("GIT_DIR", hijack_git_dir.as_os_str()),
            ("GIT_WORK_TREE", hijack_wt.as_os_str()),
            ("GIT_COMMON_DIR", hijack_common.as_os_str()),
        ],
        &[
            "snow",
            "resolve",
            "--file",
            "../../snow.yml",
            "--mode",
            "local",
        ],
    );
    assert!(
        hijack_out.status.success(),
        "ambient GIT_DIR/GIT_WORK_TREE must be scrubbed by adapter: {}",
        String::from_utf8_lossy(&hijack_out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&hijack_out.stdout)
            .contains("APP_URL=http://custom-slug.localhost\n")
    );
}

#[test]
fn git_submodule_bare_and_separate_git_dir_fail_closed_with_actionable_override_hints() {
    let tmp = disk_tempdir();
    let scratch = tmp.path();

    // 1. Bare repository (`git init --bare`) -> fail closed naming canonical bare path + clone hint.
    let bare = scratch.join("bare-origin.git");
    fs::create_dir_all(&bare).expect("create bare dir");
    run_git(&bare, &["init", "--bare", "-q"]);
    let bare_canonical = fs::canonicalize(&bare).expect("canonicalize bare");

    // Pass relative `snow.yml` so `parent_dir` exercises `start = "."` -> canonicalized bare path.
    fs::write(bare.join("snow.yml"), MINIMAL_SNOW_YML).expect("write bare snow.yml");
    let out_bare = run_slipfork(
        scratch,
        &bare,
        &["snow", "resolve", "--file", "snow.yml", "--mode", "local"],
    );
    assert!(
        !out_bare.status.success(),
        "bare repository must fail closed"
    );
    let bare_stderr = String::from_utf8_lossy(&out_bare.stderr);
    assert!(
        bare_stderr.contains("bare")
            && bare_stderr.contains(bare_canonical.to_str().expect("utf8"))
            && bare_stderr.contains("clone")
            && bare_stderr.contains("slipfork.toml")
            && !bare_stderr.contains("repository .:")
            && !bare_stderr.contains("<vcs_root>/slipfork.toml"),
        "expected bare repo refusal naming canonical path + reachable clone hint, got: {bare_stderr}"
    );

    // 2. Git submodule (`git submodule add` of a local repo) -> fail closed naming "submodule" + override,
    //    and succeed once `project = "..."` is set in `<submodule_toplevel>/slipfork.toml`.
    let upstream_mod = scratch.join("upstream-mod");
    fs::create_dir_all(&upstream_mod).expect("create upstream-mod");
    run_git(&upstream_mod, &["init", "-q"]);
    fs::write(upstream_mod.join("snow.yml"), MINIMAL_SNOW_YML).expect("write mod snow.yml");
    run_git(&upstream_mod, &["add", "snow.yml"]);
    run_git(&upstream_mod, &["commit", "-q", "-m", "init mod"]);

    let superproject = scratch.join("parent-monorepo");
    fs::create_dir_all(&superproject).expect("create parent-monorepo");
    run_git(&superproject, &["init", "-q"]);
    run_git(
        &superproject,
        &[
            "submodule",
            "add",
            "-q",
            upstream_mod.to_str().expect("utf8"),
            "vendor/sub-pkg",
        ],
    );

    let submod_dir = superproject.join("vendor").join("sub-pkg");
    let submod_canonical = fs::canonicalize(&submod_dir).expect("canonicalize submod");
    let out_submod = run_slipfork(
        scratch,
        &submod_dir,
        &["snow", "resolve", "--file", "snow.yml", "--mode", "local"],
    );
    assert!(
        !out_submod.status.success(),
        "git submodule without slipfork.toml override must fail closed"
    );
    let submod_stderr = String::from_utf8_lossy(&out_submod.stderr);
    assert!(
        submod_stderr.contains("submodule")
            && submod_stderr.contains("project =")
            && submod_stderr.contains(
                submod_canonical
                    .join("slipfork.toml")
                    .to_str()
                    .expect("utf8")
            ),
        "expected submodule refusal naming 'submodule' + concrete slipfork.toml path, got: {submod_stderr}"
    );

    // Rescue submodule via `<submodule_toplevel>/slipfork.toml`.
    fs::write(
        submod_dir.join("slipfork.toml"),
        "project = \"sub-pkg-override\"\n",
    )
    .expect("write submodule slipfork.toml");
    let out_submod_ok = run_slipfork(
        scratch,
        &submod_dir,
        &["snow", "resolve", "--file", "snow.yml", "--mode", "local"],
    );
    assert!(
        out_submod_ok.status.success(),
        "submodule with explicit project override should succeed: {}",
        String::from_utf8_lossy(&out_submod_ok.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out_submod_ok.stdout)
            .contains("APP_URL=http://sub-pkg-override.localhost\n")
    );

    // 3. Separate git dir (`git init --separate-git-dir=<dir> <repo>`) -> fail closed naming
    //    "separate git directory" + override hint (never taking slug from `admin-store`!).
    let admin_store = scratch.join("admin-store");
    let sep_repo = scratch.join("sep-checkout");
    run_git(
        scratch,
        &[
            "init",
            "-q",
            &format!("--separate-git-dir={}", admin_store.display()),
            sep_repo.to_str().expect("utf8"),
        ],
    );
    fs::write(sep_repo.join("snow.yml"), MINIMAL_SNOW_YML).expect("write sep snow.yml");
    let sep_canonical = fs::canonicalize(&sep_repo).expect("canonicalize sep");

    let out_sep = run_slipfork(
        scratch,
        &sep_repo,
        &["snow", "resolve", "--file", "snow.yml", "--mode", "local"],
    );
    assert!(
        !out_sep.status.success(),
        "--separate-git-dir checkout without override must fail closed"
    );
    let sep_stderr = String::from_utf8_lossy(&out_sep.stderr);
    assert!(
        sep_stderr.contains("separate git directory")
            && sep_stderr.contains("project =")
            && sep_stderr.contains(sep_canonical.join("slipfork.toml").to_str().expect("utf8")),
        "expected separate-git-dir refusal + concrete slipfork.toml hint, got: {sep_stderr}"
    );

    // Even when `--separate-git-dir` points to a path named `.git`, it must NOT be mistaken for a linked worktree!
    let sneaky_parent = scratch.join("sneaky-holder");
    fs::create_dir_all(&sneaky_parent).expect("create sneaky-holder");
    let sneaky_dot_git = sneaky_parent.join(".git");
    let sneaky_repo = scratch.join("sneaky-sep");
    run_git(
        scratch,
        &[
            "init",
            "-q",
            &format!("--separate-git-dir={}", sneaky_dot_git.display()),
            sneaky_repo.to_str().expect("utf8"),
        ],
    );
    fs::write(sneaky_repo.join("snow.yml"), MINIMAL_SNOW_YML).expect("write sneaky snow.yml");
    let out_sneaky = run_slipfork(
        scratch,
        &sneaky_repo,
        &["snow", "resolve", "--file", "snow.yml", "--mode", "local"],
    );
    assert!(
        !out_sneaky.status.success(),
        "--separate-git-dir ending in /.git must still fail closed"
    );
    assert!(
        String::from_utf8_lossy(&out_sneaky.stderr).contains("separate git directory"),
        "expected separate git directory error, got: {}",
        String::from_utf8_lossy(&out_sneaky.stderr)
    );

    // Rescue `--separate-git-dir` checkout via `<toplevel>/slipfork.toml`.
    fs::write(
        sep_repo.join("slipfork.toml"),
        "project = \"sep-rescued\"\n",
    )
    .expect("write sep slipfork.toml");
    let out_sep_ok = run_slipfork(
        scratch,
        &sep_repo,
        &["snow", "resolve", "--file", "snow.yml", "--mode", "local"],
    );
    assert!(
        out_sep_ok.status.success(),
        "separate-git-dir with explicit project override should succeed: {}",
        String::from_utf8_lossy(&out_sep_ok.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out_sep_ok.stdout)
            .contains("APP_URL=http://sep-rescued.localhost\n")
    );
}

#[test]
fn jj_absent_fails_closed_and_fake_jj_executable_on_child_path_proves_thin_cli_adapter() {
    let tmp = disk_tempdir();
    let scratch = tmp.path();

    // Workspace with `.jj` directory and a nested subdirectory (zero `.jj` internal pointer layout created!).
    let jj_ws = scratch.join("jj-workspace-dir");
    let jj_sub = jj_ws.join("nested").join("api");
    fs::create_dir_all(jj_ws.join(".jj")).expect("create .jj dir");
    fs::create_dir_all(&jj_sub).expect("create nested subdir");
    let snow_path = jj_sub.join("snow.yml");
    fs::write(&snow_path, MINIMAL_SNOW_YML).expect("write snow.yml");
    let jj_ws_canonical = fs::canonicalize(&jj_ws).expect("canonicalize jj_ws");

    // 1. Hermetic child PATH containing ONLY `git` (`jj` guaranteed absent without mutating process PATH).
    let no_jj_bin = scratch.join("bin-no-jj");
    fs::create_dir_all(&no_jj_bin).expect("create bin-no-jj");
    symlink(find_host_executable("git"), no_jj_bin.join("git")).expect("symlink git");

    let out_no_jj = run_slipfork_with_env(
        scratch,
        &jj_sub,
        &[("PATH", no_jj_bin.as_os_str())],
        &["snow", "resolve", "--file", "snow.yml", "--mode", "local"],
    );
    assert!(
        !out_no_jj.status.success(),
        ".jj workspace without jj on PATH and without override must fail closed"
    );
    let no_jj_stderr = String::from_utf8_lossy(&out_no_jj.stderr);
    assert!(
        no_jj_stderr.contains("jj")
            && no_jj_stderr.contains("workspace-scoped")
            && no_jj_stderr.contains(".worktrees/<fork>")
            && no_jj_stderr.contains("project =")
            && no_jj_stderr.contains(
                jj_ws_canonical
                    .join("slipfork.toml")
                    .to_str()
                    .expect("utf8")
            ),
        "expected actionable jj-absent error naming jj + workspace-scoped override, got: {no_jj_stderr}"
    );

    // Explicit `project = "..."` in `<jj_ws>/slipfork.toml` rescues the workspace even without `jj`.
    fs::write(
        jj_ws.join("slipfork.toml"),
        "project = \"jj-toml-rescue\"\n",
    )
    .expect("write jj slipfork.toml");
    let out_rescued = run_slipfork_with_env(
        scratch,
        &jj_sub,
        &[("PATH", no_jj_bin.as_os_str())],
        &["snow", "resolve", "--file", "snow.yml", "--mode", "local"],
    );
    assert!(
        out_rescued.status.success(),
        "jj workspace with explicit project override must succeed even when jj is absent: {}",
        String::from_utf8_lossy(&out_rescued.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out_rescued.stdout)
            .contains("APP_URL=http://jj-toml-rescue.localhost\n")
    );
    fs::remove_file(jj_ws.join("slipfork.toml")).expect("remove jj slipfork.toml");

    // 2. Fake `jj` executable on child-process PATH (process-local via `Command::env`, zero global leak):
    //    reports a distinct `jj-reported-root` directory and records `pwd` to verify `jj root`
    //    is invoked in `jj_ws` and its stdout drives the canonical slug.
    let reported_root = scratch.join("jj-reported-root");
    fs::create_dir_all(&reported_root).expect("create jj-reported-root");
    let reported_canonical = fs::canonicalize(&reported_root).expect("canonicalize reported_root");
    let cwd_log = scratch.join("fake-jj-cwd.txt");

    let fake_jj_bin = scratch.join("bin-fake-jj");
    fs::create_dir_all(&fake_jj_bin).expect("create bin-fake-jj");
    let script = format!(
        "#!/bin/sh\nset -eu\nif [ \"${{1:-}}\" = \"root\" ] && [ \"$#\" -eq 1 ]; then\n  pwd > '{}'\n  printf '%s\\n' '{}'\n  exit 0\nfi\necho \"unexpected jj invocation: $*\" >&2\nexit 2\n",
        cwd_log.display(),
        reported_canonical.display(),
    );
    write_executable_script(&fake_jj_bin.join("jj"), &script);

    let mut path_entries = vec![fake_jj_bin.clone()];
    if let Some(existing) = std::env::var_os("PATH") {
        path_entries.extend(std::env::split_paths(&existing));
    }
    let joined_path = std::env::join_paths(path_entries).expect("join PATH");

    let out_fake_jj = run_slipfork_with_env(
        scratch,
        &jj_sub,
        &[("PATH", joined_path.as_os_str())],
        &["snow", "resolve", "--file", "snow.yml", "--mode", "local"],
    );
    assert!(
        out_fake_jj.status.success(),
        "snow resolve with fake jj failed: {}",
        String::from_utf8_lossy(&out_fake_jj.stderr)
    );
    let fake_stdout = String::from_utf8_lossy(&out_fake_jj.stdout);
    assert!(
        fake_stdout.contains("APP_URL=http://jj-reported-root.localhost\n"),
        "expected slug from fake `jj root` stdout, got:\n{fake_stdout}"
    );
    let recorded_cwd = fs::canonicalize(
        fs::read_to_string(&cwd_log)
            .expect("read fake jj cwd log")
            .trim(),
    )
    .expect("canonicalize recorded cwd");
    assert_eq!(
        recorded_cwd, jj_ws_canonical,
        "expected `jj root` to execute in the `.jj`-containing ancestor directory"
    );

    // 3. Failing `jj root` command (without override) fails closed surfacing stderr + override hint.
    let fail_jj_bin = scratch.join("bin-fail-jj");
    fs::create_dir_all(&fail_jj_bin).expect("create bin-fail-jj");
    symlink(find_host_executable("git"), fail_jj_bin.join("git")).expect("symlink git");
    write_executable_script(
        &fail_jj_bin.join("jj"),
        "#!/bin/sh\necho 'Error: corrupted jj op store' >&2\nexit 1\n",
    );
    let out_fail_jj = run_slipfork_with_env(
        scratch,
        &jj_sub,
        &[("PATH", fail_jj_bin.as_os_str())],
        &["snow", "resolve", "--file", "snow.yml", "--mode", "local"],
    );
    assert!(!out_fail_jj.status.success());
    let fail_jj_stderr = String::from_utf8_lossy(&out_fail_jj.stderr);
    assert!(
        fail_jj_stderr.contains("corrupted jj op store")
            && fail_jj_stderr.contains("project =")
            && fail_jj_stderr.contains("slipfork.toml"),
        "expected failing jj root to surface stderr + override hint, got: {fail_jj_stderr}"
    );
}

#[test]
fn non_vcs_directory_fails_closed_and_corrupt_git_surfaces_real_git_stderr() {
    let tmp = disk_tempdir();
    let scratch = tmp.path();

    // Non-git/non-jj directory fails closed even if a local `slipfork.toml` exists.
    let plain = scratch.join("plain-dir");
    fs::create_dir_all(&plain).expect("create plain dir");
    fs::write(plain.join("snow.yml"), MINIMAL_SNOW_YML).expect("write snow.yml");
    fs::write(plain.join("slipfork.toml"), "project = \"acme\"\n").expect("write slipfork.toml");

    let out = run_slipfork(
        scratch,
        &plain,
        &["snow", "resolve", "--file", "snow.yml", "--mode", "local"],
    );
    assert!(
        !out.status.success(),
        "non-VCS directory must fail closed, stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not inside a git or jj repository") && stderr.contains("git init"),
        "expected actionable VCS init error, got: {stderr}"
    );

    // Broken `.git` pointer must surface git's stderr rather than pretending it is a non-repo.
    let broken = scratch.join("broken-git");
    fs::create_dir_all(&broken).expect("create broken dir");
    fs::write(broken.join(".git"), "gitdir: /nonexistent/worktree/admin\n")
        .expect("write broken .git");
    fs::write(broken.join("snow.yml"), MINIMAL_SNOW_YML).expect("write snow.yml");

    let out_broken = run_slipfork(
        scratch,
        &broken,
        &["snow", "resolve", "--file", "snow.yml", "--mode", "local"],
    );
    assert!(!out_broken.status.success());
    let broken_stderr = String::from_utf8_lossy(&out_broken.stderr);
    assert!(
        broken_stderr.contains("git rev-parse failed")
            && broken_stderr.contains("/nonexistent/worktree/admin"),
        "expected real git stderr, got: {broken_stderr}"
    );

    // Missing `git` binary on PATH must produce a distinct, actionable diagnostic naming `git`
    // rather than falsely claiming the directory is not a git or jj repository.
    let real_git_repo = scratch.join("git-missing-bin-repo");
    fs::create_dir_all(&real_git_repo).expect("create git-missing-bin-repo");
    run_git(&real_git_repo, &["init", "-q"]);
    fs::write(real_git_repo.join("snow.yml"), MINIMAL_SNOW_YML).expect("write snow.yml");

    let empty_bin = scratch.join("bin-empty");
    fs::create_dir_all(&empty_bin).expect("create bin-empty");
    let out_no_git = run_slipfork_with_env(
        scratch,
        &real_git_repo,
        &[("PATH", empty_bin.as_os_str())],
        &["snow", "resolve", "--file", "snow.yml", "--mode", "local"],
    );
    assert!(!out_no_git.status.success());
    let no_git_stderr = String::from_utf8_lossy(&out_no_git.stderr);
    assert!(
        no_git_stderr.contains("'git' executable was not found on PATH")
            && !no_git_stderr.contains("not inside a git or jj repository"),
        "expected missing git binary diagnostic, got: {no_git_stderr}"
    );
}

#[test]
fn invalid_project_label_fails_closed_until_rescued_by_valid_slipfork_toml_override() {
    let tmp = disk_tempdir();
    let scratch = tmp.path();

    // Repo basename `My_Project` is not a lowercase DNS label -> fails closed with actionable hint.
    let bad_repo = scratch.join("My_Project");
    fs::create_dir_all(&bad_repo).expect("create My_Project");
    run_git(&bad_repo, &["init", "-q"]);
    fs::write(bad_repo.join("snow.yml"), MINIMAL_SNOW_YML).expect("write snow.yml");

    let out_dir_err = run_slipfork(
        scratch,
        &bad_repo,
        &["snow", "resolve", "--file", "snow.yml", "--mode", "local"],
    );
    assert!(!out_dir_err.status.success());
    let err_text = String::from_utf8_lossy(&out_dir_err.stderr);
    assert!(
        err_text.contains("My_Project") && err_text.contains("slipfork.toml"),
        "expected invalid project slug diagnostic naming slipfork.toml, got: {err_text}"
    );

    // Invalid `project = "My_Project"` inside `slipfork.toml` ALSO fails closed in both commands.
    fs::write(bad_repo.join("slipfork.toml"), "project = \"My_Project\"\n")
        .expect("write bad slipfork.toml");
    let out_toml_err = run_slipfork(
        scratch,
        &bad_repo,
        &["snow", "resolve", "--file", "snow.yml", "--mode", "local"],
    );
    assert!(!out_toml_err.status.success());
    assert!(String::from_utf8_lossy(&out_toml_err.stderr).contains("My_Project"));

    let out_dag_err = run_slipfork(scratch, &bad_repo, &["dag", "plan"]);
    assert!(!out_dag_err.status.success());
    assert!(String::from_utf8_lossy(&out_dag_err.stderr).contains("My_Project"));

    // Valid `project = "my-project"` in `<vcs_root>/slipfork.toml` rescues the uppercase folder name.
    fs::write(bad_repo.join("slipfork.toml"), "project = \"my-project\"\n")
        .expect("write valid slipfork.toml");
    let out_rescued = run_slipfork(
        scratch,
        &bad_repo,
        &["snow", "resolve", "--file", "snow.yml", "--mode", "local"],
    );
    assert!(
        out_rescued.status.success(),
        "expected valid override to rescue My_Project: {}",
        String::from_utf8_lossy(&out_rescued.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out_rescued.stdout)
            .contains("APP_URL=http://my-project.localhost\n")
    );
}

#[test]
fn missing_snow_file_names_the_unreadable_path() {
    let tmp = disk_tempdir();
    let scratch = tmp.path();
    let missing = scratch.join("does-not-exist-snow.yml");
    let missing_str = missing.to_str().expect("utf8");

    // `slipfork exec --snow-file <missing>` includes the exact path and OS error cause.
    let exec_out = run_slipfork(
        scratch,
        scratch,
        &[
            "exec",
            "--world",
            "exp",
            "--snow-file",
            missing_str,
            "--",
            "true",
        ],
    );
    assert!(!exec_out.status.success());
    let exec_stderr = String::from_utf8_lossy(&exec_out.stderr);
    assert!(
        exec_stderr.contains(&format!("cannot read {missing_str}:")),
        "expected `cannot read <path>: <cause>`, got: {exec_stderr}"
    );
}
