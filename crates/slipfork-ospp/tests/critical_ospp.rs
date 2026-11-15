//! Protected conformance suite for the OSPP v1 client, device-key signers, and pairing.
//!
//! Every test drives the production code path. The WebSocket server, the seeded CSPRNG, and the
//! fake `tpmd` daemon are dev-only doubles in `tests/support/`; the signatures, transcripts, and
//! frames are real.

mod support;

use std::time::Duration;

use support::{
    FakeTpmd, FixedEntropy, HandleRecord, MisSigningSigner, MockOsppServer, MockServerConfig,
    SeededEntropy, TpmdBehavior, known_software_key, software_signer,
};

use slipfork_ospp::{
    DeviceKeySigner, DeviceSignature, FallbackDeviceKeySigner, FileDeviceKeyStore, KeyStorePaths,
    OsppClient, OsppError, OsppTimeouts, PairingRejectReason, PairingScheme, PairingUrl,
    RejectReason, SoftwareP256Signer, TpmdDeviceKeySigner, connect_transcript, pairing_transcript,
    redeem_pairing_url, verify_device_signature,
};
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

/// `OsppClient` deliberately has no `Debug`, so surface a connect failure without `expect_err`.
fn connect_error(
    server: &MockOsppServer,
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
    let server = MockOsppServer::start(MockServerConfig {
        keys: vec![key_id.clone()],
        ..MockServerConfig::default()
    });
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
    let server = MockOsppServer::start(MockServerConfig {
        keys: vec![signer.key_id().clone()],
        handles: vec![HandleRecord::new(handle, 1_000)],
        account: "acct-123".to_owned(),
        ..MockServerConfig::default()
    });

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
    let server = MockOsppServer::start(MockServerConfig {
        keys: vec![signer.key_id().clone()],
        handles: vec![HandleRecord::new(handle, 1_000)],
        account: "acct-123".to_owned(),
        ..MockServerConfig::default()
    });
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
    let server = MockOsppServer::start(MockServerConfig {
        keys: vec![signer.key_id().clone()],
        ..MockServerConfig::default()
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
    let server = MockOsppServer::start(MockServerConfig::default());
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
    let server = MockOsppServer::start(MockServerConfig {
        keys: vec![signer.key_id().clone()],
        forced_reject: Some(RejectReason::UnsupportedVersion { min: 2, max: 3 }),
        ..MockServerConfig::default()
    });
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
    let server = MockOsppServer::start(MockServerConfig {
        keys: vec![signer.key_id().clone()],
        fixed_nonce: Some(nonce),
        ..MockServerConfig::default()
    });
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
    let server = MockOsppServer::start(MockServerConfig {
        oversized_first_frame: true,
        ..MockServerConfig::default()
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

    let server = MockOsppServer::start(MockServerConfig {
        silent_first_frame: true,
        ..MockServerConfig::default()
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
    let server = MockOsppServer::start(MockServerConfig {
        keys: vec![signer.key_id().clone()],
        welcome_before_challenge: true,
        ..MockServerConfig::default()
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
