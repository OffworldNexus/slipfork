//! Protected end-to-end CLI critical tests for canonical VCS project identity (`H-005`),
//! fail-closed Git/Jujutsu topologies (owner ruling), and `snow resolve` / `dag plan` diagnostics.

use std::ffi::OsStr;
use std::fs::{self, File, Permissions};
use std::io::Write;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;

pub mod support;
use support::{DaemonLayout, disk_tempdir, run_git};

const MINIMAL_SNOW_YML: &str = r#"
modes:
  local:
    envs:
      app:
        APP_URL: '${{ resources::app.url }}'
        SECRET_KEY: '${{ secrets::SECRET_KEY !! random_string(32) }}'
"#;

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

fn run_slipfork_with_env(
    scratch: &Path,
    cwd: &Path,
    env_overrides: &[(&str, &OsStr)],
    args: &[&str],
) -> Output {
    let layout = DaemonLayout::under(scratch);
    support::ensure_daemon(scratch, &layout);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_slipfork"));
    cmd.env("GIT_CEILING_DIRECTORIES", scratch)
        .env("XDG_DATA_HOME", &layout.data_home)
        .arg("--state-dir")
        .arg(&layout.state_dir)
        .arg("--runtime-dir")
        .arg(&layout.runtime_dir)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Daemon-served diagnostics (`snow resolve`) auto-spawn `slipforkd` into this process
        // group; reaping the group after the CLI exits keeps the test host clean.
        .process_group(0)
        .current_dir(cwd);
    for &(key, val) in env_overrides {
        cmd.env(key, val);
    }
    let child = cmd.spawn().expect("spawn slipfork binary");
    let process_group = child.id();
    let output = child.wait_with_output().expect("wait for slipfork binary");
    let termination = support::terminate_process_group(process_group);
    // A diagnostic-only invocation (`dag plan`) never auto-spawns a daemon, so its group is already
    // empty here and `kill` reports `ESRCH`; a failed TERM is only tolerated when nothing is left to
    // reap, which the `kill -0` probe proves.
    assert!(
        termination.term_delivered || !termination.members_alive,
        "kill -TERM -{process_group} failed while the process group still had live members\n\
         offending invocation: slipfork {args:?}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // The daemon auto-spawned by the CLI lives in this process group and only stops serving once
    // its `SIGTERM` is serviced. A later invocation's `is_served()` only dials the socket, so it
    // would otherwise reuse a daemon that is exiting and fail mid-request (ECONNRESET/EPIPE).
    //
    // On a non-Btrfs host the harness serves the daemon in-process (it outlives every CLI child by
    // design), so there is no auto-spawned child to reap here.
    if support::host_is_btrfs(&layout.state_dir)
        && let Err(error) = support::wait_daemon_teardown(&layout.runtime_dir)
    {
        panic!(
            "the auto-spawned slipforkd must stop accepting after the process-group SIGTERM: {error}\n\
             offending invocation: slipfork {args:?}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    // A world PID 1 ignores `SIGTERM` and outlives a supervisor that was killed before it could
    // forward `--kill-child`, so the group signal alone is not a leak proof. This suite boots no
    // world, so the reap is a no-op today; it is the suite-exit proof that the auto-spawned daemon
    // group never leaves one behind.
    if let Err(survivors) = support::reap_world_init_processes(scratch) {
        panic!(
            "{survivors}\noffending invocation: slipfork {args:?}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    output
}

fn run_slipfork(scratch: &Path, cwd: &Path, args: &[&str]) -> Output {
    run_slipfork_with_env(scratch, cwd, &[], args)
}

#[test]
fn worktree_resolves_to_canonical_repo_slug_and_honors_only_vcs_root_slipfork_toml() {
    let tmp = disk_tempdir("slug-");
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
            .join("state/xdg-data/slipfork/vaults/acme-app.json")
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

    // (c) Explicit `--file` resolves its own VCS root: a config outside any repository fails
    // closed, and a standalone repository whose `inputs = ["src/**"]` match nothing plans cleanly
    // with the empty-input digest the cache-poisoning check below needs.
    let repo_toml = repo.join("slipfork.toml");
    let standalone = scratch.join("standalone");
    fs::create_dir_all(&standalone).expect("create standalone");
    let standalone_toml = standalone.join("slipfork.toml");
    fs::copy(&repo_toml, &standalone_toml).expect("copy slipfork.toml into standalone");

    let outside_out = run_slipfork(
        scratch,
        scratch,
        &[
            "dag",
            "plan",
            "--file",
            standalone_toml.to_str().expect("utf8"),
        ],
    );
    assert!(
        !outside_out.status.success(),
        "--file outside any VCS repository must fail closed"
    );
    let outside_stderr = String::from_utf8_lossy(&outside_out.stderr);
    assert!(
        outside_stderr.contains(standalone.to_str().expect("utf8"))
            && outside_stderr.contains("not a git or jj repository"),
        "expected the outside-repo refusal to name the config's directory, got: {outside_stderr}"
    );

    run_git(&standalone, &["init", "-q"]);
    let empty_plan_out = run_slipfork(
        scratch,
        scratch,
        &[
            "--json",
            "dag",
            "plan",
            "--file",
            standalone_toml.to_str().expect("utf8"),
        ],
    );
    assert!(
        empty_plan_out.status.success(),
        "dag plan over a standalone VCS repository must succeed: {}",
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

    let explicit_file_out = run_slipfork(
        scratch,
        &subdir,
        &[
            "--json",
            "dag",
            "plan",
            "--file",
            repo_toml.to_str().expect("utf8"),
        ],
    );
    assert!(explicit_file_out.status.success());
    let explicit_file_plan: Value =
        serde_json::from_slice(&explicit_file_out.stdout).expect("explicit --file plan json");
    assert_eq!(explicit_file_plan, root_plan);

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
    let tmp = disk_tempdir("slug-");
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
fn jj_workspace_uses_the_shared_git_resolver_and_never_shells_out_to_jj() {
    let tmp = disk_tempdir("slug-");
    let scratch = tmp.path();

    // 1. A Jujutsu-only workspace (no colocated Git store) fails closed through the one shared
    //    repository resolver: `slipfork-vcs` ships a Git resolver today, so no slug is invented and
    //    no `jj root` subprocess is spawned by the thin CLI.
    let jj_only = scratch.join("jj-only-workspace");
    let jj_only_sub = jj_only.join("nested").join("api");
    fs::create_dir_all(jj_only.join(".jj")).expect("create .jj dir");
    fs::create_dir_all(&jj_only_sub).expect("create nested subdir");
    fs::write(jj_only_sub.join("snow.yml"), MINIMAL_SNOW_YML).expect("write snow.yml");

    let out_jj_only = run_slipfork(
        scratch,
        &jj_only_sub,
        &["snow", "resolve", "--file", "snow.yml", "--mode", "local"],
    );
    assert!(
        !out_jj_only.status.success(),
        "a jj-only workspace without a colocated Git store must fail closed"
    );
    let jj_only_stderr = String::from_utf8_lossy(&out_jj_only.stderr);
    assert!(
        jj_only_stderr.contains("not a git or jj repository")
            && jj_only_stderr.contains("version-controlled checkout"),
        "expected the shared resolver's fail-closed diagnostic, got: {jj_only_stderr}"
    );

    // 2. A colocated jj+git checkout resolves through `GitRepoRootResolver`; the fake `jj` on the
    //    child PATH is never executed (it would create the marker file and fail).
    let colocated = scratch.join("jj-colocated-workspace");
    let colocated_sub = colocated.join("nested").join("api");
    fs::create_dir_all(colocated.join(".jj")).expect("create .jj dir");
    fs::create_dir_all(&colocated_sub).expect("create nested subdir");
    fs::write(colocated_sub.join("snow.yml"), MINIMAL_SNOW_YML).expect("write snow.yml");
    run_git(&colocated, &["init", "-q"]);
    run_git(&colocated, &["add", "nested/api/snow.yml"]);
    run_git(&colocated, &["commit", "-q", "-m", "init"]);
    fs::write(
        colocated.join("slipfork.toml"),
        "project = \"jj-colocated\"\n",
    )
    .expect("write colocated slipfork.toml");

    let jj_marker = scratch.join("jj-was-invoked.txt");
    let fake_bin = scratch.join("bin-colocated");
    fs::create_dir_all(&fake_bin).expect("create bin-colocated");
    symlink(find_host_executable("git"), fake_bin.join("git")).expect("symlink git");
    write_executable_script(
        &fake_bin.join("jj"),
        &format!(
            "#!/bin/sh\nprintf '%s' \"$*\" > '{}'\nexit 2\n",
            jj_marker.display()
        ),
    );

    let out_colocated = run_slipfork_with_env(
        scratch,
        &colocated_sub,
        &[("PATH", fake_bin.as_os_str())],
        &["snow", "resolve", "--file", "snow.yml", "--mode", "local"],
    );
    assert!(
        out_colocated.status.success(),
        "a colocated jj+git workspace must resolve through the Git resolver: {}",
        String::from_utf8_lossy(&out_colocated.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out_colocated.stdout)
            .contains("APP_URL=http://jj-colocated.localhost\n"),
        "expected the project override from <vcs_root>/slipfork.toml"
    );
    assert!(
        !jj_marker.exists(),
        "the thin CLI must never spawn `jj`; the shared resolver is Git-only (M2.12 followup)"
    );
}

#[test]
fn non_vcs_directory_fails_closed_and_corrupt_git_surfaces_real_git_stderr() {
    let tmp = disk_tempdir("slug-");
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
        stderr.contains("not a git or jj repository")
            && stderr.contains("version-controlled checkout"),
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
        broken_stderr.contains("git rev-parse")
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
    if support::host_is_btrfs(&scratch.join("state")) {
        assert!(!out_no_git.status.success());
        let no_git_stderr = String::from_utf8_lossy(&out_no_git.stderr);
        assert!(
            no_git_stderr.contains("required tool 'git' was not found on PATH")
                && !no_git_stderr.contains("not a git or jj repository"),
            "expected missing git binary diagnostic, got: {no_git_stderr}"
        );
    } else {
        // The missing-`git` diagnostic comes from the daemon-side `RepoRootResolver`, and the
        // daemon inherits the CLI's `PATH` only through a real auto-spawned `slipforkd`. On a
        // non-Btrfs host the harness serves the daemon in-process (this suite boots no world), so
        // the ambient `PATH` resolves the real repository through the typed control socket; the
        // invocation must therefore succeed and never emit the misleading non-repository refusal.
        // The missing-`git` PATH isolation runs on the Btrfs VM.
        let stderr = String::from_utf8_lossy(&out_no_git.stderr);
        let stdout = String::from_utf8_lossy(&out_no_git.stdout);
        assert!(
            out_no_git.status.success(),
            "the in-process daemon must resolve the real repository: {stderr}"
        );
        assert!(stdout.contains("APP_URL="), "resolved env: {stdout}");
        assert!(
            !stderr.contains("not a git or jj repository"),
            "a real repository must never be reported as a non-repository: {stderr}"
        );
        eprintln!("non-Btrfs host: the missing-`git` PATH isolation runs on the Btrfs VM");
    }
}

#[test]
fn invalid_project_label_fails_closed_until_rescued_by_valid_slipfork_toml_override() {
    let tmp = disk_tempdir("slug-");
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
    let tmp = disk_tempdir("slug-");
    let scratch = tmp.path();
    let missing = scratch.join("does-not-exist-snow.yml");
    let missing_str = missing.to_str().expect("utf8");

    // `slipfork snow resolve --file <missing>` includes the exact path and OS error cause.
    let resolve_out = run_slipfork(
        scratch,
        scratch,
        &["snow", "resolve", "--file", missing_str, "--mode", "local"],
    );
    assert!(!resolve_out.status.success());
    let resolve_stderr = String::from_utf8_lossy(&resolve_out.stderr);
    assert!(
        resolve_stderr.contains(&format!("cannot read {missing_str}:")),
        "expected `cannot read <path>: <cause>`, got: {resolve_stderr}"
    );
}
