//! Protected conformance suite for the OSPP v1 client, capabilities, streaming seed dumps,
//! device-key signers, and pairing.
//!
//! Every test drives the production code path. The WebSocket server, the seeded CSPRNG, and the
//! fake `tpmd` daemon are dev-only doubles in the unpublished `slipfork-ospp-testkit` crate; the
//! signatures, transcripts, frames, compare-and-set secret stores, and zstd archives are real.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use slipfork_dag::{
    DagError, DumpPolicy, PgSeed, ResourceMode, ResourcePlan, ResourceTarget, ResourcesSpec,
    SlipforkConfig,
};
use slipfork_ospp::{
    DeviceKeySigner, DeviceSignature, DiskSeedDumpStore, DumpStreamer, FallbackDeviceKeySigner,
    FileDeviceKeyStore, KeyStorePaths, OsppClient, OsppError, OsppSecretProvider, OsppTimeouts,
    PairingRejectReason, PairingScheme, PairingUrl, RejectReason, SeedDumpStore,
    SoftwareP256Signer, TpmdDeviceKeySigner, connect_transcript, pairing_transcript,
    redeem_pairing_url, verify_device_signature,
};
use slipfork_ospp_testkit::{
    DumpRecord, FakeTpmd, FixedEntropy, HandleRecord, MemorySeedDumpStore, MisSigningSigner,
    ReferenceOsppServer, ReferenceServerConfig, S3BucketRecord, SeededEntropy, TpmdBehavior,
    known_software_key, software_signer,
};
use slipfork_snow::{
    FileVaultProvider, LayeredSecretProvider, Namespace, ProviderError, SecretProvider,
    conformance::assert_secret_provider_conformance,
};
use slipfork_world::ProjectSlug;
use tpmd_proto::RejectCode;

fn timeouts(handshake: Duration) -> OsppTimeouts {
    OsppTimeouts {
        connect: Duration::from_secs(2),
        handshake,
        io: Duration::from_secs(2),
    }
}

fn live_timeouts() -> OsppTimeouts {
    timeouts(Duration::from_secs(2))
}

fn slug() -> ProjectSlug {
    ProjectSlug::parse("acme").expect("valid project slug")
}

/// `OsppClient` deliberately has no `Debug`, so surface a connect failure without `expect_err`.
fn connect_error(
    server: &ReferenceOsppServer,
    signer: &dyn DeviceKeySigner,
    entropy: &dyn slipfork_snow::EntropySource,
    timeouts: OsppTimeouts,
    context: &str,
) -> OsppError {
    match OsppClient::connect(&server.endpoint(), signer, entropy, timeouts) {
        Ok(_) => panic!("{context}"),
        Err(error) => error,
    }
}

fn connect(server: &ReferenceOsppServer, signer: &SoftwareP256Signer) -> OsppClient {
    OsppClient::connect(
        &server.endpoint(),
        signer,
        &FixedEntropy::new(0x11),
        live_timeouts(),
    )
    .expect("CHALLENGE -> HELLO -> WELCOME")
}

/// Start a reference server whose accepted key is `signer`'s.
fn start_server(signer: &SoftwareP256Signer, config: ReferenceServerConfig) -> ReferenceOsppServer {
    let mut config = config;
    config.keys = vec![signer.key_id().clone()];
    ReferenceOsppServer::start(config)
}

/// A scratch directory on the workspace's disk-backed `target/tmp`, never `/tmp` `tmpfs`.
fn disk_scratch() -> tempfile::TempDir {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("ospp-scratch");
    fs::create_dir_all(&root).expect("workspace scratch root");
    tempfile::Builder::new()
        .prefix("ospp-")
        .tempdir_in(root)
        .expect("disk scratch dir")
}

/// Deterministic, essentially incompressible bytes, so a dump spans several 4 KiB frames.
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
fn handshake_golden_transcript_and_live_welcome() {
    let nonce_s = [0x11u8; 32];
    let nonce_c = [0x22u8; 32];
    let transcript = connect_transcript(&nonce_s, &nonce_c, "ospp.example");

    // `ADR-0008` §1: "ospp-v1" || nonce_s(32) || nonce_c(32) || server_name, byte for byte.
    let mut golden = Vec::new();
    golden.extend_from_slice(b"ospp-v1");
    golden.extend_from_slice(&nonce_s);
    golden.extend_from_slice(&nonce_c);
    golden.extend_from_slice(b"ospp.example");
    assert_eq!(transcript, golden);
    assert_eq!(transcript.len(), 7 + 32 + 32 + b"ospp.example".len());

    let signer = software_signer();
    let key_id = signer.key_id().clone();
    let server = start_server(&signer, ReferenceServerConfig::default());
    let client = OsppClient::connect(
        &server.endpoint(),
        &signer,
        &FixedEntropy::new(0x22),
        live_timeouts(),
    )
    .expect("CHALLENGE -> HELLO -> WELCOME");

    assert_eq!(client.key_id(), &key_id);
    assert_eq!(client.backend_label(), "software:file-0600");
    assert_eq!(client.server_name(), "127.0.0.1");

    let state = server.state();
    assert_eq!(state.requests.len(), 1);
    assert_eq!(state.requests[0].path, "/_ospp/v1/connect");
    assert_eq!(state.requests[0].subprotocol.as_deref(), Some("ospp-v1"));
    assert_eq!(state.attempts.len(), 1);
    assert_eq!(state.attempts[0].kind, "hello");
    assert_eq!(state.attempts[0].version, 1);
    assert_eq!(state.attempts[0].key_id, key_id.as_str());
    // FixedEntropy(0x22) makes `nonce_c` exactly 32 0x22 bytes on the wire.
    assert_eq!(state.attempts[0].nonce_c, "22".repeat(32));
}

