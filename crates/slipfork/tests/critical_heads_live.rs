//! Live lab journey: the daemon's `WorldHeads` pre-plan and the supervisor's fail-closed heads
//! validation on a real Btrfs mount.
//!
//! One `FROM scratch` + `WORKDIR` + `ENV` image is sealed once (`built`) and re-planned warm (`cached`), then
//! the journey proves `__supervise` refuses a heads file whose rootfs seal was cleared, a corrupt
//! file, and a missing file — each with exactly one `next:` remedy. The lab is opt-in and fails
//! closed without `SLIPFORK_TEST_ROOT`, a Btrfs mount, `btrfs`, and `crun`.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use slipfork_world::{is_btrfs_path, is_subvolume_path};

const RUN_TIMEOUT: Duration = Duration::from_secs(240);

/// Isolated daemon runtime (`tmpfs`) and state directory (`SLIPFORK_TEST_ROOT` disk).
struct LabEnv {
    runtime: PathBuf,
    state: PathBuf,
}

impl LabEnv {
    fn isolated(root: &Path) -> Self {
        let base = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/dev/shm/slipfork-live-runtime"));
        fs::create_dir_all(&base).expect("create the live runtime base");
        let base = fs::canonicalize(&base).unwrap_or(base);
        let pid = std::process::id();
        let runtime = base.join(format!("r{pid}-heads"));
        let state = root.join(format!(".state-{pid}-heads"));
        fs::create_dir_all(&runtime).expect("create the isolated runtime directory");
        fs::create_dir_all(&state).expect("create the isolated state directory");
        Self { runtime, state }
    }
}

fn test_root() -> PathBuf {
    let Some(root) = std::env::var_os("SLIPFORK_TEST_ROOT").filter(|root| !root.is_empty()) else {
        panic!(
            "SLIPFORK_TEST_ROOT is unset or empty\nnext: SLIPFORK_TEST_ROOT=/mnt/slipfork-live make live-test"
        )
    };
    let root = PathBuf::from(root);
    if let Err(source) = fs::create_dir_all(&root) {
        panic!(
            "fail closed: cannot create `{}`: {source}\nnext: sudo chown -R \"$(id -u):$(id -g)\" {}",
            root.display(),
            root.display()
        );
    }
    let root = fs::canonicalize(&root).unwrap_or(root);
    assert!(
        is_btrfs_path(&root).unwrap_or(false),
        "fail closed: `{}` is not on Btrfs.\nnext: SLIPFORK_TEST_ROOT=/mnt/slipfork-live make live-test",
        root.display()
    );
    for tool in ["btrfs", "crun"] {
        assert!(
            Command::new(tool).arg("--version").output().is_ok(),
            "fail closed: `{tool}` is not on PATH.\nnext: install {tool}, then rerun this live test"
        );
    }
    root
}

fn unique_suffix() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock reads after the epoch")
        .as_nanos();
    format!("{}-{:08x}", std::process::id(), nanos as u32)
}

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_slipfork")
}

/// Runs one CLI invocation under the isolated runtime, reaping it within [`RUN_TIMEOUT`].
fn run(project: &Path, env: &LabEnv, args: &[&str]) -> Output {
    let mut child = Command::new(binary())
        .args(args)
        .current_dir(project)
        .env("XDG_RUNTIME_DIR", &env.runtime)
        .env("XDG_STATE_HOME", &env.state)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the slipfork binary must run");
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let out = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.take(1 << 20).read_to_end(&mut bytes);
        bytes
    });
    let err = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.take(1 << 20).read_to_end(&mut bytes);
        bytes
    });
    let deadline = Instant::now() + RUN_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => panic!("cannot wait for `slipfork {args:?}`: {error}"),
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "`slipfork {args:?}` did not finish within {}s\nnext: inspect `.slipfork/supervisor.log`",
                RUN_TIMEOUT.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    Output {
        status,
        stdout: out.join().expect("stdout reader"),
        stderr: err.join().expect("stderr reader"),
    }
}

fn heads_path(env: &LabEnv, world: &str) -> PathBuf {
    env.runtime
        .join("slipfork")
        .join("heads")
        .join(format!("{world}.json"))
}

