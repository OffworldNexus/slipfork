//! Protected conformance journey for the OSPP v1 wire (`specs/05-ospp-v1.md`).
//!
//! One real end-to-end lifecycle drives the production client against the in-tree
//! `127.0.0.1:0` reference server: fragment pairing that persists its own
//! `ospp.json`, the `0600` device identity and offline vault, the exact
//! `subscribe` payload, a live `env_updated` observed by a real child process, a
//! multi-MB sealed dump whose digest matches, and fail-closed dump transports.

mod support;

use std::collections::BTreeMap;
use std::io::{Cursor, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use slipfork_ospp::{
    DeviceKeySigner, DumpReader, FileDeviceKeyStore, FileSecretVault, OsppClaims, OsppClient,
    OsppError, OsppTimeouts, PUBLIC_KEY_HEX_LEN, PairPhase, PairingStore, Secret, SecretVault,
    SoftwareP256Key, SoftwareP256Signer, SubscribeRequest, SubscriptionEnv, SubscriptionKey,
    WsOsppClient, open_dump_stream,
};
use support::server::{ReferenceOsppServer, ReferenceServerConfig, TestValue, seal_dump};

const FORKFILE: &str = r#"
postgres pg {
    database main
}

env app {
    LOG_LEVEL     = "info"
    DEBUG         = var(DEBUG) ?? "local"
    LOCAL_DEV_KEY = secret(LOCAL_DEV_KEY)
    SECRET_KEY    = secret(SECRET_KEY)
}

service api ./api {
    use env app
    run /bin/true
}
"#;

const LOCAL_SECRET: &str = "s3cr3t-value";
const LOCAL_ONLY: &str = "local-only-value";
const ROTATED_SECRET: &str = "rotated-value";

fn timeouts() -> OsppTimeouts {
    OsppTimeouts {
        handshake: Duration::from_secs(3),
        io: Duration::from_secs(5),
        pairing_approval: Duration::from_secs(5),
    }
}

fn claims() -> OsppClaims {
    OsppClaims {
        vars: vec!["DEBUG".to_owned()],
        secrets: vec!["LOCAL_DEV_KEY".to_owned()],
        resources: BTreeMap::from([("pg_databases".to_owned(), vec!["main".to_owned()])]),
        ..OsppClaims::default()
    }
}

fn mode_of(path: &std::path::Path) -> u32 {
    std::fs::metadata(path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777
}

fn poll_until(mut predicate: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if predicate() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    predicate()
}

fn pseudo_random(len: usize) -> Vec<u8> {
    let mut state = 0x1234_5678_9abc_def0u64;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

/// Serve one raw HTTP/1.1 response on loopback and return its dump URL.
fn serve_http_once(response: Vec<u8>) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept the dump GET");
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request);
        let _ = stream.write_all(&response);
        let _ = stream.flush();
    });
    format!("http://{addr}/dumps/token")
}

fn assert_typed_dump_failure(error: OsppError) {
    assert!(
        matches!(
            error,
            OsppError::Transport { .. } | OsppError::DumpHttp { .. }
        ),
        "unexpected error variant: {error:?}"
    );
    assert!(
        !error.to_string().contains("token"),
        "dump error leaked the bearer token: {error}"
    );
}