#[test]
fn pairing_transcript_is_golden_bytes() {
    let nonce_s = [0x01u8; 32];
    let nonce_c = [0x02u8; 32];
    let handle = [0x03u8; 32];
    let transcript = pairing_transcript(&nonce_s, &nonce_c, &handle, "osp.example");

    // `ADR-0008` §1: "ospp-v1-pair" || nonce_s || nonce_c || handle(32) || server_name.
    let mut golden = Vec::new();
    golden.extend_from_slice(b"ospp-v1-pair");
    golden.extend_from_slice(&nonce_s);
    golden.extend_from_slice(&nonce_c);
    golden.extend_from_slice(&handle);
    golden.extend_from_slice(b"osp.example");
    assert_eq!(transcript, golden);
    assert_eq!(transcript.len(), 12 + 32 + 32 + 32 + b"osp.example".len());
}

#[test]
fn signer_probe_software_fallback_and_redaction() {
    let signer = software_signer();
    let message = b"ospp-v1 transcript";
    let signature = signer.sign(message).expect("software signature");
    assert_eq!(signature.as_bytes().len(), 64);
    assert_eq!(signature.to_hex().len(), 128);
    verify_device_signature(signer.key_id(), message, &signature).expect("verifies");

    // A DER-shaped signature is a different encoding; `ospp-v1` never accepts one.
    let der = hex::encode([0x30u8; 70]);
    assert!(matches!(
        DeviceSignature::from_hex(&der),
        Err(OsppError::InvalidSignature { .. })
    ));

    // Private scalar 0x11 * 32: it must never appear in any `Debug` rendering.
    let secret_hex = "11".repeat(32);
    assert!(!format!("{signer:?}").contains(&secret_hex));
    assert!(!format!("{:?}", known_software_key()).contains(&secret_hex));

    let primary: Result<SoftwareP256Signer, OsppError> = Err(OsppError::TpmdUnavailable {
        probed: "/run/tpmd/tpmd.sock".to_owned(),
    });
    let fallback = FallbackDeviceKeySigner::new(primary, || Ok(signer))
        .expect("unavailable selects the software fallback");
    assert!(fallback.is_fallback());
    assert!(!format!("{fallback:?}").contains(&secret_hex));
    let diagnostic = fallback.diagnostic().expect("one explicit diagnostic");
    assert_eq!(diagnostic.label(), "software:file-0600");
    assert!(diagnostic.line().contains("below tpmd sealed floor"));
    assert!(diagnostic.line().contains("/run/tpmd/tpmd.sock"));
    assert!(!diagnostic.line().contains(&secret_hex));
    assert_eq!(fallback.backend_label(), "software:file-0600");
    verify_device_signature(
        fallback.key_id(),
        message,
        &fallback.sign(message).expect("fallback signature"),
    )
    .expect("fallback verifies");

    // A daemon that answered but refused must fail closed: never even construct the software key.
    let primary: Result<SoftwareP256Signer, OsppError> = Err(OsppError::TpmdRejected {
        endpoint: "/run/tpmd/tpmd.sock".to_owned(),
        code: "IncompatiblePolicy".to_owned(),
        message: "sealed floor unavailable".to_owned(),
    });
    let refused =
        FallbackDeviceKeySigner::<SoftwareP256Signer, SoftwareP256Signer>::new(primary, || {
            panic!("the software key must not be constructed when tpmd refused")
        });
    assert!(matches!(refused, Err(OsppError::TpmdRejected { .. })));
}

#[test]
fn signer_probe_reports_tpmd_unavailable_with_every_candidate() {
    let dir = tempfile::tempdir().expect("scratch dir");
    let first = dir.path().join("user/tpmd.sock");
    let second = dir.path().join("system/tpmd.sock");
    let paths = KeyStorePaths::new([first.clone(), second.clone()]);
    let error = TpmdDeviceKeySigner::connect(&paths, None).expect_err("no daemon answered");
    match error {
        OsppError::TpmdUnavailable { probed } => {
            assert!(probed.contains("user/tpmd.sock"), "{probed}");
            assert!(probed.contains("system/tpmd.sock"), "{probed}");
        }
        other => panic!("expected TpmdUnavailable, got {other:?}"),
    }
}

#[test]
fn signer_probe_uses_real_tpmd_frames_across_candidates() {
    let daemon = FakeTpmd::start(TpmdBehavior::Sealed);
    let dead = daemon.dir().join("absent.sock");
    let paths = KeyStorePaths::new([dead, daemon.socket_path().to_path_buf()]);
    let signer = TpmdDeviceKeySigner::connect(&paths, None).expect("second candidate answers");
    assert_eq!(signer.key_id(), daemon.key_id());
    assert_eq!(signer.backend_label(), "tpmd:sealed");
    assert!(format!("{signer:?}").contains("tpmd.sock"));
    assert!(!format!("{signer:?}").contains(&"42".repeat(32)));

    let message = b"ospp-v1 signed by tpmd";
    let signature = signer.sign(message).expect("daemon signs");
    verify_device_signature(signer.key_id(), message, &signature).expect("daemon signature");

    let observed = daemon.observation();
    assert_eq!(observed.handshakes, 1);
    assert_eq!(observed.namespaces, vec!["slipfork".to_owned()]);
    assert_eq!(observed.key_requests, vec!["create"]);
    assert_eq!(observed.signed, 1);
}

