//! Protected conformance suite for the OSPP v1 wire (`docs/design/ospp.md`).
//!
//! Every test drives the production client path against the in-tree reference
//! server: real loopback WebSocket frames, real P-256 verification, real RFC 9180
//! HPKE sealing, single-pass `snow.yml` resolution against ID-only claims, live
//! `env_updated` pushes, and the out-of-band sealed dump stream.

use std::io::Read;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use slipfork_ospp::{
    DeviceKeySigner, DumpReader, OsppClaims, OsppClient, OsppTimeouts, SubscribeRequest,
    SubscriptionEnv, SubscriptionKey, WsOsppClient,
};
use slipfork_ospp_testkit::{
    FixedEntropy, MisSigningSigner, PairOutcome, ReferenceOsppServer, ReferenceServerConfig,
    ResourceRecord, seal_dump, seal_secret, software_signer,
};

const SNOW: &str = r#"
images: []
components: []
modes:
  all:
    envs:
      api:
        LOG_LEVEL: "info"
        DEBUG: '  ${{ vars::DEBUG ?? "local" }}  '
        LOCAL_DEV_KEY: "${{ secrets::LOCAL_DEV_KEY }}"
        DATABASE_URL: "${{ resources::pg_databases.main.url }}"
        REPORTING_URL: "${{ resources::pg_databases.reporting.url }}"
        SECRET_KEY: "${{ secrets::SECRET_KEY }}"
"#;

fn timeouts() -> OsppTimeouts {
    OsppTimeouts {
        handshake: Duration::from_secs(3),
        io: Duration::from_secs(5),
    }
}

fn config(signer_public_key: &str) -> ReferenceServerConfig {
    ReferenceServerConfig {
        accepted_keys: vec![signer_public_key.to_owned()],
        vars: [("DEBUG".to_owned(), "server".to_owned())]
            .into_iter()
            .collect(),
        secrets: [("SECRET_KEY".to_owned(), "s3cr3t-value".to_owned())]
            .into_iter()
            .collect(),
        resources: [
            (
                "pg_databases.main".to_owned(),
                ResourceRecord {
                    url: "postgres://api@127.0.0.1/main".to_owned(),
                    password: "pg-password".to_owned(),
                },
            ),
            (
                "pg_databases.reporting".to_owned(),
                ResourceRecord {
                    url: "postgres://reporting@127.0.0.1/reporting".to_owned(),
                    password: "reporting-password".to_owned(),
                },
            ),
        ]
        .into_iter()
        .collect(),
        ..ReferenceServerConfig::default()
    }
}

fn claims() -> OsppClaims {
    OsppClaims {
        vars: vec!["DEBUG".to_owned()],
        secrets: vec!["LOCAL_DEV_KEY".to_owned()],
        resources: [("pg_databases".to_owned(), vec!["main".to_owned()])]
            .into_iter()
            .collect(),
        ..OsppClaims::default()
    }
}

fn start_server(
    signing_key: &str,
    extra: impl FnOnce(&mut ReferenceServerConfig),
) -> ReferenceOsppServer {
    let mut config = config(signing_key);
    extra(&mut config);
    ReferenceOsppServer::start(config)
}

fn client_for(
    server: &ReferenceOsppServer,
    signer: slipfork_ospp::SoftwareP256Signer,
) -> WsOsppClient {
    WsOsppClient::new(
        server.endpoint(),
        Arc::new(signer),
        Arc::new(FixedEntropy::new(0x22)),
        timeouts(),
    )
}

fn subscribe_request() -> SubscribeRequest {
    SubscribeRequest {
        id: "sub-1".to_owned(),
        remote: "git@github.com:acme/web.git".to_owned(),
        snow: SNOW.to_owned(),
        claims: claims(),
    }
}

fn poll_until(mut predicate: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if predicate() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    predicate()
}

