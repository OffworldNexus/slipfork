//! Lab journey over the pod/control surface: a real disk bundle, real Unix-domain control framing,
//! a real `socketpair` descriptor handoff, and real log files under `/var/tmp`.
//!
//! No mocks, no in-memory vault, no fake clocks: every assertion runs against the shipped `crun`
//! bundle writer, the real `SCM_RIGHTS` path, and on-disk pod logs.

use std::fs;
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use slipfork_forkfile::BucketPolicy;
use slipfork_world::uapi::{
    FS_INFO, FsInfoArgs, IOC_READ, IOC_WRITE, SNAP_CREATE_V2, SNAP_DESTROY_V2, SUBVOL_CREATE,
    SUBVOL_GETFLAGS, SUBVOL_SETFLAGS, VolArgs, VolArgsV2, request,
};
use slipfork_world::{
    Bind, BindSource, BootGraph, BootKind, BootStep, Error, ForkName, FrameLines, ImageFacts,
    LogKind, LogLine, LogReader, LogSink, NamespaceIdentity, NamespaceTable, PROTOCOL_VERSION,
    PodEnv, PodFile, PodHost, PodSpec, PostgresLauncher, RedisLauncher, Request, RuntimePods,
    S3Bucket, S3Credentials, StoreLayout, WorldName, WorldPlan, control, decode_request,
    pod_log::{
        INDEX_BYTES, INDEX_ENTRY_BYTES, INDEX_LINES, LogMerge, index_path, load_index, read_range,
    },
    recv_fd, send_fd, write_bundle,
};