#[test]
fn signer_probe_tpmd_serving_never_mints_a_software_key() {
    let dir = tempfile::tempdir().expect("scratch dir");
    let key_path = dir.path().join("device.key");
    let daemon = FakeTpmd::start(TpmdBehavior::Sealed);

    let primary = TpmdDeviceKeySigner::connect(&daemon.paths(), None);
    let signer = FallbackDeviceKeySigner::new(primary, || {
        SoftwareP256Signer::open(
            &FileDeviceKeyStore::new(key_path.clone()),
            &FixedEntropy::new(0x11),
        )
    })
    .expect("primary selected");
    assert!(!signer.is_fallback());
    signer
        .sign(b"ospp-v1 transcript")
        .expect("the tpmd key signs");
    assert!(
        !key_path.exists(),
        "a tpmd-capable machine must not mint a 0600 software key file"
    );

    let primary: Result<TpmdDeviceKeySigner, OsppError> = Err(OsppError::TpmdUnavailable {
        probed: "/run/tpmd/tpmd.sock".to_owned(),
    });
    let fallback = FallbackDeviceKeySigner::new(primary, || {
        SoftwareP256Signer::open(
            &FileDeviceKeyStore::new(key_path.clone()),
            &FixedEntropy::new(0x11),
        )
    })
    .expect("fallback selected");
    assert!(fallback.is_fallback());
    fallback
        .sign(b"ospp-v1 transcript")
        .expect("the software key signs");
    assert!(
        key_path.exists(),
        "the software fallback must create exactly one key file"
    );
}

#[test]
fn signer_probe_resume_identity_mismatch_fails_closed() {
    let daemon = FakeTpmd::start(TpmdBehavior::Sealed);
    let remembered = software_signer().key_id().clone();
    let error = TpmdDeviceKeySigner::connect(&daemon.paths(), Some(&remembered))
        .expect_err("a changed identity must fail closed");
    assert!(
        matches!(error, OsppError::TpmdKeyMismatch { .. }),
        "{error:?}"
    );

    let signer = TpmdDeviceKeySigner::connect(&daemon.paths(), Some(daemon.key_id()))
        .expect("the remembered identity resumes");
    assert_eq!(signer.key_id(), daemon.key_id());
    assert_eq!(
        daemon.observation().key_requests,
        vec!["resume_or_create", "resume_or_create"]
    );
}

#[test]
fn signer_probe_waits_out_a_locked_store() {
    let daemon = FakeTpmd::start(TpmdBehavior::StoreLockedThenOpen);
    let signer = TpmdDeviceKeySigner::connect(&daemon.paths(), None)
        .expect("CLOSED -> AWAIT -> OPENED -> WELCOME");
    assert_eq!(signer.key_id(), daemon.key_id());
    let observed = daemon.observation();
    assert_eq!(observed.awaited, 1);
    assert_eq!(observed.handshakes, 1);
}

#[test]
fn signer_probe_tpmd_refusals_are_not_fallbacks() {
    let daemon = FakeTpmd::start(TpmdBehavior::Reject {
        code: RejectCode::IncompatiblePolicy,
        message: "no sealed backend".to_owned(),
    });
    let error = TpmdDeviceKeySigner::connect(&daemon.paths(), None).expect_err("refused");
    assert!(matches!(error, OsppError::TpmdRejected { .. }), "{error:?}");
    drop(daemon);

    let daemon = FakeTpmd::start(TpmdBehavior::BackendUnavailable {
        message: "no /dev/tpmrm0".to_owned(),
    });
    let error = TpmdDeviceKeySigner::connect(&daemon.paths(), None).expect_err("backend gone");
    assert!(
        matches!(error, OsppError::TpmdBackendUnavailable { .. }),
        "{error:?}"
    );
}

#[test]
fn signer_probe_rejects_a_daemon_signature_that_does_not_verify() {
    let daemon = FakeTpmd::start(TpmdBehavior::BadSignature);
    let signer = TpmdDeviceKeySigner::connect(&daemon.paths(), None).expect("handshake");
    let error = signer
        .sign(b"ospp-v1 transcript")
        .expect_err("verification must fail");
    assert!(
        matches!(error, OsppError::SignatureVerification { .. }),
        "{error:?}"
    );
}

#[test]
fn pairing_fragment_only_url_is_redacted_everywhere() {
    let handle = [0x5au8; 32];
    let handle_hex = hex::encode(handle);
    let raw = format!("https://osp.example/pair#handle={handle_hex}");
    let url = PairingUrl::parse(&raw).expect("fragment handle");
    assert_eq!(url.scheme(), PairingScheme::Https);
    assert_eq!(url.host(), "osp.example");
    assert_eq!(url.port(), 443);
    assert_eq!(url.path(), "/pair");
    assert_eq!(url.browser_url().expect("live URL").as_str(), raw);
    assert_eq!(url.redacted(), "https://osp.example/pair#handle=[REDACTED]");

    let display = url.to_string();
    let debug = format!("{url:?}");
    let json = serde_json::to_string(&url).expect("--json rendering");
    for rendered in [&display, &debug, &json] {
        assert!(!rendered.contains(&handle_hex), "{rendered}");
        assert!(rendered.contains("[REDACTED]"), "{rendered}");
    }

    // A handle anywhere but the fragment is refused rather than promoted.
    assert!(matches!(
        PairingUrl::parse(&format!(
            "https://osp.example/pair?handle={handle_hex}#handle={handle_hex}"
        )),
        Err(OsppError::PairingUrl { .. })
    ));
    match PairingUrl::parse("https://osp.example/pair") {
        Err(OsppError::PairingUrl { reason }) => {
            assert!(reason.contains("missing `#handle=` fragment"), "{reason}");
        }
        other => panic!("a URL without a fragment must fail closed: {other:?}"),
    }
    assert!(matches!(
        PairingUrl::parse(&format!("https://osp.example/pair#token={handle_hex}")),
        Err(OsppError::PairingUrl { .. })
    ));
    assert!(matches!(
        PairingUrl::parse(&format!(
            "https://osp.example/pair#handle={}",
            "A".repeat(64)
        )),
        Err(OsppError::PairingUrl { .. })
    ));
}

