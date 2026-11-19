//! Protected end-to-end CLI critical tests for canonical VCS project identity (`H-005`),
//! fail-closed Git/Jujutsu topologies (owner ruling), and `slipfork env` diagnostics.
//!
//! `slipfork env` resolves the checkout itself (the thin client runs the one Git resolver), so the
//! journey proves the same facts the retired resolver verb used to: the canonical `<vcs_root>`
//! slug, the `project <slug>` override in `<vcs_root>/Forkfile`, and the vault key it derives. One
//! harness-owned daemon still serves the scratch layout for the whole journey, so every invocation
//! is also a process-group and world-residue proof. The `.jj` routing case keeps its own two
//! daemons because its subject *is* the daemon's `PATH`: a `jj`-less daemon must refuse with the
//! typed `ToolMissing` line while a `jj`-carrying daemon creates a Jujutsu workspace.

use std::ffi::OsStr;
use std::fs::{self, File, Permissions};
use std::io::Write;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

pub mod support;
use support::{Daemon, DaemonLayout, disk_tempdir, jj_binary, repo_with_manifest, run_git};

/// The manifest the topology journey resolves: one service, one visible variable, and one
/// generated secret, so every `env` read also proves the vault is written and the secret redacted.
const TOPOLOGY_FORKFILE: &str = r#"features {
    local
}

service api ./ {
    port 8000
    route /
    env {
        BASE_URL = app.url
        SECRET_KEY = secret(SECRET_KEY) ?? random_string(32)
    }
    run sleep 300
}
"#;

/// A manifest with no generated secret, for the `.jj` fork cases.
const PLAIN_FORKFILE: &str = r#"features {
    local
}

service api ./ {
    port 8000
    route /
    run sleep 300
}
"#;

/// The topology manifest with an optional `project <slug>` override at the top.
fn topology_forkfile(project: Option<&str>) -> String {
    match project {
        Some(slug) => format!("project {slug}\n\n{TOPOLOGY_FORKFILE}"),
        None => TOPOLOGY_FORKFILE.to_owned(),
    }
}

/// Write the journey `Forkfile`, optionally carrying a `project <slug>` override.
fn write_forkfile(repo: &Path, project: Option<&str>) {
    fs::write(repo.join("Forkfile"), topology_forkfile(project)).expect("write Forkfile");
}

