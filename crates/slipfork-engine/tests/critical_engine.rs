//! Protected critical tests for `slipfork-engine` (`R10`, `R11`, `R12`).

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::thread;

use slipfork_engine::conformance::assert_world_engine_conformance;
use slipfork_engine::{
    EngineError, ExecSpec, HostBindMount, IncusEngine, ReflinkNetnsEngine, SnapshotId, WorldEngine,
    probe_reflink_dir,
};

fn xfs_test_dir(label: &str) -> PathBuf {
    let home = std::env::var("HOME").expect("HOME");
    let dir = PathBuf::from(home)
        .join(".cache")
        .join("slipfork-critical-tests")
        .join(format!("{label}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create xfs test dir");
    dir
}

#[test]
fn r10_tmpfs_fails_closed_without_silent_byte_copy_fallback() {
    let tmp_dir =
        PathBuf::from("/tmp/opencode").join(format!("slipfork-tmpfs-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp_dir);
    let err = probe_reflink_dir(&tmp_dir).expect_err("/tmp tmpfs must fail FICLONE closed");
    let _ = fs::remove_dir_all(&tmp_dir);
    assert!(matches!(err, EngineError::ReflinkUnsupported { .. }));
}

#[test]
fn r10_r11_reflink_netns_conformance_cow_isolation_uid_and_parallel_same_port() {
    let root = xfs_test_dir("reflink-netns");
    let uid = unsafe { libc::geteuid() };
    let gid = unsafe { libc::getegid() };
    let engine = ReflinkNetnsEngine::new(root.join("engine"), uid, gid);

    // 1. Shared WorldEngine conformance contract.
    assert_world_engine_conformance(&engine, &root);

    // 2. Create a source state tree and snapshot it via XFS FICLONE.
    let src_state = root.join("src-state");
    fs::create_dir_all(&src_state).expect("src_state");
    fs::write(src_state.join("seed.txt"), b"ancestor-row-1\n").expect("write seed");
    let snap = engine
        .create_snapshot(&src_state, "seed-v1", false)
        .expect("snapshot");

    // 3. Clone TWO worlds simultaneously; verify CoW isolation, $UID ownership, AND
    //    simultaneous binding of 127.0.0.1:18432 inside both isolated network namespaces!
    let ws_a = root.join("ws-a");
    let ws_b = root.join("ws-b");
    fs::create_dir_all(&ws_a).expect("ws_a");
    fs::create_dir_all(&ws_b).expect("ws_b");

    let w_a = engine
        .clone_world(
            &snap,
            "clone-a",
            &HostBindMount {
                host_path: ws_a.clone(),
            },
        )
        .expect("clone a");
    let w_b = engine
        .clone_world(
            &snap,
            "clone-b",
            &HostBindMount {
                host_path: ws_b.clone(),
            },
        )
        .expect("clone b");

    // Mutate state in clone-a; verify clone-b and snapshot stay untouched!
    let out_a = engine
        .exec_in_world(
            &w_a,
            &ExecSpec {
                argv: vec![
                    "sh".to_owned(),
                    "-c".to_owned(),
                    "echo mutated-a >> \"$SLIPFORK_WORLD_STATE/seed.txt\" && echo host-owned > generated.txt"
                        .to_owned(),
                ],
                cwd: ws_a.clone(),
                env: BTreeMap::new(),
            },
        )
        .expect("exec a");
    assert_eq!(out_a.exit_code, 0, "stderr: {}", out_a.stderr);

    // Verify file created inside world is owned by exact host UID/GID (never root:root!).
    let gen_meta = fs::metadata(ws_a.join("generated.txt")).expect("generated.txt");
    assert_eq!(gen_meta.uid(), uid);
    assert_eq!(gen_meta.gid(), gid);

    // Clone B still sees clean `ancestor-row-1`!
    let b_contents = fs::read_to_string(w_b.state_dir.join("seed.txt")).expect("read b");
    assert_eq!(b_contents, "ancestor-row-1\n");

    // Both worlds bind TCP 127.0.0.1:18432 simultaneously in parallel threads with ZERO EADDRINUSE!
    let eng1 = engine.clone();
    let eng2 = engine.clone();
    let wa_c = w_a.clone();
    let wb_c = w_b.clone();
    let bind_py = r#"
import socket, time
s = socket.socket()
s.bind(("127.0.0.1", 18432))
s.listen(1)
time.sleep(0.15)
print("BOUND_OK", s.getsockname()[1])
"#;
    let t1 = thread::spawn(move || {
        eng1.exec_in_world(
            &wa_c,
            &ExecSpec {
                argv: vec![
                    "uv".to_owned(),
                    "run".to_owned(),
                    "--no-project".to_owned(),
                    "python".to_owned(),
                    "-c".to_owned(),
                    bind_py.to_owned(),
                ],
                cwd: wa_c.workspace_dir.clone(),
                env: BTreeMap::new(),
            },
        )
        .expect("t1")
    });
    let t2 = thread::spawn(move || {
        eng2.exec_in_world(
            &wb_c,
            &ExecSpec {
                argv: vec![
                    "uv".to_owned(),
                    "run".to_owned(),
                    "--no-project".to_owned(),
                    "python".to_owned(),
                    "-c".to_owned(),
                    bind_py.to_owned(),
                ],
                cwd: wb_c.workspace_dir.clone(),
                env: BTreeMap::new(),
            },
        )
        .expect("t2")
    });
    let r1 = t1.join().expect("join 1");
    let r2 = t2.join().expect("join 2");
    assert_eq!(r1.exit_code, 0, "r1 stderr: {}", r1.stderr);
    assert_eq!(r2.exit_code, 0, "r2 stderr: {}", r2.stderr);
    assert!(r1.stdout.contains("BOUND_OK 18432"));
    assert!(r2.stdout.contains("BOUND_OK 18432"));

    engine.destroy_world(w_a).expect("destroy a");
    engine.destroy_world(w_b).expect("destroy b");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn r12_incus_engine_fails_closed_when_socket_missing_and_speaks_exact_rest_contract() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing_sock = dir.path().join("missing.socket");
    let missing_eng = IncusEngine::new(&missing_sock, "default", "acme-base", 1000, 1000);
    let err = missing_eng
        .clone_world(
            &SnapshotId("mig".to_owned()),
            "w1",
            &HostBindMount {
                host_path: dir.path().to_path_buf(),
            },
        )
        .expect_err("must fail closed when incusd socket is absent");
    assert!(matches!(err, EngineError::DaemonUnavailable { .. }));

    // Spin up a local Unix-socket HTTP /1.0 server verifying IncusEngine's conformance & wire contract!
    let sock_path = dir.path().join("incus.socket");
    let listener = UnixListener::bind(&sock_path).expect("bind unix");
    let server = thread::spawn(move || {
        for _ in 0..3 {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut raw = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                let n = stream.read(&mut chunk).expect("read");
                if n == 0 {
                    break;
                }
                raw.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&raw);
                if let Some((head, body_part)) = text.split_once("\r\n\r\n") {
                    let cl = head
                        .lines()
                        .find_map(|l| l.strip_prefix("Content-Length: "))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if body_part.len() >= cl {
                        break;
                    }
                }
            }
            let req = String::from_utf8_lossy(&raw);
            let first = req.lines().next().unwrap_or("");
            let (status, body) = if req.contains("definitely-missing-snapshot") {
                ("404 Not Found", r#"{"error":"not found","error_code":404}"#)
            } else {
                assert!(req.contains(r#""shift":"true""#) || first.starts_with("DELETE "));
                (
                    "200 OK",
                    r#"{"type":"sync","status":"Success","status_code":200,"metadata":{}}"#,
                )
            };
            let resp = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(resp.as_bytes()).expect("write");
        }
    });

    let incus = IncusEngine::new(&sock_path, "default", "acme-base", 1000, 1000);
    assert_world_engine_conformance(&incus, dir.path());

    let handle = incus
        .clone_world(
            &SnapshotId("seed-catalog".to_owned()),
            "w-clone-1",
            &HostBindMount {
                host_path: dir.path().to_path_buf(),
            },
        )
        .expect("incus clone");
    assert_eq!(handle.id, "w-clone-1");
    incus.destroy_world(handle).expect("incus destroy");
    server.join().expect("server thread");
}