#[test]
fn pairing_single_use_handle_is_redeemed_once_and_redacted() {
    let handle = [0x3cu8; 32];
    let signer = software_signer();
    let server = start_server(
        &signer,
        ReferenceServerConfig {
            handles: vec![HandleRecord::new(handle, 1_000)],
            account: "acct-123".to_owned(),
            ..ReferenceServerConfig::default()
        },
    );

    // `redeem_pairing_url` takes the URL by value: the capability cannot be replayed by the client,
    // and `PairingHandle::drop` zeroizes its bytes after the one `pair_hello`.
    let url = PairingUrl::parse(&server.browser_pairing_url(handle)).expect("fragment URL");
    let approval = redeem_pairing_url(url, &signer, &FixedEntropy::new(0x33), live_timeouts())
        .expect("approved");
    assert_eq!(approval.account, "acct-123");

    {
        let state = server.state();
        assert_eq!(state.handles_seen, vec![handle]);
        assert_eq!(state.attempts.len(), 1);
        assert_eq!(state.attempts[0].kind, "pair_hello");
        assert!(state.handle_redeemed(&handle));
    }

    let replay = PairingUrl::parse(&server.browser_pairing_url(handle)).expect("fragment URL");
    let error = redeem_pairing_url(replay, &signer, &FixedEntropy::new(0x34), live_timeouts())
        .expect_err("second redemption refused");
    assert!(
        matches!(
            error,
            OsppError::PairingRejected {
                reason: PairingRejectReason::AlreadyRedeemed
            }
        ),
        "{error:?}"
    );
}

#[test]
fn pairing_expired_handle_fails_closed() {
    let handle = [0x4du8; 32];
    let signer = software_signer();
    let server = start_server(
        &signer,
        ReferenceServerConfig {
            handles: vec![HandleRecord::new(handle, 1_000)],
            account: "acct-123".to_owned(),
            ..ReferenceServerConfig::default()
        },
    );
    server.clock().advance(5 * 60 * 1000);
    let url = PairingUrl::parse(&server.browser_pairing_url(handle)).expect("fragment URL");
    let error = redeem_pairing_url(url, &signer, &FixedEntropy::new(0x35), live_timeouts())
        .expect_err("expired handle refused");
    assert!(
        matches!(
            error,
            OsppError::PairingRejected {
                reason: PairingRejectReason::ExpiredHandle
            }
        ),
        "{error:?}"
    );
}

#[test]
fn reject_paths_bad_signature_unknown_key_and_version() {
    let signer = MisSigningSigner::new(software_signer());
    let server = ReferenceOsppServer::start(ReferenceServerConfig {
        keys: vec![signer.key_id().clone()],
        ..ReferenceServerConfig::default()
    });
    let error = connect_error(
        &server,
        &signer,
        &FixedEntropy::new(0x41),
        live_timeouts(),
        "an invalid signature is refused",
    );
    assert!(
        matches!(
            error,
            OsppError::Rejected {
                reason: RejectReason::BadSignature
            }
        ),
        "{error:?}"
    );
    drop(server);

    let signer = software_signer();
    let server = ReferenceOsppServer::start(ReferenceServerConfig::default());
    let error = connect_error(
        &server,
        &signer,
        &FixedEntropy::new(0x42),
        live_timeouts(),
        "an unregistered key is refused",
    );
    assert!(
        matches!(
            error,
            OsppError::Rejected {
                reason: RejectReason::UnknownKey
            }
        ),
        "{error:?}"
    );
    drop(server);

    let signer = software_signer();
    let server = start_server(
        &signer,
        ReferenceServerConfig {
            forced_reject: Some(RejectReason::UnsupportedVersion { min: 2, max: 3 }),
            ..ReferenceServerConfig::default()
        },
    );
    let error = connect_error(
        &server,
        &signer,
        &FixedEntropy::new(0x43),
        live_timeouts(),
        "no shared version",
    );
    assert!(
        matches!(
            error,
            OsppError::Rejected {
                reason: RejectReason::UnsupportedVersion { min: 2, max: 3 }
            }
        ),
        "{error:?}"
    );
}

#[test]
fn reject_paths_replayed_nonce_is_refused() {
    let signer = software_signer();
    let nonce = [0x77u8; 32];
    let server = start_server(
        &signer,
        ReferenceServerConfig {
            fixed_nonce: Some(nonce),
            ..ReferenceServerConfig::default()
        },
    );
    {
        let _accepted = OsppClient::connect(
            &server.endpoint(),
            &signer,
            &SeededEntropy::new(0x51),
            live_timeouts(),
        )
        .expect("first HELLO accepted");
    }
    assert!(server.state().nonce_used(&nonce));

    let error = connect_error(
        &server,
        &signer,
        &FixedEntropy::new(0x52),
        live_timeouts(),
        "a replayed nonce_s is refused",
    );
    assert!(
        matches!(
            error,
            OsppError::Rejected {
                reason: RejectReason::BadSignature
            }
        ),
        "{error:?}"
    );
}

#[test]
fn reject_paths_oversized_frame_and_deadline() {
    let signer = software_signer();
    let server = ReferenceOsppServer::start(ReferenceServerConfig {
        oversized_first_frame: true,
        ..ReferenceServerConfig::default()
    });
    let error = connect_error(
        &server,
        &signer,
        &FixedEntropy::new(0x61),
        live_timeouts(),
        "a >16 KiB frame is refused",
    );
    match error {
        OsppError::Protocol { phase, message } => {
            assert_eq!(phase, "challenge");
            assert!(message.contains("16 KiB"), "{message}");
        }
        other => panic!("expected a Protocol cap violation, got {other:?}"),
    }
    drop(server);

    let server = ReferenceOsppServer::start(ReferenceServerConfig {
        silent_first_frame: true,
        ..ReferenceServerConfig::default()
    });
    let error = connect_error(
        &server,
        &signer,
        &FixedEntropy::new(0x62),
        timeouts(Duration::from_millis(150)),
        "the first-frame deadline fires",
    );
    assert!(
        matches!(
            error,
            OsppError::Deadline {
                phase: "challenge",
                millis: 150
            }
        ),
        "{error:?}"
    );
}