fn lab(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    let root = PathBuf::from("/var/tmp").join(format!(
        "slipfork-world-{prefix}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&root).expect("the lab needs real disk under /var/tmp");
    root
}

fn host() -> PodHost {
    PodHost {
        crun: "/usr/bin/crun".into(),
        resolv_conf: Some("/etc/resolv.conf".into()),
        uid: 1000,
        gid: 1001,
    }
}

fn spec(root: &Path) -> PodSpec {
    PodSpec {
        id: "api".to_owned(),
        rootfs: root.to_path_buf(),
        workdir: "/project".to_owned(),
        argv: vec!["python3".to_owned(), "-V".to_owned()],
        inherited_env: vec!["PATH=/usr/local/bin:/usr/bin".to_owned()],
        env: vec![PodEnv::new("PORT", "8000"), PodEnv::new("SECRET", "s3cr3t")],
        binds: vec![
            Bind {
                source: BindSource::Host(root.join("project")),
                destination: "/project".to_owned(),
                read_only: false,
            },
            Bind {
                source: BindSource::Bundle("app.json".to_owned()),
                destination: "/etc/example/app.json".to_owned(),
                read_only: true,
            },
        ],
        files: vec![PodFile {
            name: "app.json".to_owned(),
            contents: "{\"name\":\"api\"}".to_owned(),
        }],
    }
}

/// The bundle the supervisor hands to `crun`: world network, mapped identity, container cwd.
#[test]
fn a_pod_bundle_keeps_the_world_network_and_never_leaks_secrets() {
    let lab = lab("pod");
    let root = lab.join("rootfs");
    let bundle_dir = lab.join("bundle");
    fs::create_dir_all(root.join("project")).expect("seed rootfs");
    fs::create_dir_all(root.join("etc/example")).expect("seed config dir");
    let pod = spec(&root);
    pod.validate().expect("the sample spec is valid");

    let written = write_bundle(&pod, &host(), &bundle_dir).expect("write the OCI bundle");
    assert_eq!(written.config, bundle_dir.join("config.json"));
    assert_eq!(
        fs::metadata(&written.config).expect("stat config").mode() & 0o777,
        0o600,
        "the bundle config carries pod env and must stay private"
    );

    let json: serde_json::Value =
        serde_json::from_slice(&fs::read(&written.config).expect("read config"))
            .expect("parse config");
    let namespaces = json["linux"]["namespaces"].as_array().expect("namespaces");
    assert!(
        !namespaces.iter().any(|ns| ns["type"] == "network"),
        "the pod must join the world network namespace"
    );
    assert!(namespaces.iter().any(|ns| ns["type"] == "ipc"));
    assert!(namespaces.iter().any(|ns| ns["type"] == "uts"));
    assert_eq!(json["linux"]["uidMappings"][0]["hostID"], 1000);
    assert_eq!(json["linux"]["gidMappings"][0]["hostID"], 1001);
    assert_eq!(json["process"]["user"]["uid"], 1000);
    assert_eq!(
        json["process"]["cwd"], "/project",
        "the OCI cwd is a container path, never the host rootfs path"
    );
    assert_eq!(json["hostname"], "api");
    assert!(
        json["process"]["env"]
            .as_array()
            .expect("env")
            .iter()
            .any(|value| value == "SECRET=s3cr3t")
    );
    let passwd = fs::read_to_string(bundle_dir.join("passwd")).expect("read passwd");
    assert!(passwd.contains("slipfork:x:1000:1001:"));
    assert!(
        bundle_dir.join("app.json").is_file(),
        "declared files ride the bundle"
    );

    let debug = format!("{pod:?}");
    assert!(!debug.contains("s3cr3t"), "{debug}");
    assert!(!debug.contains("identities"), "{debug}");
    assert!(debug.contains("[REDACTED]"), "{debug}");

    let mut escaped = spec(&root);
    escaped.id = "../../escape".to_owned();
    assert!(escaped.validate().is_err(), "pod ids may not traverse");
    let mut undeclared = spec(&root);
    undeclared.files.clear();
    assert!(
        undeclared.validate().is_err(),
        "a bundle bind needs a declared file"
    );
    let mut traversal = spec(&root);
    traversal.binds[0].destination = "/project/../etc".to_owned();
    assert!(
        traversal.validate().is_err(),
        "bind destinations may not traverse"
    );
    let mut bad_key = spec(&root);
    bad_key.env.push(PodEnv::new("BAD=KEY", "x"));
    assert!(bad_key.validate().is_err(), "env keys are validated");
    let _ = fs::remove_dir_all(&lab);
}

/// Backing resources: bucket identities validate, policies gate anonymous reads, and the native
/// S3 endpoint is container-free while postgres/redis still render real pods.
#[test]
fn backing_resource_specs_validate_and_native_s3_has_no_pod() {
    let buckets = [
        S3Bucket::new("uploads", BucketPolicy::Private).expect("valid bucket"),
        S3Bucket::new("assets", BucketPolicy::PublicRead).expect("valid bucket"),
    ];
    assert_eq!(buckets[0].policy(), BucketPolicy::Private);
    assert!(!buckets[0].public_read());
    assert!(!buckets[0].permits_anonymous("GET"));
    assert!(buckets[1].public_read());
    assert!(buckets[1].permits_anonymous("GET"));
    assert!(buckets[1].permits_anonymous("HEAD"));
    assert!(!buckets[1].permits_anonymous("PUT"));
    let credentials = S3Credentials::new("AKIA", "topsecret");
    assert!(!format!("{credentials:?}").contains("AKIA"));
    assert!(!format!("{credentials:?}").contains("topsecret"));
    let local = S3Credentials::local("demo");
    assert_eq!(local.access_key(), "slipfork");
    assert_eq!(local.secret_key(), "slipfork-demo-s3");
    for bad in ["", "ab", "UPPER", "-lead", "trail-", "with space"] {
        assert!(
            S3Bucket::new(bad, BucketPolicy::Private).is_err(),
            "`{bad}`"
        );
    }
    // Native S3 lives at the fixed loopback port and needs no image or pod.
    let s3 = slipfork_world::ResourceKind::S3;
    assert_eq!(s3.image(), None);
    assert_eq!(s3.port(), 8333);
    assert_eq!(s3.data_dir(), "s3");

    let rootfs = lab("pg-rootfs");
    fs::create_dir_all(rootfs.join("usr/lib/postgresql/18/bin")).expect("rootfs bin dir");
    fs::write(rootfs.join("usr/lib/postgresql/18/bin/postgres"), b"ELF").expect("seed postgres");
    fs::create_dir_all(rootfs.join("usr/local/bin")).expect("rootfs bin dir");
    fs::write(rootfs.join("usr/local/bin/redis-server"), b"ELF").expect("seed redis-server");
    let store = StoreLayout::at(lab("resources-store"));
    let world = WorldName::parse("demo").expect("valid world");
    let facts = ImageFacts {
        env: vec!["PATH=/usr/local/bin:/usr/bin".to_owned()],
        entrypoint: vec!["docker-entrypoint.sh".to_owned()],
        cmd: vec!["postgres".to_owned()],
    };
    let pg = PostgresLauncher::new(store.clone(), world.clone())
        .spec("pg", &rootfs, &rootfs.join("shm"), &facts)
        .expect("render the postgres pod");
    assert_eq!(
        pg.argv[0], "/usr/lib/postgresql/18/bin/postgres",
        "the launcher execs postgres itself, never docker-entrypoint.sh"
    );
    assert_eq!(pg.argv[1..3], ["-D", "/var/lib/postgresql/data"]);
    assert!(
        !pg.argv
            .iter()
            .any(|arg| arg.ends_with("docker-entrypoint.sh")),
        "the image entrypoint must be bypassed"
    );
    assert_eq!(
        pg.binds[0].source,
        BindSource::Host(store.world_clone(&world, slipfork_world::ResourceKind::Postgres, "pg"))
    );
    assert!(pg.validate().is_ok());
    let cache = RedisLauncher::new(store.clone(), world.clone())
        .spec("cache", &rootfs, &facts)
        .expect("render the redis pod");
    assert!(cache.argv.iter().any(|arg| arg == "127.0.0.1"));
    assert_eq!(cache.binds[0].destination, "/data");
    assert!(cache.validate().is_ok());
    let _ = fs::remove_dir_all(&rootfs);
    let _ = fs::remove_dir_all(store.store_root());
}

/// Store layout, tmpfs runtime roots, and namespace identity are derived, never global.
#[test]
fn layout_runtime_and_namespace_roots_are_injected_and_fail_closed() {
    let from_mount = StoreLayout::new("/mnt/btrfs");
    let from_store = StoreLayout::at("/mnt/btrfs/.slipfork-store");
    assert_eq!(from_mount, from_store);
    assert_eq!(from_store.btrfs_mount(), Path::new("/mnt/btrfs"));
    assert_eq!(
        from_store.worlds_root(),
        Path::new("/mnt/btrfs/.slipfork-store/worlds")
    );
    let world = WorldName::parse("demo").expect("valid world");
    let fork = ForkName::parse("feat-auth").expect("valid fork");
    assert_eq!(
        from_store.world(&world),
        Path::new("/mnt/btrfs/.slipfork-store/worlds/demo")
    );
    assert_eq!(
        from_store.fork_world(&world, &fork),
        Path::new("/mnt/btrfs/.slipfork-store/worlds/feat-auth--demo")
    );
    assert_eq!(
        StoreLayout::project_fork(Path::new("/mnt/btrfs/projects/demo"), &fork),
        Path::new("/mnt/btrfs/projects/demo/.forks/feat-auth")
    );
    for bad in ["", ".", "..", "a/b", "Upper", "-lead", "with space"] {
        assert!(WorldName::parse(bad).is_err(), "`{bad}`");
        assert!(ForkName::parse(bad).is_err(), "`{bad}`");
    }
    for good in ["demo", "s3", "feat-auth", "a.b_c-d", "9lives"] {
        assert!(WorldName::parse(good).is_ok(), "`{good}`");
        assert!(ForkName::parse(good).is_ok(), "`{good}`");
    }
    let (long_fork, long_world) =
        StoreLayout::parse_fork_world(std::ffi::OsStr::new("x--a--b")).expect("fork/world pair");
    assert_eq!(long_fork.as_str(), "x--a");
    assert_eq!(long_world.as_str(), "b");
    assert!(StoreLayout::parse_fork_world(std::ffi::OsStr::new("demo")).is_none());

    let shm = Path::new("/dev/shm").join(format!("slipfork-rt-{}", std::process::id()));
    let pods = RuntimePods::prepare(&shm, &world).expect("prepare on real tmpfs");
    assert_eq!(pods.pod_dir("api"), pods.path().join("api"));
    assert_eq!(
        fs::metadata(pods.path()).expect("stat").mode() & 0o777,
        0o700
    );
    let _ = fs::remove_dir_all(&shm);
    let disk = lab("runtime-disk");
    assert!(
        matches!(
            RuntimePods::prepare(&disk, &world),
            Err(Error::RuntimeNotTmpfs { .. })
        ),
        "a persistent runtime root must be refused"
    );

    let identity = NamespaceIdentity::observe().expect("read /proc/self/ns");
    let table = NamespaceTable::from_proc(std::process::id()).expect("table for self");
    table.verify(&identity).expect("identity must match");
    assert!(table.path("net").is_some());
    let identity_path = disk.join("inside.json");
    identity.write(&identity_path).expect("write identity");
    assert_eq!(
        NamespaceIdentity::read(&identity_path).expect("read identity"),
        identity
    );
    let _ = fs::remove_dir_all(&disk);
}

/// Control framing over a real `socketpair`, plus the AF_UNIX path and dial fail-closed rules.
#[test]
fn control_requests_round_trip_over_a_real_socket_and_refuse_bad_input() {
    let request = Request::Launch {
        v: PROTOCOL_VERSION,
        id: "req-1".to_owned(),
        pod: spec(Path::new("/images/alpine")),
    };
    let (mut writer, mut reader) = UnixStream::pair().expect("real socketpair");
    let bytes = slipfork_world::encode(&request).expect("encode");
    assert_eq!(bytes.last(), Some(&b'\n'));
    writer.write_all(&bytes).expect("write the frame");
    writer.shutdown(Shutdown::Write).expect("half-close");
    let mut received = Vec::new();
    reader.read_to_end(&mut received).expect("read the frame");
    let decoded =
        decode_request(std::str::from_utf8(&received).expect("utf8").trim()).expect("decode");
    assert_eq!(decoded, request);
    assert_eq!(decoded.id(), "req-1");

    let wrong = slipfork_world::encode(&Request::StopAll {
        v: PROTOCOL_VERSION + 1,
        id: "req-2".to_owned(),
    })
    .expect("encode");
    assert!(decode_request(std::str::from_utf8(&wrong).expect("utf8").trim()).is_err());
    assert!(slipfork_world::decode_init_frame("not json").is_err());

    let pods_root = Path::new("/run/user/1000/slipfork/pods/demo");
    assert!(
        control::control_socket_path(pods_root)
            .expect("short tmpfs path")
            .ends_with(control::CONTROL_SOCKET)
    );
    let long = format!("/run/user/1000/{}", "a".repeat(120));
    assert!(
        control::control_socket_path(Path::new(&long)).is_err(),
        "an overlong sun_path must fail closed"
    );
    assert!(matches!(
        control::dial_tcp(Path::new("/var/tmp/no-such-world"), 8000),
        Err(Error::Io { .. })
    ));
    assert!(
        control::dial_tcp(Path::new("/var/tmp/no-such-world"), 0).is_err(),
        "port 0 must be refused"
    );
}

/// Canonical log paths, `ExecPty`/`ExecResize` framing, and the console-socket path bound.
#[test]
fn pty_requests_and_canonical_log_paths_round_trip() {
    let request = Request::ExecPty {
        v: PROTOCOL_VERSION,
        id: "pty-1".to_owned(),
        pod: "api".to_owned(),
        argv: vec!["/bin/sh".to_owned()],
        cols: 120,
        rows: 40,
    };
    let decoded = decode_request(
        std::str::from_utf8(&slipfork_world::encode(&request).unwrap())
            .unwrap()
            .trim(),
    )
    .expect("decode exec-pty");
    assert_eq!(decoded, request);
    assert_eq!(decoded.id(), "pty-1");
    let resize = Request::ExecResize {
        v: PROTOCOL_VERSION,
        id: "pty-1".to_owned(),
        cols: 80,
        rows: 24,
    };
    let decoded = decode_request(
        std::str::from_utf8(&slipfork_world::encode(&resize).unwrap())
            .unwrap()
            .trim(),
    )
    .expect("decode exec-resize");
    assert_eq!(decoded, resize);
    assert_eq!(decoded.id(), "pty-1");
    let wrong = Request::ExecPty {
        v: PROTOCOL_VERSION + 1,
        id: "pty-2".to_owned(),
        pod: "api".to_owned(),
        argv: vec!["/bin/sh".to_owned()],
        cols: 80,
        rows: 24,
    };
    assert!(
        decode_request(
            std::str::from_utf8(&slipfork_world::encode(&wrong).unwrap())
                .unwrap()
                .trim()
        )
        .is_err()
    );

    let store = StoreLayout::at("/var/tmp/store/.slipfork-store");
    let world = WorldName::parse("demo").expect("valid world");
    assert_eq!(
        store.logs_root(),
        Path::new("/var/tmp/store/.slipfork-store/logs")
    );
    assert_eq!(
        store.world_logs(&world),
        Path::new("/var/tmp/store/.slipfork-store/logs/demo")
    );
    assert_eq!(
        store.pod_log(&world, "api"),
        Path::new("/var/tmp/store/.slipfork-store/logs/demo/api.log")
    );
    let rootfs = store.world_pods(&world).join("svc-api");
    assert_eq!(
        StoreLayout::from_pod_rootfs(&rootfs),
        Some((store.clone(), world.clone()))
    );
    assert!(
        StoreLayout::from_pod_rootfs(Path::new("/run/user/1000/slipfork/pods/demo/api")).is_none(),
        "a tmpfs run-once bundle is not a store rootfs"
    );
    let socket = control::console_socket_path(Path::new("/run/user/1000/slipfork/pods/demo"), 3)
        .expect("short console socket path");
    assert_eq!(
        socket.file_name().and_then(|name| name.to_str()),
        Some("pty-3.sock")
    );

    let lab = lab("canonical-log");
    let dir = lab.join("store").join("logs").join("demo");
    slipfork_world::pod_log::append(&dir, "api", b"build output\n").expect("append a build log");
    let path = slipfork_world::pod_log::log_path(&dir, "api");
    assert_eq!(
        fs::metadata(&dir).expect("stat leaf").mode() & 0o777,
        0o700,
        "the world log directory stays private"
    );
    assert_eq!(
        fs::metadata(dir.parent().expect("logs parent"))
            .expect("stat parent")
            .mode()
            & 0o777,
        0o700,
        "the whole `<store>/logs` chain is created private"
    );
    let raw = fs::read_to_string(&path).expect("read the log");
    let rows: Vec<&str> = raw.lines().collect();
    assert_eq!(rows.len(), 1, "{raw:?}");
    assert!(rows[0].ends_with("\tL\tbuild output"), "{raw:?}");
    assert!(
        rows[0]
            .split_once('\t')
            .is_some_and(|(ts, _)| ts.parse::<u64>().is_ok()),
        "each append row carries a millisecond timestamp: {raw:?}"
    );
    slipfork_world::pod_log::append(&dir, "api", b"more\n").expect("append again");
    let raw = fs::read_to_string(&path).expect("read the log");
    let rows: Vec<&str> = raw.lines().collect();
    assert_eq!(rows.len(), 2, "{raw:?}");
    assert!(rows[1].ends_with("\tL\tmore"), "{raw:?}");
    assert_eq!(fs::metadata(&path).expect("stat").mode() & 0o777, 0o600);
    let _ = fs::remove_dir_all(&lab);
}

/// `SCM_RIGHTS` over a real socketpair: one descriptor survives, plain payloads do not invent one.
#[test]
fn descriptor_passing_moves_the_payload_and_the_file() {
    let (sender, receiver) = UnixStream::pair().expect("real socketpair");
    let scratch = lab("fdpass");
    let payload_path = scratch.join("payload.txt");
    fs::write(&payload_path, "scm-rights").expect("seed the payload file");
    let file = fs::File::open(&payload_path).expect("open the payload file");
    send_fd(&sender, b"one", Some(file.as_fd())).expect("send one descriptor");
    let (payload, descriptor) = recv_fd(&receiver).expect("receive one descriptor");
    assert_eq!(payload, b"one");
    let mut received = fs::File::from(descriptor.expect("one descriptor arrived"));
    let mut text = String::new();
    received
        .read_to_string(&mut text)
        .expect("read through the fd");
    assert_eq!(text, "scm-rights");

    send_fd(&sender, b"plain", None).expect("send a plain payload");
    let (payload, descriptor) = recv_fd(&receiver).expect("receive a plain payload");
    assert_eq!(payload, b"plain");
    assert!(descriptor.is_none(), "no descriptor may be invented");

    let (closed_sender, closed_receiver) = UnixStream::pair().expect("real socketpair");
    drop(closed_sender);
    assert_eq!(
        recv_fd(&closed_receiver)
            .expect_err("a closed peer must not yield a frame")
            .kind(),
        std::io::ErrorKind::UnexpectedEof
    );
    let _ = fs::remove_dir_all(&scratch);
}

/// Pod logs: private files, timestamped dividers, and a merge that orders across two pods.
#[test]
fn pod_logs_merge_by_timestamp_across_two_real_files() {
    let lab = lab("podlog");
    let mut api = LogSink::open(&lab, "api").expect("api sink");
    assert_eq!(
        fs::metadata(api.path()).expect("stat").mode() & 0o777,
        0o600
    );
    let mut worker = LogSink::open(&lab, "worker").expect("worker sink");
    api.append_at(10, LogKind::Divider, "── api started ──")
        .expect("divider");
    api.append_at(40, LogKind::Line, "a4").expect("a4");
    worker.append_at(10, LogKind::Line, "b1").expect("b1");
    worker.append_at(20, LogKind::Line, "b2").expect("b2");
    api.append_at(30, LogKind::Line, "a3").expect("a3");

    let mut merge = LogMerge::default();
    for (pod, path) in [("api", api.path()), ("worker", worker.path())] {
        let mut reader = LogReader::open(path).expect("replay reader");
        reader
            .read_lines(&mut |line| merge.push(pod, line))
            .expect("replay");
        reader.flush(&mut |line| merge.push(pod, line));
    }
    let merged: Vec<(String, LogLine)> = merge.flush();
    let texts: Vec<&str> = merged
        .iter()
        .filter(|(_, line)| line.kind != LogKind::Divider)
        .map(|(_, line)| line.text.as_str())
        .collect();
    assert_eq!(texts, ["b1", "b2", "a3", "a4"], "{merged:?}");
    assert_eq!(
        merged
            .iter()
            .filter(|(_, line)| line.kind == LogKind::Divider)
            .count(),
        1
    );

    let mut window = LogMerge::default();
    window.push("api", LogLine::new(1_000, LogKind::Line, "old"));
    window.push("api", LogLine::new(1_400, LogKind::Line, "new"));
    let drained: Vec<String> = window
        .drain(1_200)
        .into_iter()
        .map(|(_, line)| line.text)
        .collect();
    assert_eq!(drained, ["old"]);
    assert_eq!(window.flush().len(), 1);
    let _ = fs::remove_dir_all(&lab);
}

/// The `.idx` stays dense and a `since` window seeks past the head of a real capture file.
#[test]
fn indexed_read_range_seeks_past_the_head_of_a_real_capture() {
    let lab = lab("podlog-window");
    let mut sink = LogSink::open(&lab, "api").expect("api sink");
    for step in 0..200_u64 {
        sink.append_at(1_000 + step * 10, LogKind::Line, &format!("line-{step:03}"))
            .expect("append line");
    }
    drop(sink);

    let index = index_path(&lab, "api");
    let meta = fs::metadata(&index).expect("the .idx is written beside the log");
    assert_eq!(meta.mode() & 0o777, 0o600, "the index is private");
    assert_eq!(
        meta.len() % INDEX_ENTRY_BYTES as u64,
        0,
        "fixed 16-byte entries"
    );
    let entries = load_index(&lab, "api").expect("valid index");
    assert_eq!(
        entries[0].byte_offset, 0,
        "the first line is always indexed"
    );
    assert!(
        entries.len() >= 4,
        "one entry per <= {INDEX_LINES} lines: {entries:?}"
    );
    for pair in entries.windows(2) {
        assert!(
            pair[1].byte_offset - pair[0].byte_offset <= INDEX_BYTES,
            "indexed gaps stay <= {INDEX_BYTES} bytes: {entries:?}"
        );
    }

    let since = 1_000 + 130 * 10;
    let mut range = read_range(&lab, "api", Some(since), None).expect("windowed reader");
    assert!(
        range.start_offset() > 0,
        "a `since` window must seek to the indexed boundary, not byte zero"
    );
    let mut seen = Vec::new();
    while range
        .read_lines(&mut |line| seen.push(line))
        .expect("read window")
        > 0
    {}
    range.flush(&mut |line| seen.push(line));
    assert_eq!(seen.len(), 70, "only lines at/after {since}");
    assert_eq!(
        seen[0].text, "line-130",
        "the first returned line starts the window"
    );
    assert!(seen.iter().all(|line| line.ts_ms >= since));

    let until = 1_000 + 160 * 10;
    let mut bounded = read_range(&lab, "api", Some(since), Some(until)).expect("bounded reader");
    let mut window = Vec::new();
    while bounded
        .read_lines(&mut |line| window.push(line))
        .expect("read bounded window")
        > 0
    {}
    bounded.flush(&mut |line| window.push(line));
    assert_eq!(window.len(), 31, "strictly inside [{since}, {until}]");
    assert!(
        window
            .iter()
            .all(|line| line.ts_ms >= since && line.ts_ms <= until)
    );

    // A deleted index is rebuilt explicitly, never silently bypassed.
    fs::remove_file(&index).expect("drop the index");
    assert_eq!(load_index(&lab, "api").expect("rebuilt index"), entries);
    let rebuilt = read_range(&lab, "api", Some(since), None).expect("rebuilt reader");
    assert!(rebuilt.start_offset() > 0);
    let _ = fs::remove_dir_all(&lab);
}

/// Boot graph: dependencies gate, and a cycle is refused before any node is released.
#[test]
fn boot_graph_refuses_a_dependency_cycle() {
    let step = |label: &str, deps: Vec<usize>| BootStep {
        kind: BootKind::Setup,
        image: None,
        subject: label.to_owned(),
        phase: None,
        label: label.to_owned(),
        command: Some("true".to_owned()),
        stamp: None,
        uses_env: false,
        line: 1,
        deps,
    };
    let cyclic = WorldPlan {
        steps: vec![step("a", vec![1]), step("b", vec![0])],
        ..WorldPlan::default()
    };
    assert!(matches!(
        BootGraph::new(&cyclic.steps),
        Err(Error::BootCycle { .. })
    ));
}

/// Frame scanning splits complete lines and bounds an oversized line at `cap`.
#[test]
fn framed_lines_split_complete_lines_and_bound_oversized_ones() {
    let mut lines = FrameLines::new();
    assert_eq!(
        lines.feed(b"one\ntwo\npart", 16),
        vec![b"one".to_vec(), b"two".to_vec()]
    );
    assert_eq!(lines.feed(b"ial\n", 16), vec![b"partial".to_vec()]);
    let cut = lines.feed(&[b'x'; 40], 16);
    assert_eq!(cut.len(), 2, "{cut:?}");
    assert!(cut.iter().all(|line| line.len() == 16), "{cut:?}");
    assert_eq!(lines.pending_len(), 8, "the tail waits for the next feed");
}

/// The kernel Btrfs UAPI is pinned by value, independent of any installed `<linux/btrfs.h>`.
#[test]
fn btrfs_uapi_requests_and_struct_layouts_are_pinned() {
    assert_eq!(request(IOC_WRITE, 14, 4096), 0x5000_940e, "SUBVOL_CREATE");
    assert_eq!(request(IOC_WRITE, 23, 4096), 0x5000_9417, "SNAP_CREATE_V2");
    assert_eq!(
        request(IOC_WRITE, 24, 4096),
        0x5000_9418,
        "SUBVOL_CREATE_V2"
    );
    assert_eq!(request(IOC_READ, 25, 8), 0x8008_9419, "SUBVOL_GETFLAGS");
    assert_eq!(request(IOC_WRITE, 26, 8), 0x4008_941a, "SUBVOL_SETFLAGS");
    assert_eq!(request(IOC_READ, 31, 1024), 0x8400_941f, "FS_INFO");
    assert_eq!(request(IOC_WRITE, 63, 4096), 0x5000_943f, "SNAP_DESTROY_V2");
    assert_eq!(SUBVOL_CREATE, 0x5000_940e);
    assert_eq!(SNAP_CREATE_V2, 0x5000_9417);
    assert_eq!(SUBVOL_GETFLAGS, 0x8008_9419);
    assert_eq!(SUBVOL_SETFLAGS, 0x4008_941a);
    assert_eq!(FS_INFO, 0x8400_941f);
    assert_eq!(
        SNAP_DESTROY_V2, 0x5000_943f,
        "command 63, never the ENOTTY 47"
    );

    assert_eq!(std::mem::size_of::<VolArgs>(), 4096);
    assert_eq!(std::mem::size_of::<VolArgsV2>(), 4096);
    assert_eq!(std::mem::size_of::<FsInfoArgs>(), 1024);
    assert_eq!(std::mem::offset_of!(VolArgs, name), 8);
    assert_eq!(std::mem::offset_of!(VolArgsV2, fd), 0);
    assert_eq!(std::mem::offset_of!(VolArgsV2, name), 56);
    assert_eq!(std::mem::offset_of!(FsInfoArgs, fsid), 16);
    assert_eq!(std::mem::offset_of!(FsInfoArgs, reserved), 80);
}

/// One anonymous `memfd` whose `/proc/self/fd` link is unique to this journey.
fn memfd(name: &str) -> OwnedFd {
    let name = std::ffi::CString::new(name).expect("no interior NUL");
    // SAFETY: `name` is a live NUL-terminated string; the returned fd is owned exactly once here.
    let raw = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
    assert!(
        raw >= 0,
        "memfd_create: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: `raw` was just created and is owned exactly once.
    unsafe { OwnedFd::from_raw_fd(raw) }
}

/// How many live descriptors point at `memfd:<name>`; a leaked duplicate would make it two.
fn links_to(name: &str) -> usize {
    let needle = format!("memfd:{name}");
    fs::read_dir("/proc/self/fd")
        .expect("read /proc/self/fd")
        .filter_map(Result::ok)
        .filter_map(|entry| fs::read_link(entry.path()).ok())
        .filter(|target| target.to_string_lossy().contains(&needle))
        .count()
}

/// Sends a control message with an arbitrary descriptor count, bypassing [`send_fd`].
fn send_many(socket: &UnixStream, payload: &[u8], fds: &[RawFd]) {
    let mut iov = libc::iovec {
        iov_base: payload.as_ptr().cast_mut().cast(),
        iov_len: payload.len(),
    };
    let declared = std::mem::size_of_val(fds) as libc::c_uint;
    // SAFETY: `CMSG_SPACE` is pure arithmetic over the declared byte length.
    let mut control = vec![0_u8; unsafe { libc::CMSG_SPACE(declared) } as usize];
    // SAFETY: an all-zero `msghdr` is valid; every field is set below.
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &raw mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = control.len() as _;
    // SAFETY: `control` is exactly `CMSG_SPACE(n * sizeof(RawFd))` bytes, so one header and its
    // `n` slots fit; each slot is written exactly once.
    unsafe {
        let header = libc::CMSG_FIRSTHDR(&message);
        assert!(!header.is_null());
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(declared) as _;
        let data = libc::CMSG_DATA(header).cast::<RawFd>();
        for (index, fd) in fds.iter().enumerate() {
            std::ptr::write_unaligned(data.add(index), *fd);
        }
        message.msg_controllen = libc::CMSG_LEN(declared) as _;
    }
    // SAFETY: `message` references initialized buffers for the call's duration.
    let sent = unsafe { libc::sendmsg(socket.as_raw_fd(), &message, 0) };
    assert_eq!(sent, payload.len() as isize);
}

/// A peer that attaches two or four descriptors is refused and every received fd is closed.
#[test]
fn extra_descriptors_are_refused_and_never_leak() {
    for (case, count) in [("two", 2_usize), ("four", 4_usize)] {
        let (sender, receiver) = UnixStream::pair().expect("real socketpair");
        let names: Vec<String> = (0..count)
            .map(|index| format!("slipfork-fdpass-{case}-{index}"))
            .collect();
        let held: Vec<OwnedFd> = names.iter().map(|name| memfd(name)).collect();
        let fds: Vec<RawFd> = held.iter().map(AsRawFd::as_raw_fd).collect();
        send_many(&sender, b"wide", &fds);
        let error = recv_fd(&receiver).expect_err("more than one descriptor must fail closed");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData, "{case}");
        for name in &names {
            assert_eq!(links_to(name), 1, "received descriptor `{name}` must close");
        }
    }
}

/// A foreign namespace identity is a typed mismatch, never silently trusted.
#[test]
fn a_foreign_namespace_identity_is_a_typed_mismatch() {
    let mut value = serde_json::to_value(NamespaceIdentity::observe().expect("read /proc/self/ns"))
        .expect("identity json");
    value["net"] = serde_json::json!("net:[1]");
    let foreign: NamespaceIdentity = serde_json::from_value(value).expect("foreign identity");
    let table = NamespaceTable::from_proc(std::process::id()).expect("table for self");
    assert!(matches!(
        table.verify(&foreign),
        Err(Error::NamespaceMismatch { .. })
    ));
}

/// Live PTY journey: a real pod, a real `/dev/pts/N` master over `SCM_RIGHTS`, a real
/// `TIOCSWINSZ` resize observed by `stty size`, and a capture file that survives pod stop.
///
/// Runs only with `--features live` because it needs `crun`, unprivileged user namespaces, and a
/// tmpfs runtime root; it fails closed with the missing capability otherwise.
#[cfg(feature = "live")]
mod pty_lab {
    use super::*;
    use std::os::fd::AsRawFd;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use slipfork_world::{WorldHost, WorldSupervisor};

    fn require_tool(name: &str) -> PathBuf {
        std::env::var_os("PATH")
            .into_iter()
            .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
            .unwrap_or_else(|| panic!("`{name}` is not on PATH"))
    }

    fn tmpfs_dir(tag: &str) -> PathBuf {
        let dir = PathBuf::from("/dev/shm").join(format!(
            "slipfork-pty-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| since.as_nanos())
        ));
        fs::create_dir_all(&dir).expect("create the tmpfs runtime root");
        let file = std::fs::File::open(&dir).expect("open the runtime root");
        // SAFETY: the descriptor is live and `statfs` is plain-old-data.
        let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
        // SAFETY: the descriptor is live and the kernel writes exactly one `statfs`.
        let ret = unsafe { libc::fstatfs(file.as_raw_fd(), &mut stat) };
        assert_eq!(ret, 0, "fstatfs the runtime root");
        assert_eq!(
            stat.f_type as u64, 0x0102_1994,
            "the runtime root must be tmpfs"
        );
        dir
    }

    /// Copies one host binary and every shared object `ldd` reports into `rootfs`.
    fn copy_bin(rootfs: &Path, binary: &str) {
        let destination = rootfs.join(binary.trim_start_matches('/'));
        fs::create_dir_all(destination.parent().expect("binary parent")).expect("create bin dir");
        fs::copy(binary, &destination).unwrap_or_else(|error| panic!("copy `{binary}`: {error}"));
        let output = std::process::Command::new("ldd")
            .arg(binary)
            .output()
            .unwrap_or_else(|error| panic!("ldd `{binary}`: {error}"));
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let mut fields = line.split_whitespace();
            let mut library = fields.next().unwrap_or_default();
            if library == "linux-vdso.so.1" {
                continue;
            }
            if !library.starts_with('/')
                && let Some(next) = fields.next()
                && next == "=>"
            {
                library = fields.next().unwrap_or_default();
            }
            if !library.starts_with('/') {
                continue;
            }
            let target = rootfs.join(library.trim_start_matches('/'));
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).expect("create library dir");
            }
            fs::copy(library, &target).unwrap_or_else(|error| panic!("copy `{library}`: {error}"));
        }
    }

    fn assemble_rootfs(rootfs: &Path) {
        for dir in ["bin", "usr/bin", "etc", "tmp", "dev", "proc", "sys", "root"] {
            fs::create_dir_all(rootfs.join(dir)).expect("create rootfs dir");
        }
        copy_bin(rootfs, "/bin/sh");
        copy_bin(rootfs, "/usr/bin/tty");
        copy_bin(rootfs, "/usr/bin/stty");
        let sleep = ["/bin/sleep", "/usr/bin/sleep"]
            .into_iter()
            .find(|candidate| Path::new(candidate).is_file())
            .expect("the lab needs a `sleep` binary");
        copy_bin(rootfs, sleep);
    }

    fn nonblocking(fd: RawFd) {
        // SAFETY: `fd` is a live descriptor; F_GETFL/F_SETFL only inspect flags.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        assert!(flags >= 0, "read fd flags");
        // SAFETY: same live descriptor with the flags we just read.
        let ret = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
        assert_eq!(ret, 0, "set O_NONBLOCK on the PTY master");
    }

    fn write_fd(fd: RawFd, bytes: &[u8]) {
        let mut written = 0;
        while written < bytes.len() {
            // SAFETY: `bytes` is live for the call and `fd` is a live writable descriptor.
            let ret =
                unsafe { libc::write(fd, bytes[written..].as_ptr().cast(), bytes.len() - written) };
            assert!(ret > 0, "write to the PTY master");
            written += ret as usize;
        }
    }

    /// Reads the master until `needle` appears or the budget runs out.
    fn read_until(fd: RawFd, needle: &str, budget: Duration) -> String {
        let deadline = Instant::now() + budget;
        let mut seen = String::new();
        let mut chunk = [0_u8; 1024];
        while Instant::now() < deadline {
            let mut pollfd = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one valid `pollfd` lives across the call.
            let ready = unsafe { libc::poll(&mut pollfd, 1, 100) };
            if ready <= 0 {
                continue;
            }
            // SAFETY: `chunk` is a live buffer and `fd` is a live readable descriptor.
            let read = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), chunk.len()) };
            if read <= 0 {
                continue;
            }
            seen.push_str(&String::from_utf8_lossy(&chunk[..read as usize]));
            if seen.contains(needle) {
                return seen;
            }
        }
        panic!("PTY never produced `{needle}`; saw: {seen:?}");
    }

    fn ptsname(fd: RawFd) -> String {
        let mut buffer = [0_i8; 128];
        // SAFETY: `buffer` is a live NUL-terminated target for `ptsname_r`.
        let ret = unsafe { libc::ptsname_r(fd, buffer.as_mut_ptr(), buffer.len()) };
        assert_eq!(ret, 0, "ptsname_r the PTY master");
        // SAFETY: `ptsname_r` wrote a NUL-terminated string into `buffer`.
        unsafe { std::ffi::CStr::from_ptr(buffer.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    }

    fn send(stream: &mut UnixStream, request: &Request) {
        let bytes = slipfork_world::encode(request).expect("encode a control request");
        stream.write_all(&bytes).expect("write a control request");
        stream.flush().expect("flush a control request");
    }

    fn recv_frame(stream: &mut UnixStream, lines: &mut FrameLines) -> slipfork_world::InitFrame {
        loop {
            if let Some(raw) = lines.next_line() {
                let text = std::str::from_utf8(&raw).expect("control frame is UTF-8");
                return slipfork_world::decode_init_frame(text.trim()).expect("decode a frame");
            }
            let mut chunk = [0_u8; 4096];
            let read = stream.read(&mut chunk).expect("read a control frame");
            assert!(read > 0, "the world closed its control socket");
            lines.extend(&chunk[..read]);
        }
    }

    fn connect(pods_dir: &Path) -> UnixStream {
        let stream = UnixStream::connect(control::control_socket_path(pods_dir).unwrap())
            .expect("connect to control.sock");
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .expect("control read timeout");
        stream
    }

    #[test]
    fn a_real_pty_is_handed_off_resized_and_logs_outlive_the_pod() {
        let crun = require_tool("crun");
        let unshare = require_tool("unshare");
        let runtime = tmpfs_dir("rt");
        let store = StoreLayout::new(lab("live-pty"));
        let world = WorldName::parse("ptylab").expect("valid world");
        let rootfs = store.world_pods(&world).join("api");
        fs::create_dir_all(&rootfs).expect("create the pod rootfs");
        assemble_rootfs(&rootfs);

        let uid = unsafe { libc::getuid() };
        let gid = unsafe { libc::getgid() };
        let host = WorldHost::new(
            crun.clone(),
            unshare,
            PathBuf::from(env!("CARGO_BIN_EXE_slipfork-world-init")),
            runtime.clone(),
            uid,
            gid,
            None,
        );
        let supervisor = WorldSupervisor::boot(host, &world).expect("boot a real world");
        let pods_dir = supervisor.pods_dir().to_path_buf();
        let spec = PodSpec {
            id: "api".to_owned(),
            rootfs: rootfs.clone(),
            workdir: "/".to_owned(),
            argv: ["/bin/sh", "-c", "echo booted; while :; do sleep 1; done"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            inherited_env: vec!["PATH=/bin:/usr/bin".to_owned()],
            env: Vec::new(),
            binds: Vec::new(),
            files: Vec::new(),
        };
        write_bundle(&spec, &supervisor.pod_host(), &pods_dir.join("api"))
            .expect("write the OCI bundle");
        control::launch_pod(&pods_dir, &spec, Duration::from_secs(60)).expect("launch the pod");

        let log_path = store.pod_log(&world, "api");
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if fs::read_to_string(&log_path).is_ok_and(|text| text.contains("booted")) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            fs::read_to_string(&log_path).is_ok_and(|text| text.contains("booted")),
            "the launch pump must write `{}`",
            log_path.display()
        );

        let mut pty = connect(&pods_dir);
        send(
            &mut pty,
            &Request::ExecPty {
                v: PROTOCOL_VERSION,
                id: "pty-1".to_owned(),
                pod: "api".to_owned(),
                argv: vec!["/bin/sh".to_owned()],
                cols: 80,
                rows: 24,
            },
        );
        let (payload, master) = recv_fd(&pty).expect("receive the PTY master over SCM_RIGHTS");
        assert_eq!(payload, b"pty");
        let master = master.expect("crun delivered a PTY master");
        let fd = master.as_raw_fd();
        // SAFETY: `fd` is the live PTY master received over SCM_RIGHTS.
        assert_eq!(
            unsafe { libc::isatty(fd) },
            1,
            "the handed-off fd is a terminal"
        );
        let device = ptsname(fd);
        assert!(
            device.starts_with("/dev/pts/"),
            "real pts device, got {device}"
        );
        nonblocking(fd);
        write_fd(fd, b"stty size\n");
        let seen = read_until(fd, "24 80", Duration::from_secs(10));
        assert!(seen.contains("24 80"), "{seen:?}");

        send(
            &mut pty,
            &Request::ExecResize {
                v: PROTOCOL_VERSION,
                id: "pty-1".to_owned(),
                cols: 120,
                rows: 40,
            },
        );
        std::thread::sleep(Duration::from_millis(100));
        write_fd(fd, b"stty size\n");
        let seen = read_until(fd, "40 120", Duration::from_secs(10));
        assert!(
            seen.contains("40 120"),
            "TIOCSWINSZ did not reach the PTY: {seen:?}"
        );

        write_fd(fd, b"exit\n");
        let mut lines = FrameLines::new();
        let reply = recv_frame(&mut pty, &mut lines);
        match reply {
            slipfork_world::InitFrame::Reply { ok, error, .. } => {
                assert!(error.is_none(), "{error:?}");
                assert_eq!(ok.expect("reply payload")["exit_code"], 0);
            }
            other => panic!("expected a final PTY reply, got {other:?}"),
        }
        drop(pty);
        drop(master);

        let mut stop = connect(&pods_dir);
        send(
            &mut stop,
            &Request::StopAll {
                v: PROTOCOL_VERSION,
                id: "stop".to_owned(),
            },
        );
        let mut stop_lines = FrameLines::new();
        let _ = recv_frame(&mut stop, &mut stop_lines);
        drop(stop);
        assert!(
            log_path.is_file(),
            "`down` must never delete `{}`",
            log_path.display()
        );

        let mut logs = connect(&pods_dir);
        send(
            &mut logs,
            &Request::Logs {
                v: PROTOCOL_VERSION,
                id: "logs-1".to_owned(),
                service: Some("api".to_owned()),
                pods: None,
                follow: false,
                since_ms: None,
                until_ms: None,
            },
        );
        let mut log_lines = FrameLines::new();
        let mut texts = Vec::new();
        loop {
            match recv_frame(&mut logs, &mut log_lines) {
                slipfork_world::InitFrame::Output { line, .. } => texts.push(line),
                slipfork_world::InitFrame::Reply { error, .. } => {
                    assert!(error.is_none(), "{error:?}");
                    break;
                }
                _ => {}
            }
        }
        assert!(
            texts.iter().any(|line| line.contains("booted")),
            "the stopped pod replay must come from `{}`: {texts:?}",
            log_path.display()
        );

        drop(supervisor);
        let _ = fs::remove_dir_all(&runtime);
        let _ = fs::remove_dir_all(store.btrfs_mount());
    }

    fn launch_in_thread(
        pods_dir: PathBuf,
        spec: PodSpec,
        barrier: Arc<std::sync::Barrier>,
    ) -> std::thread::JoinHandle<slipfork_world::Result<u32>> {
        std::thread::spawn(move || {
            barrier.wait();
            control::launch_pod(&pods_dir, &spec, Duration::from_secs(60))
        })
    }

    /// Two `Launch` frames must overlap in the control plane, and a racing duplicate of one pod
    /// id must resolve to a single `crun run` whose pid both callers reuse.
    #[test]
    fn concurrent_launches_overlap_and_same_id_coalesces() {
        let crun = require_tool("crun");
        let unshare = require_tool("unshare");
        let runtime = tmpfs_dir("rt-launch");
        let store = StoreLayout::new(lab("live-launch"));
        let world = WorldName::parse("launchlab").expect("valid world");
        let rootfs = store.world_pods(&world).join("api");
        fs::create_dir_all(&rootfs).expect("create the pod rootfs");
        assemble_rootfs(&rootfs);

        let uid = unsafe { libc::getuid() };
        let gid = unsafe { libc::getgid() };
        let host = WorldHost::new(
            crun,
            unshare,
            PathBuf::from(env!("CARGO_BIN_EXE_slipfork-world-init")),
            runtime.clone(),
            uid,
            gid,
            None,
        );
        let supervisor = WorldSupervisor::boot(host, &world).expect("boot a real world");
        let pods_dir = supervisor.pods_dir().to_path_buf();
        let spec = |id: &str| PodSpec {
            id: id.to_owned(),
            rootfs: rootfs.clone(),
            workdir: "/".to_owned(),
            argv: ["/bin/sh", "-c", "echo booted; while :; do sleep 1; done"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            inherited_env: vec!["PATH=/bin:/usr/bin".to_owned()],
            env: Vec::new(),
            binds: Vec::new(),
            files: Vec::new(),
        };
        for id in ["api", "worker", "dup"] {
            write_bundle(&spec(id), &supervisor.pod_host(), &pods_dir.join(id))
                .expect("write the OCI bundle");
        }

        let barrier = Arc::new(std::sync::Barrier::new(2));
        let api = launch_in_thread(pods_dir.clone(), spec("api"), Arc::clone(&barrier));
        let worker = launch_in_thread(pods_dir.clone(), spec("worker"), Arc::clone(&barrier));
        let api_pid = api.join().expect("api thread").expect("api launch");
        let worker_pid = worker
            .join()
            .expect("worker thread")
            .expect("worker launch");
        assert_ne!(api_pid, worker_pid, "two ids must be separate pod pids");

        let barrier = Arc::new(std::sync::Barrier::new(2));
        let first = launch_in_thread(pods_dir.clone(), spec("dup"), Arc::clone(&barrier));
        let second = launch_in_thread(pods_dir.clone(), spec("dup"), Arc::clone(&barrier));
        let first_pid = first.join().expect("first dup thread").expect("first dup");
        let second_pid = second
            .join()
            .expect("second dup thread")
            .expect("second dup");
        assert_eq!(
            first_pid, second_pid,
            "a racing duplicate Launch of one id must coalesce onto one crun run"
        );

        let statuses = control::list_pods(&pods_dir).expect("list the world's pods");
        for id in ["api", "worker", "dup"] {
            assert!(
                statuses
                    .iter()
                    .any(|status| status.id == id && status.running),
                "`{id}` must be running after the concurrent launches: {statuses:?}"
            );
        }
        for id in ["api", "worker", "dup"] {
            let path = store.pod_log(&world, id);
            let deadline = Instant::now() + Duration::from_secs(10);
            let booted = || fs::read_to_string(&path).is_ok_and(|text| text.contains("booted"));
            while Instant::now() < deadline && !booted() {
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(booted(), "`{id}` never became ready: {}", path.display());
        }

        drop(supervisor);
        let _ = fs::remove_dir_all(&runtime);
        let _ = fs::remove_dir_all(store.btrfs_mount());
    }
}
