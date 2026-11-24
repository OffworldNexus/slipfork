//! One real daemon, one real Btrfs store, and the raw RPC helpers the lab journey shares.
use serde_json::{Value, json};
use slipfork_world::{BtrfsEngine, Dirtiness, StoreLayout, WorldEngine};
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
pub fn lab_root() -> PathBuf {
    let root = std::env::var_os("SLIPFORK_TEST_ROOT")
        .filter(|root| !root.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            panic!(
                "SLIPFORK_TEST_ROOT is unset\nnext: export SLIPFORK_TEST_ROOT=<a writable Btrfs directory>"
            )
        });
    fs::create_dir_all(&root).unwrap_or_else(|error| {
        panic!(
            "cannot create `{}`: {error}\nnext: set SLIPFORK_TEST_ROOT to a writable Btrfs directory",
            root.display()
        )
    });
    let is_btrfs = slipfork_world::is_btrfs_path(&root).unwrap_or_else(|error| {
        panic!(
            "cannot probe `{}`: {error}\nnext: set SLIPFORK_TEST_ROOT to a Btrfs directory",
            root.display()
        )
    });
    assert!(
        is_btrfs,
        "`{}` is not Btrfs\nnext: set SLIPFORK_TEST_ROOT to a Btrfs directory (e.g. /var/tmp on a Btrfs host)",
        root.display()
    );
    root
}
pub struct Lab {
    pub root: PathBuf,
    pub project: PathBuf,
    pub runtime: PathBuf,
    pub state: PathBuf,
    pub nonce: String,
    child: Option<Child>,
}
impl Lab {
    pub fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock reads after the epoch")
            .as_nanos();
        let nonce = format!("{}{nanos}", std::process::id());
        let root = lab_root().join(format!("slipfork-image-rpc-{nonce}"));
        let project = root.join("project");
        let runtime = root.join("run");
        let state = root.join("state");
        for directory in [&project, &runtime, &state] {
            fs::create_dir_all(directory).expect("create the image RPC lab directory");
        }
        Self {
            root,
            project,
            runtime,
            state,
            nonce,
            child: None,
        }
    }
    pub fn write_project(&self, reference: &str) {
        let forkfile = "# forkfile: 0.1\nimage app . {\n  dockerfile Dockerfile\n}\n";
        fs::write(self.project.join("Forkfile"), forkfile).expect("write the Forkfile");
        fs::write(
            self.project.join("Dockerfile"),
            format!(
                "FROM {reference}\nENV APP_MODE=dev\nENV LAB_NONCE={}\nWORKDIR /app\n",
                self.nonce
            ),
        )
        .expect("write the Dockerfile");
    }
    pub fn spawn_daemon(&mut self) {
        let log = fs::File::create(self.state.join("daemon.stderr")).expect("daemon stderr log");
        let child = Command::new(env!("CARGO_BIN_EXE_slipfork"))
            .arg("__daemon")
            .env("XDG_RUNTIME_DIR", &self.runtime)
            .env("XDG_STATE_HOME", &self.state)
            .env("XDG_DATA_HOME", &self.root)
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(log))
            .spawn()
            .expect("the slipfork binary must start the daemon");
        self.child = Some(child);
        let socket = self.runtime.join("daemon.sock");
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if UnixStream::connect(&socket).is_ok() {
                return;
            }
            thread::sleep(Duration::from_millis(25));
        }
        panic!("the daemon never bound `{}`", socket.display());
    }
    pub fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_slipfork"));
        command
            .current_dir(&self.project)
            .env("XDG_RUNTIME_DIR", &self.runtime)
            .env("XDG_STATE_HOME", &self.state)
            .env("XDG_DATA_HOME", &self.root)
            .env("NO_COLOR", "1");
        command
    }
    pub fn call(&self, id: &str, method: &str, params: &Value) -> (Vec<Value>, Value) {
        let mut stream = self.connect();
        send(&mut stream, id, method, params);
        let mut reader = BufReader::new(stream.try_clone().expect("clone the socket"));
        read_reply(&mut reader, id, false)
    }
    pub fn call_error(&self, id: &str, method: &str, params: &Value) -> Value {
        let mut stream = self.connect();
        send(&mut stream, id, method, params);
        let mut reader = BufReader::new(stream.try_clone().expect("clone the socket"));
        read_reply(&mut reader, id, true).1
    }
    fn connect(&self) -> UnixStream {
        UnixStream::connect(self.runtime.join("daemon.sock")).expect("connect the real daemon")
    }
    fn reap(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let engine = BtrfsEngine::new(StoreLayout::at(&self.root));
        reap_subvolumes(&engine, &self.root);
        let _ = fs::remove_dir_all(&self.root);
    }
}
impl Drop for Lab {
    fn drop(&mut self) {
        self.reap();
    }
}
fn reap_subvolumes(engine: &BtrfsEngine, path: &Path) {
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        reap_subvolumes(engine, &entry.path());
    }
    if slipfork_world::is_subvolume_path(path).unwrap_or(false) {
        let _ = engine.destroy(path, Dirtiness::Dirty, true);
    }
}
fn send(stream: &mut UnixStream, id: &str, method: &str, params: &Value) {
    let frame = json!({ "v": 1, "id": id, "method": method, "params": params });
    stream
        .write_all(format!("{frame}\n").as_bytes())
        .and_then(|()| stream.flush())
        .expect("send one request frame");
}
fn read_reply(
    reader: &mut BufReader<UnixStream>,
    id: &str,
    want_error: bool,
) -> (Vec<Value>, Value) {
    let mut events = Vec::new();
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).expect("read one daemon frame");
        assert!(read > 0, "the daemon closed the socket during `{id}`");
        let value: Value = serde_json::from_str(line.trim()).expect("every frame is JSON");
        assert_eq!(value["v"], 1, "{value}");
        assert_eq!(value["id"], id, "{value}");
        if let Some(event) = value.get("event") {
            events.push(event.clone());
        } else if let Some(error) = value.get("error") {
            if want_error {
                return (events, error.clone());
            }
            panic!("daemon `{id}` failed: {error}");
        } else if let Some(ok) = value.get("ok") {
            if want_error {
                panic!("daemon `{id}` unexpectedly succeeded: {ok}");
            }
            return (events, ok.clone());
        }
    }
}
pub fn refs_beside(rootfs: &str) -> BTreeMap<String, String> {
    let path = Path::new(rootfs)
        .parent()
        .expect("a rootfs has an oci parent")
        .join("refs.json");
    serde_json::from_slice(&fs::read(path).expect("read the real refs file"))
        .expect("refs.json is a map")
}
pub fn stderr_of(mut child: Child) -> String {
    let mut text = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut text);
    }
    text
}