#[test]
fn reject_paths_challenge_must_come_first() {
    let signer = software_signer();
    let server = ReferenceOsppServer::start(ReferenceServerConfig {
        keys: vec![signer.key_id().clone()],
        welcome_before_challenge: true,
        ..ReferenceServerConfig::default()
    });
    let error = connect_error(
        &server,
        &signer,
        &FixedEntropy::new(0x71),
        live_timeouts(),
        "server-first WELCOME is a protocol violation",
    );
    assert!(
        matches!(
            error,
            OsppError::Protocol {
                phase: "challenge",
                ..
            }
        ),
        "{error:?}"
    );
}

#[test]
fn reject_paths_capability_before_welcome_fails_closed() {
    let signer = software_signer();
    let server = ReferenceOsppServer::start(ReferenceServerConfig {
        keys: vec![signer.key_id().clone()],
        capability_before_challenge: true,
        ..ReferenceServerConfig::default()
    });
    let error = connect_error(
        &server,
        &signer,
        &FixedEntropy::new(0x72),
        live_timeouts(),
        "a capability frame before CHALLENGE is a protocol violation",
    );
    match error {
        OsppError::Protocol { phase, message } => {
            assert_eq!(phase, "challenge");
            assert!(message.contains("env.bundle"), "{message}");
        }
        other => panic!("expected a Protocol violation, got {other:?}"),
    }
}

#[test]
fn vars_secrets_bundle_is_typed_and_redacted() {
    let signer = software_signer();
    let server = start_server(
        &signer,
        ReferenceServerConfig {
            vars: BTreeMap::from([("LOG_LEVEL".to_owned(), "INFO".to_owned())]),
            secrets: BTreeMap::from([(
                "STRIPE_KEY".to_owned(),
                "sk-live-must-never-print".to_owned(),
            )]),
            domains: BTreeMap::from([("api".to_owned(), "api.acme.test".to_owned())]),
            ..ReferenceServerConfig::default()
        },
    );
    let client = Arc::new(connect(&server, &signer));
    let local = OsppSecretProvider::new(Arc::clone(&client), slug(), "local").expect("provider");

    // The shared `SecretProvider` suite runs against the same reference server.
    assert_secret_provider_conformance(|| local.clone());

    assert_eq!(
        local
            .get(Namespace::Vars, "LOG_LEVEL")
            .expect("vars")
            .as_deref(),
        Some("INFO")
    );
    assert_eq!(
        local
            .get(Namespace::Secrets, "STRIPE_KEY")
            .expect("secret")
            .as_deref(),
        Some("sk-live-must-never-print")
    );
    // `domains::` is provider data only in `cloud` mode; local worlds derive `WorldOrigin`.
    assert_eq!(
        local.get(Namespace::Domains, "api").expect("local domain"),
        None
    );

    let cloud = OsppSecretProvider::new(Arc::clone(&client), slug(), "cloud").expect("provider");
    assert_eq!(
        cloud
            .get(Namespace::Domains, "api")
            .expect("cloud domain")
            .as_deref(),
        Some("api.acme.test")
    );

    // Compare-and-set: a second `!!` proposal observes the first stored value.
    let first = local
        .ensure_secret("CAS_TOKEN", "first-generated")
        .expect("first ensure");
    assert_eq!(first, "first-generated");
    let second = local
        .ensure_secret("CAS_TOKEN", "second-different")
        .expect("second ensure");
    assert_eq!(second, first);
    assert_eq!(
        server.state().secret_value("CAS_TOKEN"),
        Some("first-generated")
    );

    // No rendering of a bundle or provider leaks a secret value.
    let bundle = client.env_bundle(&slug(), "local").expect("bundle");
    let debug = format!("{bundle:?}");
    assert!(debug.contains("[REDACTED]"), "{debug}");
    assert!(!debug.contains("sk-live-must-never-print"), "{debug}");
    let provider_debug = format!("{local:?}");
    assert!(provider_debug.contains("acme:local"), "{provider_debug}");
    assert!(
        !provider_debug.contains("sk-live-must-never-print"),
        "{provider_debug}"
    );
    assert!(
        !provider_debug.contains("first-generated"),
        "{provider_debug}"
    );

    // Every capability request named the world scope, and `vars`/`secrets`/`domains` stayed
    // separate namespaces.
    let state = server.state();
    assert!(
        state
            .capability_requests
            .iter()
            .all(|request| request.project == "acme")
    );
    assert!(
        state
            .capability_requests
            .iter()
            .any(|request| request.capability == "env.bundle" && request.mode == "cloud")
    );
}

#[test]
fn ensure_converges_two_concurrent_callers_on_one_value() {
    let signer = software_signer();
    let server = start_server(&signer, ReferenceServerConfig::default());
    let client = Arc::new(connect(&server, &signer));
    let provider =
        Arc::new(OsppSecretProvider::new(Arc::clone(&client), slug(), "local").expect("provider"));

    let mut callers = Vec::new();
    for proposed in ["value-from-a", "value-from-b"] {
        let provider = Arc::clone(&provider);
        callers.push(thread::spawn(move || {
            provider.ensure_secret("SHARED_TOKEN", proposed)
        }));
    }
    let observed: Vec<String> = callers
        .into_iter()
        .map(|caller| caller.join().expect("caller thread").expect("ensure"))
        .collect();
    assert_eq!(observed.len(), 2);
    assert_eq!(
        observed[0], observed[1],
        "two racing ensure callers must observe one value"
    );

    let state = server.state();
    assert_eq!(state.ensure_calls.len(), 2);
    assert!(
        state
            .ensure_calls
            .iter()
            .all(|call| call.returned == observed[0])
    );
    assert_eq!(
        state.secret_value("SHARED_TOKEN"),
        Some(observed[0].as_str())
    );
}