#[test]
fn handshake_and_subscribe_resolve_id_only_claims() {
    let signer = software_signer();
    let server = start_server(signer.public_key_hex(), |_| {});
    let client = client_for(&server, software_signer());
    let session = client.connect(Box::new(|_| {})).expect("hello -> auth_ok");

    let environment = session
        .subscribe(&subscribe_request())
        .expect("subscribe -> env_resolved");
    let api = environment.component("api").expect("api component");

    assert_eq!(api["LOG_LEVEL"].expose(), "info");
    // Claimed templates survive byte-verbatim, including the surrounding spaces.
    assert_eq!(api["DEBUG"].expose(), "  ${{ vars::DEBUG ?? \"local\" }}  ");
    assert_eq!(
        api["LOCAL_DEV_KEY"].expose(),
        "${{ secrets::LOCAL_DEV_KEY }}"
    );
    assert!(!api["LOCAL_DEV_KEY"].is_secret());
    // A claimed resource keeps its expression verbatim (the uniform claim rule);
    // an unclaimed resource coordinate resolves to plaintext.
    assert_eq!(
        api["DATABASE_URL"].expose(),
        "${{ resources::pg_databases.main.url }}"
    );
    assert_eq!(
        api["REPORTING_URL"].expose(),
        "postgres://reporting@127.0.0.1/reporting"
    );
    assert!(api["SECRET_KEY"].is_secret());
    assert_eq!(api["SECRET_KEY"].expose(), "s3cr3t-value");
    assert!(!format!("{environment:?}").contains("s3cr3t-value"));

    let state = server.state();
    assert_eq!(state.requests.len(), 1);
    assert_eq!(state.requests[0].subprotocol.as_deref(), Some("ospp-v1"));
    assert_eq!(state.frames, vec!["hello", "auth_response", "subscribe"]);
    let recorded = &state.subscriptions["sub-1"];
    assert_eq!(recorded.remote, "git@github.com:acme/web.git");
    assert_eq!(recorded.claims, claims());
    // Only identifiers cross the wire, never server values.
    let claims_json = serde_json::to_string(&recorded.claims).expect("claims json");
    assert!(!claims_json.contains("s3cr3t-value"));
    assert!(!claims_json.contains("pg-password"));
}

#[test]
fn pushed_env_updated_is_decrypted_in_memory_and_delivered() {
    let signer = software_signer();
    let server = start_server(signer.public_key_hex(), |_| {});
    let client = client_for(&server, software_signer());

    let updates: Arc<Mutex<Vec<SubscriptionEnv>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&updates);
    let session = client
        .connect(Box::new(move |environment| {
            sink.lock().expect("updates lock").push(environment);
        }))
        .expect("authenticated session");
    session
        .subscribe(&subscribe_request())
        .expect("subscribe -> env_resolved");

    server.set_secret("SECRET_KEY", "rotated-value");
    server.push_update().expect("push env_updated");
    assert!(
        poll_until(|| !updates.lock().expect("updates lock").is_empty()),
        "env_updated was never delivered"
    );

    let update = updates
        .lock()
        .expect("updates lock")
        .pop()
        .expect("one update");
    assert_eq!(update.id, "sub-1");
    let api = update.component("api").expect("api component");
    assert_eq!(api["SECRET_KEY"].expose(), "rotated-value");
    assert!(api["SECRET_KEY"].is_secret());
    assert!(!format!("{update:?}").contains("rotated-value"));
}

#[test]
fn unsubscribe_releases_the_subscription() {
    let signer = software_signer();
    let server = start_server(signer.public_key_hex(), |_| {});
    let client = client_for(&server, software_signer());
    let session = client
        .connect(Box::new(|_| {}))
        .expect("authenticated session");
    session.subscribe(&subscribe_request()).expect("subscribed");
    assert!(server.state().subscriptions.contains_key("sub-1"));

    session.unsubscribe("sub-1").expect("unsubscribe");
    assert!(
        poll_until(|| server.state().subscriptions.is_empty()),
        "server kept the subscription"
    );
    assert!(
        server
            .state()
            .frames
            .iter()
            .any(|frame| frame == "unsubscribe")
    );
}

#[test]
fn pairing_appends_the_server_url_fragment_and_reaches_pair_approved() {
    let signer = software_signer();
    let server = start_server(signer.public_key_hex(), |config| {
        config.auto_approve = false;
    });
    let client = client_for(&server, software_signer());

    let mut handle = None;
    let server_ref = &server;
    let approval = client
        .pair("dev-laptop", &mut |url| {
            let text = url.as_str();
            let (prefix, fragment) = text.split_once("#handle=").expect("fragment handle");
            assert!(prefix.starts_with("http://127.0.0.1:"));
            assert!(!prefix.contains("#handle="));
            handle = Some(fragment.to_owned());
            server_ref.approve_pairing(fragment);
            Ok(())
        })
        .expect("pair_approved");

    assert_eq!(approval.account, "alice");
    assert_eq!(approval.machine, "dev-laptop");
    let state = server.state();
    assert_eq!(state.pairings.len(), 1);
    assert_eq!(state.pairings[0].hostname, "dev-laptop");
    assert_eq!(Some(state.pairings[0].handle.clone()), handle);
    assert!(state.pairings[0].approved);
}