/// The vault the client keys by the resolved canonical `ProjectSlug`.
fn vault(scratch: &Path, project: &str) -> PathBuf {
    DaemonLayout::under(scratch)
        .data_home
        .join("slipfork")
        .join("vaults")
        .join(format!("{project}.json"))
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

/// Own the harness daemon for one test.
///
/// Every invocation in this suite is client-side (`slipfork env`) or daemon-served (the `.jj`
/// forks), so the test starts the daemon once and binds the handle in its own scope:
/// `let daemon = harness_daemon(scratch);`. `Drop` → [`support::Daemon::shutdown`] kills and waits
/// a real `slipforkd` child (or unlinks the in-process control socket) and reaps the worlds it
/// booted, both processes and subvolumes.
fn harness_daemon(scratch: &Path) -> Daemon {
    support::ensure_daemon(scratch, &DaemonLayout::under(scratch), "env")
}

fn run_slipfork_with_env(
    scratch: &Path,
    daemon: &Daemon,
    cwd: &Path,
    env_overrides: &[(&str, &OsStr)],
    args: &[&str],
) -> Output {
    let layout = DaemonLayout::under(scratch);
    // The daemon must already be owned by the test (`harness_daemon`), so this invocation can never
    // silently start a daemon whose lifetime nobody supervises.
    assert!(
        support::daemon_is_served(&layout.runtime_dir),
        "the test must own the harness daemon (`let daemon = harness_daemon(scratch);`) before \
         invoking `slipfork {}`",
        args.join(" ")
    );
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_slipfork"));
    cmd.env("GIT_CEILING_DIRECTORIES", scratch)
        .env("XDG_DATA_HOME", &layout.data_home)
        .arg("--state-dir")
        .arg(&layout.state_dir)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // The CLI never auto-spawns a read/diagnostic verb (`M3.28.1` R5), so this group carries the
        // CLI alone; reaping it after the CLI exits is the leak proof that the invocation left no
        // process of its own behind.
        .process_group(0)
        .current_dir(cwd);
    for &(key, val) in env_overrides {
        cmd.env(key, val);
    }
    let child = cmd.spawn().expect("spawn slipfork binary");
    let process_group = child.id();
    let output = child.wait_with_output().expect("wait for slipfork binary");
    let termination = support::terminate_process_group(process_group);
    // Every invocation in this suite is daemon-served by the harness (`ensure_daemon`), so the
    // invocation's own process group is already empty here and `kill` reports `ESRCH`; a failed
    // TERM is only tolerated when nothing is left to reap, which the `kill -0` probe proves.
    assert!(
        termination.term_delivered || !termination.members_alive,
        "kill -TERM -{process_group} failed while the process group still had live members\n\
         offending invocation: slipfork {args:?}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // The daemon is the harness's own (`ensure_daemon`), so it is not a member of the invocation's
    // process group and every later invocation reuses it: the group `SIGTERM` must leave it serving
    // rather than take it down with the CLI (a reused, dying daemon would fail mid-request with
    // ECONNRESET/EPIPE).
    assert!(
        support::daemon_is_served(&layout.runtime_dir),
        "the harness daemon must outlive the invocation's process-group SIGTERM\n\
         offending invocation: slipfork {args:?}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // A world PID 1 ignores `SIGTERM` and outlives a supervisor that was killed before it could
    // forward `--kill-child`, so the group signal alone is not a leak proof. This suite boots no
    // world, so the reap is a no-op today; it is the per-invocation proof that no invocation leaves
    // a world init or a world subvolume behind.
    daemon.assert_no_world_residue(&format!(
        "offending invocation: slipfork {args:?}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    ));
    output
}

fn run_slipfork(scratch: &Path, daemon: &Daemon, cwd: &Path, args: &[&str]) -> Output {
    run_slipfork_with_env(scratch, daemon, cwd, &[], args)
}

/// The exact `KEY=value` document one default-feature `slipfork env` prints for
/// [`TOPOLOGY_FORKFILE`] resolved as `project`: the derived world URL and the redacted secret.
fn assert_topology_env(output: &Output, project: &str, what: &str) {
    support::assert_success(output, what);
    assert_eq!(
        support::stdout(output),
        format!("BASE_URL=http://{project}.localhost\nSECRET_KEY=[REDACTED]\n"),
        "{what} must resolve the redacted environment of project `{project}`"
    );
}

#[test]
fn one_daemon_resolves_every_vcs_topology_and_diagnostic_journey() {
    let tmp = disk_tempdir("slug-");
    let scratch = tmp.path();
    let daemon = harness_daemon(scratch);

    // ---------------------------------------------------------------------------------------
    // (1) A `.worktrees/<fork>` checkout resolves to the canonical repo slug, and only the
    //     `<vcs_root>/Forkfile` may override it.
    // ---------------------------------------------------------------------------------------
    {
        let repo = scratch.join("acme-app");
        fs::create_dir_all(repo.join("src")).expect("create repo/src");
        fs::write(repo.join("src/lib.txt"), "v1\n").expect("write input");
        run_git(&repo, &["init", "-q"]);
        write_forkfile(&repo, Some("acme-app"));
        run_git(&repo, &["add", "Forkfile", "src/lib.txt"]);
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

        // (a) Inside `.worktrees/<fork>`, the vault is keyed by canonical `acme-app`, never
        // `feat-auth`. A rogue worktree-local `project rogue-wt` MUST be ignored for keying: only
        // `<vcs_root>/Forkfile` names the project.
        fs::write(wt.join("Forkfile"), topology_forkfile(Some("rogue-wt")))
            .expect("write rogue Forkfile");
        let out = run_slipfork(scratch, &daemon, &wt, &["env"]);
        assert_topology_env(&out, "acme-app", "slipfork env inside .worktrees/feat-auth");
        assert!(
            vault(scratch, "acme-app").is_file(),
            "vault must be keyed by the canonical ProjectSlug"
        );
        assert!(
            !vault(scratch, "rogue-wt").exists() && !vault(scratch, "feat-auth").exists(),
            "a worktree-local project override must never re-key the vault"
        );

        // (b) `project custom-slug` in `<vcs_root>/Forkfile` wins cleanly, even when the worktree
        // manifest still says `rogue-wt`.
        write_forkfile(&repo, Some("custom-slug"));
        let out_override = run_slipfork(scratch, &daemon, &wt, &["env"]);
        assert_topology_env(
            &out_override,
            "custom-slug",
            "slipfork env with a root project override",
        );
        assert!(
            vault(scratch, "custom-slug").is_file(),
            "expected the <vcs_root>/Forkfile override, got no custom-slug vault"
        );

        // (d) Ambient `GIT_DIR` / `GIT_WORK_TREE` / `GIT_COMMON_DIR` in the parent environment must
        // be scrubbed by the thin git adapter so slug resolution cannot be hijacked.
        let subdir = repo.join("nested").join("sub");
        fs::create_dir_all(&subdir).expect("create repo/nested/sub");
        let hijack_git_dir = scratch.join("hijack-nonexistent-git-dir");
        let hijack_wt = scratch.join("hijack-nonexistent-wt");
        let hijack_common = scratch.join("hijack-nonexistent-common");
        let hijack_out = run_slipfork_with_env(
            scratch,
            &daemon,
            &subdir,
            &[
                ("GIT_DIR", hijack_git_dir.as_os_str()),
                ("GIT_WORK_TREE", hijack_wt.as_os_str()),
                ("GIT_COMMON_DIR", hijack_common.as_os_str()),
            ],
            &["env"],
        );
        assert_topology_env(
            &hijack_out,
            "custom-slug",
            "ambient GIT_DIR/GIT_WORK_TREE must be scrubbed by the adapter",
        );
        assert!(
            vault(scratch, "custom-slug").is_file(),
            "the scrubbed environment still resolves the root override"
        );
    }

    // ---------------------------------------------------------------------------------------
    // (2) Bare repositories, submodules, and `--separate-git-dir` checkouts fail closed with
    //     actionable override hints, and each is rescued by its `<vcs_root>/Forkfile`.
    // ---------------------------------------------------------------------------------------
    {
        // 1. Bare repository (`git init --bare`) -> fail closed naming the canonical bare path and
        //    a reachable clone hint.
        let bare = scratch.join("bare-origin.git");
        fs::create_dir_all(&bare).expect("create bare dir");
        run_git(&bare, &["init", "--bare", "-q"]);
        let bare_canonical = fs::canonicalize(&bare).expect("canonicalize bare");

        let out_bare = run_slipfork(scratch, &daemon, &bare, &["env"]);
        assert!(
            !out_bare.status.success(),
            "bare repository must fail closed"
        );
        let bare_stderr = String::from_utf8_lossy(&out_bare.stderr);
        assert!(
            bare_stderr.contains("bare")
                && bare_stderr.contains(bare_canonical.to_str().expect("utf8"))
                && bare_stderr.contains("clone")
                && bare_stderr.contains("project =")
                && !bare_stderr.contains("repository .:")
                && !bare_stderr.contains("<vcs_root>"),
            "expected bare repo refusal naming canonical path + reachable clone hint, got: {bare_stderr}"
        );

        // 2. Git submodule (`git submodule add` of a local repo) -> fail closed naming "submodule"
        //    and the override, then succeed once its `Forkfile` declares `project ...`.
        let upstream_mod = scratch.join("upstream-mod");
        fs::create_dir_all(&upstream_mod).expect("create upstream-mod");
        run_git(&upstream_mod, &["init", "-q"]);
        fs::write(upstream_mod.join("Forkfile"), PLAIN_FORKFILE).expect("write mod Forkfile");
        run_git(&upstream_mod, &["add", "Forkfile"]);
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
        let out_submod = run_slipfork(scratch, &daemon, &submod_dir, &["env"]);
        assert!(
            !out_submod.status.success(),
            "git submodule without a project override must fail closed"
        );
        let submod_stderr = String::from_utf8_lossy(&out_submod.stderr);
        assert!(
            submod_stderr.contains("submodule")
                && submod_stderr.contains("project =")
                && submod_stderr
                    .contains(submod_canonical.join("Forkfile").to_str().expect("utf8")),
            "expected submodule refusal naming 'submodule' + the concrete override path, got: {submod_stderr}"
        );

        fs::write(
            submod_dir.join("Forkfile"),
            topology_forkfile(Some("sub-pkg-override")),
        )
        .expect("write submodule Forkfile");
        let out_submod_ok = run_slipfork(scratch, &daemon, &submod_dir, &["env"]);
        assert_topology_env(
            &out_submod_ok,
            "sub-pkg-override",
            "submodule with an explicit project override should succeed",
        );
        assert!(
            vault(scratch, "sub-pkg-override").is_file(),
            "the submodule override keys its own vault"
        );

        // 3. Separate git dir (`git init --separate-git-dir=<dir> <repo>`) -> fail closed naming
        //    "separate git directory" and the override (never taking the slug from `admin-store`!).
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
        write_forkfile(&sep_repo, None);
        let sep_canonical = fs::canonicalize(&sep_repo).expect("canonicalize sep");

        let out_sep = run_slipfork(scratch, &daemon, &sep_repo, &["env"]);
        assert!(
            !out_sep.status.success(),
            "--separate-git-dir checkout without override must fail closed"
        );
        let sep_stderr = String::from_utf8_lossy(&out_sep.stderr);
        assert!(
            sep_stderr.contains("separate git directory")
                && sep_stderr.contains("project =")
                && sep_stderr.contains(sep_canonical.join("Forkfile").to_str().expect("utf8")),
            "expected separate-git-dir refusal + concrete override hint, got: {sep_stderr}"
        );

        // Even when `--separate-git-dir` points at a path named `.git`, it must NOT be mistaken for
        // a linked worktree.
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
        write_forkfile(&sneaky_repo, None);
        let out_sneaky = run_slipfork(scratch, &daemon, &sneaky_repo, &["env"]);
        assert!(
            !out_sneaky.status.success(),
            "--separate-git-dir ending in /.git must still fail closed"
        );
        assert!(
            String::from_utf8_lossy(&out_sneaky.stderr).contains("separate git directory"),
            "expected separate git directory error, got: {}",
            String::from_utf8_lossy(&out_sneaky.stderr)
        );

        write_forkfile(&sep_repo, Some("sep-rescued"));
        let out_sep_ok = run_slipfork(scratch, &daemon, &sep_repo, &["env"]);
        assert_topology_env(
            &out_sep_ok,
            "sep-rescued",
            "separate-git-dir with an explicit project override should succeed",
        );
        assert!(
            vault(scratch, "sep-rescued").is_file(),
            "the separate-git-dir override keys its own vault"
        );
    }

    // ---------------------------------------------------------------------------------------
    // (3) A Jujutsu-only workspace fails closed through the one shared resolver, and a colocated
    //     jj+git checkout resolves through the Git resolver without ever spawning `jj`.
    // ---------------------------------------------------------------------------------------
    {
        let jj_only = scratch.join("jj-only-workspace");
        let jj_only_sub = jj_only.join("nested").join("api");
        fs::create_dir_all(jj_only.join(".jj")).expect("create .jj dir");
        fs::create_dir_all(&jj_only_sub).expect("create nested subdir");
        write_forkfile(&jj_only_sub, None);

        let out_jj_only = run_slipfork(scratch, &daemon, &jj_only_sub, &["env"]);
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

        // A colocated jj+git checkout resolves through `GitRepoRootResolver`; the fake `jj` on the
        // child PATH is never executed (it would create the marker file and fail).
        let colocated = scratch.join("jj-colocated-workspace");
        let colocated_sub = colocated.join("nested").join("api");
        fs::create_dir_all(colocated.join(".jj")).expect("create .jj dir");
        fs::create_dir_all(&colocated_sub).expect("create nested subdir");
        write_forkfile(&colocated_sub, None);
        run_git(&colocated, &["init", "-q"]);
        write_forkfile(&colocated, Some("jj-colocated"));
        run_git(&colocated, &["add", "Forkfile", "nested/api/Forkfile"]);
        run_git(&colocated, &["commit", "-q", "-m", "init"]);

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
            &daemon,
            &colocated_sub,
            &[("PATH", fake_bin.as_os_str())],
            &["env"],
        );
        assert_topology_env(
            &out_colocated,
            "jj-colocated",
            "a colocated jj+git workspace must resolve through the Git resolver",
        );
        assert!(
            vault(scratch, "jj-colocated").is_file(),
            "expected the project override from <vcs_root>/Forkfile"
        );
        assert!(
            !jj_marker.exists(),
            "the thin CLI must never spawn `jj`; the shared resolver is Git-only (M2.12 followup)"
        );
    }

    // ---------------------------------------------------------------------------------------
    // (4) A non-VCS directory fails closed, a corrupt `.git` surfaces the real git stderr, and a
    //     client PATH without `git` fails closed with the typed remedy.
    // ---------------------------------------------------------------------------------------
    {
        let plain = scratch.join("plain-dir");
        fs::create_dir_all(&plain).expect("create plain dir");
        write_forkfile(&plain, None);

        let out = run_slipfork(scratch, &daemon, &plain, &["env"]);
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

        // A broken `.git` pointer must surface git's stderr rather than pretending it is a non-repo.
        let broken = scratch.join("broken-git");
        fs::create_dir_all(&broken).expect("create broken dir");
        fs::write(broken.join(".git"), "gitdir: /nonexistent/worktree/admin\n")
            .expect("write broken .git");
        write_forkfile(&broken, None);

        let out_broken = run_slipfork(scratch, &daemon, &broken, &["env"]);
        assert!(!out_broken.status.success());
        let broken_stderr = String::from_utf8_lossy(&out_broken.stderr);
        assert!(
            broken_stderr.contains("git rev-parse")
                && broken_stderr.contains("/nonexistent/worktree/admin"),
            "expected real git stderr, got: {broken_stderr}"
        );

        // `slipfork env` is a client-side verb: it runs the one Git resolver itself, so a client
        // PATH without `git` fails closed with the typed `ToolMissing` remedy instead of degrading.
        let real_git_repo = scratch.join("git-missing-bin-repo");
        fs::create_dir_all(&real_git_repo).expect("create git-missing-bin-repo");
        run_git(&real_git_repo, &["init", "-q"]);
        write_forkfile(&real_git_repo, Some("acme-app"));
        let ambient = run_slipfork(scratch, &daemon, &real_git_repo, &["env"]);
        assert_topology_env(
            &ambient,
            "acme-app",
            "slipfork env with the ambient client PATH",
        );
        let empty_bin = scratch.join("bin-empty");
        fs::create_dir_all(&empty_bin).expect("create bin-empty");
        let out_no_git = run_slipfork_with_env(
            scratch,
            &daemon,
            &real_git_repo,
            &[("PATH", empty_bin.as_os_str())],
            &["env"],
        );
        assert!(
            !out_no_git.status.success(),
            "a client PATH without `git` must fail closed"
        );
        let no_git_stderr = String::from_utf8_lossy(&out_no_git.stderr);
        assert!(
            no_git_stderr.contains("git")
                && no_git_stderr.contains("not found on PATH")
                && no_git_stderr.contains("install Git"),
            "expected the typed ToolMissing remedy naming git, got: {no_git_stderr}"
        );
    }

    // ---------------------------------------------------------------------------------------
    // (5) An invalid project label fails closed until a valid `project <slug>` override rescues
    //     it, and a missing `Forkfile` names the unreadable path with its remedy.
    // ---------------------------------------------------------------------------------------
    {
        // Repo basename `My_Project` is not a lowercase DNS label -> fails closed with an
        // actionable hint.
        let bad_repo = scratch.join("My_Project");
        fs::create_dir_all(&bad_repo).expect("create My_Project");
        run_git(&bad_repo, &["init", "-q"]);
        write_forkfile(&bad_repo, None);

        let out_dir_err = run_slipfork(scratch, &daemon, &bad_repo, &["env"]);
        assert!(!out_dir_err.status.success());
        let err_text = String::from_utf8_lossy(&out_dir_err.stderr);
        assert!(
            err_text.contains("My_Project")
                && err_text.contains("not a single lowercase DNS label"),
            "expected invalid project slug diagnostic, got: {err_text}"
        );

        // An invalid `project My_Project` inside the Forkfile ALSO fails closed.
        write_forkfile(&bad_repo, Some("My_Project"));
        let out_manifest_err = run_slipfork(scratch, &daemon, &bad_repo, &["env"]);
        assert!(!out_manifest_err.status.success());
        assert!(String::from_utf8_lossy(&out_manifest_err.stderr).contains("My_Project"));

        // A valid `project my-project` rescues the uppercase folder name.
        write_forkfile(&bad_repo, Some("my-project"));
        let out_rescued = run_slipfork(scratch, &daemon, &bad_repo, &["env"]);
        assert_topology_env(
            &out_rescued,
            "my-project",
            "a valid override must rescue the My_Project folder name",
        );
        assert!(
            vault(scratch, "my-project").is_file(),
            "the valid override keys its own vault"
        );

        // A checkout with no manifest is one fail-closed line naming the path and the remedy.
        let empty_repo = scratch.join("manifest-less-repo");
        fs::create_dir_all(&empty_repo).expect("create manifest-less repo");
        run_git(&empty_repo, &["init", "-q"]);
        let missing = empty_repo.join("Forkfile");
        let missing_str = missing.to_str().expect("utf8");
        let resolve_out = run_slipfork(scratch, &daemon, &empty_repo, &["env"]);
        assert!(!resolve_out.status.success());
        let resolve_stderr = String::from_utf8_lossy(&resolve_out.stderr);
        assert!(
            resolve_stderr.contains(&format!("cannot read {missing_str}:"))
                && resolve_stderr.contains("next: add the project `Forkfile`"),
            "expected `cannot read <path>; next: ...`, got: {resolve_stderr}"
        );
    }
}

/// Start the real daemon with an explicit `PATH`, or assert its fail-closed non-Btrfs refusal.
fn daemon_with_path(scratch: &Path, path: &Path) -> Option<Daemon> {
    match Daemon::try_start_with_path(scratch, Some(path)) {
        Ok(daemon) => Some(daemon),
        Err(log) => {
            assert!(
                !support::host_is_btrfs(&scratch.join("state")),
                "the real daemon refused a Btrfs host: {log}"
            );
            support::assert_btrfs_refusal(&log);
            eprintln!("non-Btrfs host: `.jj` routing runs on the Btrfs VM");
            None
        }
    }
}

#[test]
fn dot_jj_repository_routes_to_jujutsu_and_never_falls_back_to_git() {
    let jj = jj_binary().unwrap_or_else(|| {
        panic!(
            "jj is required for the .jj routing test; install Jujutsu or place the pinned binary in ~/.cache/slipfork-tools/jj/jj"
        )
    });
    let jj_dir = jj.parent().expect("jj parent").to_path_buf();

    // Negative: a `.jj` repository with no `jj` on PATH fails closed with ToolMissing and never
    // creates a Git worktree.
    let tmp = disk_tempdir("jj-");
    let scratch = tmp.path();
    let repo = repo_with_manifest(scratch, "acme-jj-absent", PLAIN_FORKFILE);
    fs::create_dir(repo.join(".jj")).expect("create .jj marker");
    let git_only = support::git_only_path(scratch);
    let Some(daemon) = daemon_with_path(scratch, &git_only) else {
        return;
    };
    let refused = daemon.cli(&repo, &["fork", "feat-jj"]);
    assert!(
        !refused.status.success(),
        "a .jj repository without jj must fail closed"
    );
    let diagnostic = support::stderr(&refused);
    assert!(
        diagnostic.contains("jj") && diagnostic.contains("not found on PATH"),
        "the refusal must be the typed ToolMissing diagnostic: {diagnostic}"
    );
    assert!(
        !repo.join(".worktrees/feat-jj").exists(),
        "a refused Jujutsu fork must never create a Git worktree"
    );

    // Positive: the same `.jj` repository with `jj` on PATH creates a Jujutsu workspace.
    let tmp = disk_tempdir("jj-");
    let scratch = tmp.path();
    let repo = repo_with_manifest(scratch, "acme-jj", PLAIN_FORKFILE);
    let status = Command::new(&jj)
        .args(["git", "init", "--colocate"])
        .current_dir(&repo)
        .env("JJ_USER", "Slipfork Test")
        .env("JJ_EMAIL", "test@localhost")
        .status()
        .expect("spawn jj");
    assert!(status.success(), "jj git init --colocate failed");
    let mut path = std::ffi::OsString::from(jj_dir);
    path.push(":");
    path.push(std::env::var_os("PATH").expect("PATH"));
    let Some(daemon) = daemon_with_path(scratch, Path::new(&path)) else {
        return;
    };
    assert_success(&daemon.cli(&repo, &["fork", "feat-jj"]), "fork feat-jj");
    assert!(
        repo.join(".worktrees/feat-jj").is_dir(),
        "the jj workspace checkout must exist"
    );
    assert_success(&daemon.cli(&repo, &["drop", "feat-jj"]), "drop feat-jj");
}

/// Assert a CLI invocation succeeded, naming it in the failure.
fn assert_success(output: &Output, what: &str) {
    assert!(
        output.status.success(),
        "{what} failed with {}:\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