#[test]
fn resources_strategy_maps_the_bundle_into_one_offer() {
    let signer = software_signer();
    let dump = DumpRecord::new("main", pseudo_random(8_192));
    let expected_sha = dump.sha256_compressed();
    let expected_len = dump.compressed().len() as u64;
    let server = start_server(
        &signer,
        ReferenceServerConfig {
            s3_buckets: BTreeMap::from([(
                "assets".to_owned(),
                S3BucketRecord::new(
                    "https://s3.acme.test",
                    "eu-central-1",
                    "acme-dev-assets",
                    "AKIA-TEST-ACCESS",
                    "secret-key-must-never-print",
                ),
            )]),
            pg_databases: BTreeMap::from([("main".to_owned(), dump)]),
            ..ReferenceServerConfig::default()
        },
    );
    let client = connect(&server, &signer);
    let offer = client
        .resource_offer(&slug(), "local")
        .expect("resources.describe")
        .expect("an offer");

    assert_eq!(
        offer.s3_bucket("assets").expect("bucket").bucket(),
        "acme-dev-assets"
    );
    let dump_offer = offer.pg_dump("main").expect("dump");
    assert_eq!(dump_offer.sha256_compressed(), expected_sha);
    assert_eq!(dump_offer.compressed_bytes(), expected_len);
    let rendered = format!("{offer:?}");
    assert!(
        !rendered.contains("secret-key-must-never-print"),
        "{rendered}"
    );
    assert!(!rendered.contains("AKIA-TEST-ACCESS"), "{rendered}");

    let spec = SlipforkConfig::parse_toml(
        r#"
[resources.s3_buckets.assets]
local = "remote"

[resources.pg_databases.main]
local = "remote"
"#,
    )
    .expect("slipfork.toml")
    .resources;
    let plan = ResourcePlan::resolve(
        &spec,
        ResourceMode::Local,
        DumpPolicy::Default,
        Some(&offer),
    )
    .expect("plan");
    assert_eq!(
        plan.s3_bucket("assets").expect("bucket").target(),
        ResourceTarget::Remote
    );
    assert!(matches!(
        plan.pg_database("main").expect("dump").seed,
        PgSeed::RemoteDump { refresh: false, .. }
    ));

    // An explicit required remote resource fails closed when OSPP offers nothing.
    let error = ResourcePlan::resolve(&spec, ResourceMode::Local, DumpPolicy::Default, None)
        .expect_err("a required remote resource must fail closed");
    assert!(
        matches!(error, DagError::RemoteResourceUnavailable { .. }),
        "{error:?}"
    );

    let state = server.state();
    assert_eq!(state.capability_requests.len(), 1);
    assert_eq!(
        state.capability_requests[0].capability,
        "resources.describe"
    );
    assert_eq!(state.capability_requests[0].project, "acme");
    assert_eq!(state.capability_requests[0].mode, "local");
}

#[test]
fn test_mode_hermetic_default_ignores_an_offered_remote_everything() {
    let signer = software_signer();
    let server = start_server(
        &signer,
        ReferenceServerConfig {
            s3_buckets: BTreeMap::from([(
                "assets".to_owned(),
                S3BucketRecord::new(
                    "https://s3.acme.test",
                    "eu-central-1",
                    "acme-dev-assets",
                    "AKIA-TEST",
                    "secret",
                ),
            )]),
            pg_databases: BTreeMap::from([(
                "main".to_owned(),
                DumpRecord::new("main", pseudo_random(1_024)),
            )]),
            ..ReferenceServerConfig::default()
        },
    );
    let client = connect(&server, &signer);
    let offer = client
        .resource_offer(&slug(), "test")
        .expect("resources.describe")
        .expect("an offer");

    // Default `[resources]` in `TestSlot` worlds: hermetically local despite a full offer.
    let default_spec = ResourcesSpec::default();
    let plan = ResourcePlan::resolve(
        &default_spec,
        ResourceMode::Test,
        DumpPolicy::Default,
        Some(&offer),
    )
    .expect("plan");
    assert!(!plan.s3_buckets.is_empty());
    assert!(!plan.pg_databases.is_empty());
    assert!(
        plan.s3_buckets
            .iter()
            .all(|bucket| bucket.target() == ResourceTarget::Local),
        "{plan:?}"
    );
    assert!(
        plan.pg_databases.iter().all(|database| {
            database.target() == ResourceTarget::Local
                && matches!(database.seed, PgSeed::CleanInitdb)
        }),
        "{plan:?}"
    );

    // An explicit `test = "remote"` override is the only way to require OSPP in a `TestSlot`...
    let spec = SlipforkConfig::parse_toml(
        r#"
[resources.s3_buckets.assets]
test = "remote"
"#,
    )
    .expect("slipfork.toml")
    .resources;
    let error = ResourcePlan::resolve(&spec, ResourceMode::Test, DumpPolicy::Default, None)
        .expect_err("required remote without an offer must fail closed");
    assert!(
        matches!(
            error,
            DagError::RemoteResourceUnavailable {
                kind: "s3_bucket",
                ..
            }
        ),
        "{error:?}"
    );
    // ...and then resolves only because the server really offered it.
    let plan = ResourcePlan::resolve(&spec, ResourceMode::Test, DumpPolicy::Default, Some(&offer))
        .expect("plan");
    assert_eq!(
        plan.s3_bucket("assets").expect("bucket").target(),
        ResourceTarget::Remote
    );
}

/// Start a server that offers one dump and connect a client to it.
fn dump_fixture(
    mutate: impl FnOnce(&mut ReferenceServerConfig),
) -> (ReferenceOsppServer, OsppClient, DumpRecord, ProjectSlug) {
    let signer = software_signer();
    let dump = DumpRecord::new("main", pseudo_random(8_192));
    let mut config = ReferenceServerConfig {
        pg_databases: BTreeMap::from([("main".to_owned(), dump.clone())]),
        dump_chunk_bytes: 4_096,
        ..ReferenceServerConfig::default()
    };
    mutate(&mut config);
    let server = start_server(&signer, config);
    let client = connect(&server, &signer);
    (server, client, dump, slug())
}