fn read_heads(path: &Path) -> serde_json::Value {
    let bytes = fs::read(path).unwrap_or_else(|error| {
        panic!(
            "cannot read image heads `{}`: {error}\nnext: dump the isolated runtime directory",
            path.display()
        )
    });
    serde_json::from_slice(&bytes).expect("the daemon writes valid heads JSON")
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Asserts one fail-closed refusal with exactly one `next:` remedy and returns the whole output.
fn refused(output: &Output, needle: &str) -> String {
    let all = text(output);
    assert!(
        !output.status.success(),
        "the refusal must fail closed, saw success:\n{all}"
    );
    assert!(
        all.contains(needle),
        "expected `{needle}` in the refusal:\n{all}"
    );
    assert_eq!(
        all.matches("next: ").count(),
        1,
        "a refusal carries exactly one remedy:\n{all}"
    );
    all
}

fn sealed(path: &Path) -> bool {
    Command::new("btrfs")
        .args(["property", "get", "-ts"])
        .arg(path)
        .arg("ro")
        .output()
        .is_ok_and(|out| {
            out.status.success() && String::from_utf8_lossy(&out.stdout).contains("ro=true")
        })
}

/// One journey: seal, re-plan warm, then refuse unsealed/corrupt/missing heads.
#[test]
fn heads_seal_warm_reuse_and_refuse_unsealed_corrupt_or_missing_files() {
    let root = test_root();
    let env = LabEnv::isolated(&root);
    let world = format!("heads{}", unique_suffix());
    let project = root.join(&world);
    fs::create_dir_all(&project).expect("create the project");
    fs::write(
        project.join("Forkfile"),
        "# forkfile: 0.1\n\nimage web . {\n  dockerfile Dockerfile\n}\n",
    )
    .expect("write Forkfile");
    fs::write(
        project.join("Dockerfile"),
        "FROM scratch\nWORKDIR /app\nENV PORT=8080\n",
    )
    .expect("write Dockerfile");
    fs::write(project.join("seed.txt"), "seed").expect("write the build context");
    let heads = heads_path(&env, &world);

    // 1. The daemon seals the scratch image, then the supervisor validates the heads it boots.
    let up = run(&project, &env, &["up"]);
    assert!(
        up.status.success(),
        "a scratch image world must boot:\n{}",
        text(&up)
    );
    let sealed_heads = read_heads(&heads);
    assert_eq!(sealed_heads["v"], 1, "schema version");
    assert_eq!(sealed_heads["world"], world.as_str(), "world id");
    assert_eq!(
        sealed_heads["images"]["web"]["state"], "built",
        "the first plan seals a built head:\n{sealed_heads}"
    );
    let images = sealed_heads["images"]
        .as_object()
        .expect("one head per image");
    assert!(!images.is_empty(), "at least one sealed head");
    for (image, head) in images {
        let rootfs = PathBuf::from(
            head["rootfs"]
                .as_str()
                .unwrap_or_else(|| panic!("image `{image}` has no rootfs")),
        );
        assert!(
            is_subvolume_path(&rootfs).unwrap_or(false),
            "image `{image}` rootfs `{}` must be a Btrfs subvolume",
            rootfs.display()
        );
        assert!(
            sealed(&rootfs),
            "image `{image}` rootfs `{}` must be read-only sealed",
            rootfs.display()
        );
    }

    // 2. A stopped world re-plans warm: every sealed head is reused, never rebuilt.
    let down = run(&project, &env, &["down"]);
    assert!(down.status.success(), "down:\n{}", text(&down));
    let warm = run(&project, &env, &["up"]);
    assert!(warm.status.success(), "warm up:\n{}", text(&warm));
    assert_eq!(
        read_heads(&heads)["images"]["web"]["state"],
        "cached",
        "the second plan reuses the sealed head"
    );
    let down = run(&project, &env, &["down"]);
    assert!(down.status.success(), "down:\n{}", text(&down));

    // 3. A cleared seal is refused before any pod spawns.
    let rootfs = PathBuf::from(
        read_heads(&heads)["images"]["web"]["rootfs"]
            .as_str()
            .expect("rootfs path"),
    );
    let cleared = Command::new("btrfs")
        .args(["property", "set", "-ts"])
        .arg(&rootfs)
        .args(["ro", "false"])
        .status()
        .expect("run `btrfs property set`");
    assert!(
        cleared.success(),
        "clear the seal on `{}`",
        rootfs.display()
    );
    assert!(!sealed(&rootfs), "the rootfs seal must be cleared");
    let out = supervise(&project, &env, &world, &heads);
    refused(&out, "gone or unsealed");

    // 4. A corrupt heads file is refused, never re-guessed.
    fs::write(&heads, "not json").expect("corrupt the heads file");
    let out = supervise(&project, &env, &world, &heads);
    refused(&out, "cannot parse image heads");

    // 5. A missing heads file is refused, never rebuilt by the supervisor.
    fs::remove_file(&heads).expect("remove the heads file");
    let out = supervise(&project, &env, &world, &heads);
    refused(&out, "cannot read image heads");
}

/// Runs the hidden supervisor directly, so heads validation is observed before any pod spawns.
fn supervise(project: &Path, env: &LabEnv, world: &str, heads: &Path) -> Output {
    run(
        project,
        env,
        &[
            "__supervise",
            "--project",
            project.to_str().expect("utf-8 project"),
            "--name",
            world,
            "--heads",
            heads.to_str().expect("utf-8 heads"),
        ],
    )
}