#[test]
fn pairing_rejection_is_a_typed_error_frame() {
    let signer = software_signer();
    let server = start_server(signer.public_key_hex(), |config| {
        config.pair_outcome = PairOutcome::Reject {
            code: "rejected".to_owned(),
        };
    });
    let client = client_for(&server, software_signer());
    let error = client
        .pair("dev-laptop", &mut |_| Ok(()))
        .expect_err("rejected pairing");
    assert_eq!(error.code(), Some("rejected"));
}

#[test]
fn bad_signature_fails_closed_with_bad_proof() {
    let signer = software_signer();
    let server = start_server(signer.public_key_hex(), |_| {});
    let client = WsOsppClient::new(
        server.endpoint(),
        Arc::new(MisSigningSigner::new(software_signer())),
        Arc::new(FixedEntropy::new(0x22)),
        timeouts(),
    );
    let error = match client.connect(Box::new(|_| {})) {
        Ok(_) => panic!("all-zero signature must not authenticate"),
        Err(error) => error,
    };
    assert_eq!(error.code(), Some("bad_proof"));
}

#[test]
fn hpke_round_trip_pinning_rejects_a_wrong_aad() {
    let key = SubscriptionKey::from_scalar_bytes(&[0x11; 32]).expect("fixed key");
    let sealed = seal_secret(
        key.public_key_hex(),
        "api",
        "SECRET_KEY",
        "pinned-plaintext",
        &FixedEntropy::new(0x33),
    )
    .expect("seal");
    assert_eq!(
        key.open_secret("api", "SECRET_KEY", &sealed)
            .expect("right aad opens"),
        "pinned-plaintext"
    );
    assert!(key.open_secret("api", "OTHER_KEY", &sealed).is_err());
    assert!(key.open_secret("worker", "SECRET_KEY", &sealed).is_err());
    let other = SubscriptionKey::from_scalar_bytes(&[0x12; 32]).expect("other key");
    assert!(other.open_secret("api", "SECRET_KEY", &sealed).is_err());
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

#[test]
fn dump_stream_restores_and_a_truncated_tail_fails_closed() {
    let payload = pseudo_random(150_000);
    let dump_bytes = payload.clone();
    let signer = software_signer();
    let server = start_server(signer.public_key_hex(), move |config| {
        config.dumps = [("pg_databases.main".to_owned(), dump_bytes)]
            .into_iter()
            .collect();
    });
    let client = client_for(&server, software_signer());
    let session = client
        .connect(Box::new(|_| {}))
        .expect("authenticated session");
    session.subscribe(&subscribe_request()).expect("subscribed");

    let mut restore = Vec::new();
    session
        .dump("sub-1", "pg_databases.main")
        .expect("dump_ready")
        .read_to_end(&mut restore)
        .expect("sealed dump restores");
    assert_eq!(restore, payload);
    assert_eq!(server.state().dump_urls, 1);

    // Truncation proof against the same keying, with only the EOF frame removed.
    let key = SubscriptionKey::from_scalar_bytes(&[0x44; 32]).expect("fixed key");
    let mut sealed = seal_dump(
        key.public_key_hex(),
        "sub-1",
        "pg_databases.main",
        &payload,
        &sealed_rng(),
    )
    .expect("seal dump");
    sealed.truncate(sealed.len() - (4 + 16));
    let mut reader = DumpReader::open(
        std::io::Cursor::new(sealed),
        &key,
        "sub-1",
        "pg_databases.main",
    )
    .expect("header opens");
    let error = reader
        .read_to_end(&mut Vec::new())
        .expect_err("missing EOF fails");
    assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);

    // An intact stream with a swapped resource aad also fails closed.
    let sealed = seal_dump(
        key.public_key_hex(),
        "sub-1",
        "pg_databases.main",
        &payload,
        &sealed_rng(),
    )
    .expect("seal dump");
    let mut reader = DumpReader::open(
        std::io::Cursor::new(sealed),
        &key,
        "sub-1",
        "s3_buckets.media",
    )
    .expect("header opens");
    assert!(reader.read_to_end(&mut Vec::new()).is_err());
}

fn sealed_rng() -> slipfork_ospp_testkit::SeededEntropy {
    slipfork_ospp_testkit::SeededEntropy::new(0xfeed_beef)
}