#[test]
fn dump_stream_issues_a_digest_pinned_archive() {
    let (server, client, dump, project) = dump_fixture(|_| {});
    let scratch = disk_scratch();
    let store = DiskSeedDumpStore::open(scratch.path()).expect("disk store");
    let artifact = DumpStreamer::new(&client)
        .fetch(&project, "local", "main", &store)
        .expect("streamed dump");

    let sha = dump.sha256_compressed();
    let expected = dump.compressed();
    assert_eq!(
        artifact.path(),
        Some(store.dir().join(format!("{sha}.pgdump")).as_path())
    );
    assert_eq!(artifact.sha256_compressed(), sha);
    assert_eq!(artifact.compressed_bytes(), expected.len() as u64);
    let mut opened = artifact.open().expect("open verified dump");
    let mut bytes = Vec::new();
    opened.read_to_end(&mut bytes).expect("read verified dump");
    assert_eq!(bytes, expected);
    artifact
        .verify_zstd()
        .expect("the published archive decodes as zstd");
    assert_eq!(
        zstd::decode_all(&expected[..]).expect("valid zstd"),
        dump.archive,
        "the cached archive is the real zstd stream the server produced"
    );
    assert!(store.lookup(&sha).is_some());
    assert!(!store.dir().join(format!("{sha}.part")).exists());

    // The cache survives a fresh store over the same root (a later `up` reuses the dump).
    let reopened = DiskSeedDumpStore::open(scratch.path()).expect("reopened store");
    assert_eq!(
        reopened.lookup(&sha),
        artifact.path().map(Path::to_path_buf)
    );

    let state = server.state();
    assert_eq!(state.dumps_served, vec!["main".to_owned()]);
    assert!(
        state.dump_frames > 1,
        "the dump spans several 64 KiB-bounded frames"
    );
    assert_eq!(state.dump_bytes, expected.len() as u64);
}

#[test]
fn dump_stream_works_with_a_filesystem_free_store() {
    let (server, client, dump, project) = dump_fixture(|_| {});
    let store = MemorySeedDumpStore::default();
    let artifact = DumpStreamer::new(&client)
        .fetch(&project, "local", "main", &store)
        .expect("streamed dump into memory");

    let sha = dump.sha256_compressed();
    let expected = dump.compressed();
    assert_eq!(artifact.path(), None, "an in-memory store has no path");
    assert_eq!(artifact.sha256_compressed(), sha);
    assert_eq!(artifact.compressed_bytes(), expected.len() as u64);
    let mut opened = artifact.open().expect("open in-memory dump");
    let mut bytes = Vec::new();
    opened.read_to_end(&mut bytes).expect("read in-memory dump");
    assert_eq!(bytes, expected);
    artifact
        .verify_zstd()
        .expect("the in-memory archive decodes as zstd");
    assert!(store.is_published(&sha));
    assert!(store.discards().is_empty());
    assert_eq!(server.state().dumps_served, vec!["main".to_owned()]);
}

#[test]
fn dump_stream_truncation_discards_the_partial_artifact() {
    let (_server, client, dump, project) = dump_fixture(|config| config.truncate_dump = true);
    let store = MemorySeedDumpStore::default();

    let error = DumpStreamer::new(&client)
        .fetch(&project, "local", "main", &store)
        .expect_err("a truncated stream must abort");
    assert!(
        matches!(
            error,
            OsppError::SeedDump {
                capability: "dump.begin",
                ..
            }
        ),
        "{error:?}"
    );
    assert!(!store.is_published(&dump.sha256_compressed()));
    assert_eq!(store.discards(), vec![dump.sha256_compressed()]);
}

#[test]
fn dump_stream_truncation_aborts_and_unlinks_before_any_restore() {
    let (server, client, dump, project) = dump_fixture(|config| config.truncate_dump = true);
    let scratch = disk_scratch();
    let store = DiskSeedDumpStore::open(scratch.path()).expect("disk store");

    let error = DumpStreamer::new(&client)
        .fetch(&project, "local", "main", &store)
        .expect_err("a truncated stream must abort");
    assert!(
        matches!(
            error,
            OsppError::SeedDump {
                capability: "dump.begin",
                ..
            }
        ),
        "{error:?}"
    );
    assert!(
        store.lookup(&dump.sha256_compressed()).is_none(),
        "no digest-pinned dump may be published from a truncated stream"
    );
    assert_eq!(
        fs::read_dir(store.dir()).expect("dumps dir").count(),
        0,
        "the .part file must be unlinked before any database mutation"
    );
    assert_eq!(server.state().dumps_served, vec!["main".to_owned()]);

    // An abandoned dump leaves unread binary frames: the session must be re-established.
    let error = client
        .env_bundle(&project, "local")
        .expect_err("the aborted session must be poisoned");
    assert!(matches!(error, OsppError::Transport { .. }), "{error:?}");
}

#[test]
fn dump_stream_digest_mismatch_aborts_and_unlinks() {
    let (server, client, dump, project) = dump_fixture(|config| config.corrupt_dump = true);
    let scratch = disk_scratch();
    let store = DiskSeedDumpStore::open(scratch.path()).expect("disk store");

    let error = DumpStreamer::new(&client)
        .fetch(&project, "local", "main", &store)
        .expect_err("a digest mismatch must abort");
    assert!(matches!(error, OsppError::SeedDump { .. }), "{error:?}");
    assert!(store.lookup(&dump.sha256_compressed()).is_none());
    assert_eq!(
        fs::read_dir(store.dir()).expect("dumps dir").count(),
        0,
        "a mis-hashed .part file must be unlinked"
    );
    assert_eq!(server.state().dumps_served, vec!["main".to_owned()]);
}