#[test]
fn ospp_v1_lifecycle_pairs_subscribes_streams_and_vaults() {
    let root = tempfile::TempDir::new_in("/var/tmp").expect("disk state dir");
    let state_dir = root.path().join("state");
    std::fs::create_dir_all(&state_dir).expect("create state dir");
    let pairing_path = state_dir.join("ospp.json");

    let signer = SoftwareP256Signer::new(
        SoftwareP256Key::from_scalar_bytes(&[0x11; 32]).expect("fixed machine key"),
    );
    let public_key = signer.public_key_hex().to_owned();
    assert_eq!(public_key.len(), PUBLIC_KEY_HEX_LEN);

    let payload = pseudo_random(3 * 1024 * 1024);
    let server = ReferenceOsppServer::start(ReferenceServerConfig {
        accepted_keys: vec![public_key.clone()],
        components: BTreeMap::from([(
            "api".to_owned(),
            BTreeMap::from([
                ("LOG_LEVEL".to_owned(), TestValue::Plain("info".to_owned())),
                (
                    "DEBUG".to_owned(),
                    TestValue::Plain("server-debug".to_owned()),
                ),
                (
                    "SECRET_KEY".to_owned(),
                    TestValue::Secret(LOCAL_SECRET.to_owned()),
                ),
                (
                    "LOCAL_DEV_KEY".to_owned(),
                    TestValue::Secret(LOCAL_ONLY.to_owned()),
                ),
            ]),
        )]),
        dumps: BTreeMap::from([("pg_databases.main".to_owned(), payload.clone())]),
    });
    let client = WsOsppClient::new(
        server.endpoint(),
        Arc::new(signer),
        Some(pairing_path.clone()),
        timeouts(),
    );

    // 1. Pairing reports phases, uses a fragment-only handle, and persists itself.
    let mut phases = Vec::new();
    let mut handle_seen = None;
    let approval = client
        .pair("dev-laptop", &mut |phase| phases.push(phase), &mut |url| {
            let (prefix, handle) = url
                .as_str()
                .split_once("#handle=")
                .expect("fragment-only handle");
            assert!(prefix.starts_with("http://127.0.0.1:"));
            handle_seen = Some(handle.to_owned());
            server.approve_pairing(handle);
            Ok(())
        })
        .expect("pair_approved");
    assert_eq!(
        phases,
        vec![
            PairPhase::SolvingAltcha,
            PairPhase::AwaitingApproval,
            PairPhase::Approved
        ]
    );
    assert_eq!(approval.account, "alice");
    assert_eq!(approval.machine, "dev-laptop");
    assert_eq!(approval.public_key, public_key);
    let handle_seen = handle_seen.expect("handle delivered");
    let pairing = server.state();
    assert_eq!(pairing.pairings.len(), 1);
    assert_eq!(pairing.pairings[0].hostname, "dev-laptop");
    assert_eq!(pairing.pairings[0].handle, handle_seen);
    assert!(pairing.pairings[0].approved);

    // The client itself wrote `<state_dir>/ospp.json`: 0600, exactly three keys, no secrets.
    let record = std::fs::read_to_string(&pairing_path).expect("client wrote ospp.json");
    let parsed: serde_json::Value = serde_json::from_str(&record).expect("ospp.json is JSON");
    let mut keys: Vec<&str> = parsed
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, vec!["account", "endpoint", "machine"]);
    for secret in [
        LOCAL_SECRET,
        LOCAL_ONLY,
        ROTATED_SECRET,
        handle_seen.as_str(),
    ] {
        assert!(!record.contains(secret), "ospp.json leaked {secret}");
    }
    assert_eq!(mode_of(&pairing_path), 0o600);
    assert_eq!(
        PairingStore::at(pairing_path.clone())
            .load()
            .expect("load")
            .expect("present")
            .machine,
        "dev-laptop"
    );

    // 2. The machine identity is one `0600` 32-byte scalar.
    let key_store = FileDeviceKeyStore::new(state_dir.join("device-key-software.json"));
    let machine_key = key_store.load_or_create().expect("device key");
    assert_eq!(machine_key.public_key_hex().len(), PUBLIC_KEY_HEX_LEN);
    assert_eq!(mode_of(key_store.path()), 0o600);

    // 3. Subscribe: the exact payload, claims left local, secrets opened in memory.
    let observed = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&observed);
    let session = client
        .connect(Box::new(move |environment: SubscriptionEnv| {
            if let Some(secret) = environment
                .component("api")
                .and_then(|api| api.get("SECRET_KEY"))
            {
                let child = std::process::Command::new("/bin/sh")
                    .arg("-c")
                    .arg("printf %s \"$SECRET_KEY\"")
                    .env_clear()
                    .env("SECRET_KEY", secret.expose())
                    .output()
                    .expect("child process reads its environment");
                sink.lock()
                    .expect("observed lock")
                    .push(String::from_utf8(child.stdout).expect("utf8 child output"));
            }
        }))
        .expect("hello -> auth_ok");
    let subscribed = session
        .subscribe(&SubscribeRequest {
            id: "sub-1".to_owned(),
            remote: "git@github.com:acme/web.git".to_owned(),
            forkfile: FORKFILE.to_owned(),
            claims: claims(),
        })
        .expect("subscribe -> env_resolved");
    let api = subscribed.component("api").expect("api component");
    assert_eq!(api["LOG_LEVEL"].expose(), "info");
    assert_eq!(api["SECRET_KEY"].expose(), LOCAL_SECRET);
    assert!(!api.contains_key("DEBUG"), "claimed var stays local");
    assert!(
        !api.contains_key("LOCAL_DEV_KEY"),
        "claimed secret stays local"
    );
    assert!(!format!("{subscribed:?}").contains(LOCAL_SECRET));

    let state = server.state();
    let authenticated = state.requests.last().expect("authenticated handshake");
    assert_eq!(authenticated.path, "/_ospp/v1");
    assert_eq!(authenticated.subprotocol.as_deref(), Some("ospp-v1"));
    let recorded = &state.subscriptions["sub-1"];
    assert_eq!(recorded.id, "sub-1");
    assert_eq!(recorded.remote, "git@github.com:acme/web.git");
    assert_eq!(recorded.forkfile, FORKFILE);
    assert_eq!(recorded.claims, claims());
    assert_eq!(recorded.public_key.len(), 130);
    assert_eq!(&recorded.public_key[..2], "04");

    // 4. A live rotation reaches a real child process before any restart.
    server.set_secret("SECRET_KEY", ROTATED_SECRET);
    server.push_update().expect("push env_updated");
    assert!(
        poll_until(|| !observed.lock().expect("observed lock").is_empty()),
        "env_updated was never delivered"
    );
    let child_view = observed
        .lock()
        .expect("observed lock")
        .pop()
        .expect("one child observation");
    assert_eq!(child_view, ROTATED_SECRET);

    // 5. The sealed dump streams through the caller-opened ticket intact.
    let ticket = session
        .dump("sub-1", "pg_databases.main")
        .expect("dump_ready");
    let mut restored = Vec::new();
    ticket
        .open(Duration::from_secs(5))
        .expect("open sealed dump")
        .read_to_end(&mut restored)
        .expect("sealed dump streams");
    assert_eq!(restored.len(), payload.len());
    assert_eq!(Sha256::digest(&restored), Sha256::digest(&payload));
    assert_eq!(server.state().dump_urls, 1);

    // 6. An unknown resource is a typed `dump_unavailable` refusal.
    let unavailable = session
        .dump("sub-1", "redis_clusters.missing")
        .expect_err("no dump for that resource");
    assert_eq!(unavailable.code(), Some("dump_unavailable"));

    // 7. The offline vault is `0600` and refuses a group-readable file.
    let vault = FileSecretVault::new(state_dir.join("secrets.json"));
    vault
        .store("api", "SECRET_KEY", &Secret::new(&child_view))
        .expect("vault store");
    assert_eq!(mode_of(vault.path()), 0o600);
    assert_eq!(
        vault
            .load("api", "SECRET_KEY")
            .expect("vault load")
            .expect("present")
            .expose(),
        ROTATED_SECRET
    );
    std::fs::set_permissions(vault.path(), std::fs::Permissions::from_mode(0o644))
        .expect("chmod 0644");
    let error = vault.load("api", "SECRET_KEY").expect_err("refused");
    assert!(error.to_string().contains("0600"), "{error}");

    // 8. Bad dump transports fail closed with typed errors and no bearer token.
    assert_typed_dump_failure(
        open_dump_stream(
            &serve_http_once(b"HTTP/1.1 200 OK\r\nconnection: close\r\n\r\nbody".to_vec()),
            Duration::from_secs(2),
        )
        .err()
        .expect("missing content-length"),
    );
    assert_typed_dump_failure(
        open_dump_stream(
            &serve_http_once(b"HTTP/1.1 200 OK\r\ncontent-length: nope\r\n\r\nbody".to_vec()),
            Duration::from_secs(2),
        )
        .err()
        .expect("non-numeric content-length"),
    );
    let oversized = format!(
        "HTTP/1.1 200 OK\r\ncontent-length: 1\r\nx-pad: {}\r\n\r\nx",
        "a".repeat(17 * 1024)
    );
    assert_typed_dump_failure(
        open_dump_stream(
            &serve_http_once(oversized.into_bytes()),
            Duration::from_secs(2),
        )
        .err()
        .expect("oversized headers"),
    );
    let refusal = open_dump_stream(
        &serve_http_once(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n".to_vec()),
        Duration::from_secs(2),
    )
    .err()
    .expect("HTTP 404");
    assert!(matches!(refusal, OsppError::DumpHttp { status: 404 }));

    // 9. Truncation and AAD swap both fail closed on the same real sealing.
    let pinned = SubscriptionKey::from_scalar_bytes(&[0x44; 32]).expect("pinned key");
    let mut sealed = seal_dump(
        pinned.public_key_hex(),
        "sub-1",
        "pg_databases.main",
        &payload[..64 * 1024],
    )
    .expect("seal dump");
    sealed.truncate(sealed.len() - 20);
    let mut truncated =
        DumpReader::open(Cursor::new(sealed), &pinned, "sub-1", "pg_databases.main")
            .expect("header opens");
    let error = truncated
        .read_to_end(&mut Vec::new())
        .expect_err("missing EOF frame fails closed");
    assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);

    // 10. Unsubscribe and reap the session; the server observes the release.
    session.unsubscribe("sub-1").expect("unsubscribe");
    assert!(
        poll_until(|| server.state().subscriptions.is_empty()),
        "server kept the subscription"
    );
    session.close().expect("close");
    server.shutdown();

    // 11. An unapproved pairing closes its window with an actionable error.
    let signer = SoftwareP256Signer::new(
        SoftwareP256Key::from_scalar_bytes(&[0x11; 32]).expect("fixed machine key"),
    );
    let public_key = signer.public_key_hex().to_owned();
    let idle = ReferenceOsppServer::start(ReferenceServerConfig {
        accepted_keys: vec![public_key],
        ..ReferenceServerConfig::default()
    });
    let impatient = WsOsppClient::new(
        idle.endpoint(),
        Arc::new(signer),
        None,
        OsppTimeouts {
            pairing_approval: Duration::from_millis(200),
            ..timeouts()
        },
    );
    let error = impatient
        .pair("dev-laptop", &mut |_| {}, &mut |_| Ok(()))
        .expect_err("the pairing window closed");
    assert!(
        error.to_string().contains("pairing window closed"),
        "{error}"
    );
    assert!(error.to_string().contains("next:"), "{error}");
    idle.shutdown();
}