#[test]
fn dump_stream_rejects_an_oversized_binary_frame() {
    let (_server, client, dump, project) =
        dump_fixture(|config| config.oversized_dump_frame = true);
    let scratch = disk_scratch();
    let store = DiskSeedDumpStore::open(scratch.path()).expect("disk store");

    let error = DumpStreamer::new(&client)
        .fetch(&project, "local", "main", &store)
        .expect_err("a >64 KiB binary frame must abort");
    assert!(matches!(error, OsppError::SeedDump { .. }), "{error:?}");
    assert!(store.lookup(&dump.sha256_compressed()).is_none());
    assert_eq!(fs::read_dir(store.dir()).expect("dumps dir").count(), 0);
}

#[test]
fn dump_stream_rejects_an_unknown_archive_format() {
    let (_server, client, _dump, project) =
        dump_fixture(|config| config.dump_format = Some("pg_custom_gzip".to_owned()));
    let scratch = disk_scratch();
    let store = DiskSeedDumpStore::open(scratch.path()).expect("disk store");

    let error = DumpStreamer::new(&client)
        .fetch(&project, "local", "main", &store)
        .expect_err("an unknown dump format must fail closed");
    match error {
        OsppError::Protocol { phase, message } => {
            assert_eq!(phase, "dump.begin");
            assert!(
                message.contains("pg_custom_gzip") || message.contains("format"),
                "{message}"
            );
        }
        other => panic!("expected a Protocol format refusal, got {other:?}"),
    }
    assert_eq!(fs::read_dir(store.dir()).expect("dumps dir").count(), 0);
}

#[test]
fn dump_stream_rejects_tmpfs_cache_roots() {
    let shm = std::path::Path::new("/dev/shm");
    assert!(
        shm.is_dir(),
        "/dev/shm must exist to prove tmpfs rejection on this host"
    );
    let tmpfs = tempfile::Builder::new()
        .prefix("ospp-tmpfs-")
        .tempdir_in(shm)
        .expect("shm scratch");
    match DiskSeedDumpStore::open(tmpfs.path()) {
        Err(OsppError::SeedDumpStore { path, message }) => {
            assert!(path.contains("dumps"), "{path}");
            assert!(message.contains("tmpfs"), "{message}");
        }
        Err(other) => panic!("expected a SeedDumpStore refusal, got {other:?}"),
        Ok(_) => panic!("a tmpfs cache root must be refused"),
    }

    let disk = disk_scratch();
    assert!(
        DiskSeedDumpStore::open(disk.path()).is_ok(),
        "the workspace target directory is disk-backed"
    );
}

#[test]
fn offline_vault_resolves_every_secret_without_the_server() {
    let signer = software_signer();
    let server = start_server(&signer, ReferenceServerConfig::default());
    let client = Arc::new(connect(&server, &signer));
    let scratch = disk_scratch();
    let vault_path = scratch.path().join("vault.json");
    let vault = FileVaultProvider::open(&vault_path).expect("local vault");
    vault
        .put(Namespace::Vars, "API_URL", "http://acme.localhost")
        .expect("seed vars");
    vault
        .put(Namespace::Secrets, "STATIC_KEY", "vault-only-value")
        .expect("seed secrets");

    let ospp = OsppSecretProvider::new(Arc::clone(&client), slug(), "local").expect("provider");
    let layered = LayeredSecretProvider::new(vault, ospp);

    // Vault-first: present keys never consult OSPP at all.
    assert_eq!(
        layered
            .get(Namespace::Vars, "API_URL")
            .expect("vault vars")
            .as_deref(),
        Some("http://acme.localhost")
    );
    assert_eq!(
        layered
            .get(Namespace::Secrets, "STATIC_KEY")
            .expect("vault secret")
            .as_deref(),
        Some("vault-only-value")
    );
    assert!(server.state().capability_requests.is_empty());

    // A newly generated `!!` secret is written to the local `0600` vault, never to OSPP.
    assert_eq!(
        layered
            .ensure_secret("GENERATED", "local-generated")
            .expect("ensure"),
        "local-generated"
    );
    assert!(
        server.state().ensure_calls.is_empty(),
        "OSPP must never receive a `secrets.ensure` for a local `!!` write"
    );
    let reopened = FileVaultProvider::open(&vault_path).expect("reopen vault");
    assert_eq!(
        reopened
            .get(Namespace::Secrets, "GENERATED")
            .expect("vault read")
            .as_deref(),
        Some("local-generated")
    );

    // Stop the server for real: the vault still answers every resolved secret.
    drop(server);
    assert_eq!(
        layered
            .get(Namespace::Secrets, "STATIC_KEY")
            .expect("offline static secret")
            .as_deref(),
        Some("vault-only-value")
    );
    assert_eq!(
        layered
            .get(Namespace::Secrets, "GENERATED")
            .expect("offline generated secret")
            .as_deref(),
        Some("local-generated")
    );
}

#[test]
fn capability_denial_is_typed_and_names_the_world_scope() {
    let signer = software_signer();
    let server = start_server(
        &signer,
        ReferenceServerConfig {
            deny_capabilities: true,
            ..ReferenceServerConfig::default()
        },
    );
    let client = Arc::new(connect(&server, &signer));
    let provider = OsppSecretProvider::new(client, slug(), "local").expect("provider");

    match provider.get(Namespace::Vars, "LOG_LEVEL") {
        Err(ProviderError::Io { path, message }) => {
            assert!(path.contains("env.bundle"), "{path}");
            assert!(path.contains("acme:local"), "{path}");
            assert!(message.contains("not_authenticated"), "{message}");
        }
        other => panic!("a denied capability must fail closed with ProviderError::Io: {other:?}"),
    }
    assert_eq!(server.state().capability_requests.len(), 1);
}
