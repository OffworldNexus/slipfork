//! Real-disk checks for the public `<state_dir>/ospp.json` pairing record.
//!
//! The auth journey runs the real `slipfork-ospp-testkit` reference server (loopback WebSocket),
//! the real P-256 signer, and `DaemonService::auth_login`; only the public coordinates may reach
//! disk. The boot round-trip executes the same `resolve_boot_endpoint` `slipforkd` calls.

use super::super::pairing_store::{PairingRecord, PairingStore, resolve_boot_endpoint};
use super::engine::{disk_scratch, test_service_with_ospp};
use slipfork_ospp::DeviceKeySigner;
use slipfork_ospp_testkit::signers::software_signer;
use slipfork_ospp_testkit::{ReferenceOsppServer, ReferenceServerConfig};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// One real server-side secret value the pairing record must never carry.
const SERVER_SECRET: &str = "reference-server-secret-\u{1f510}";

fn mode_of(path: &Path) -> u32 {
    fs::metadata(path)
        .expect("pairing record metadata")
        .permissions()
        .mode()
        & 0o777
}

fn wait_for_pairing(server: &ReferenceOsppServer) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(pairing) = server.state().pairings.last() {
            return pairing.handle.clone();
        }
        assert!(
            Instant::now() < deadline,
            "the reference server never issued a pairing handle"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
pub(super) fn auth_journey_persists_only_public_pairing_coordinates() {
    let scratch = disk_scratch("pairing-auth");
    let worktree = scratch.join("worktree");
    fs::create_dir_all(&worktree).expect("worktree");

    let server = ReferenceOsppServer::start(ReferenceServerConfig {
        auto_approve: false,
        ..ReferenceServerConfig::default()
    });
    server.set_secret("REMOTE_TOKEN", SERVER_SECRET);
    let endpoint = server.endpoint();
    let signer = Arc::new(software_signer());
    let key_hex = signer.public_key_hex().to_owned();
    let service = test_service_with_ospp(&scratch, &worktree, endpoint.clone(), signer);

    let approval = thread::scope(|scope| {
        let auth = scope.spawn(|| service.auth_login(None));
        let handle = wait_for_pairing(&server);
        server.approve_pairing(&handle);
        auth.join().expect("auth thread").expect("auth_login")
    });
    assert_eq!(approval.account.as_deref(), Some("alice"));
    assert_eq!(approval.machine.as_deref(), Some("dev-laptop"));
    assert!(
        approval
            .pairing_url
            .expose()
            .starts_with("http://127.0.0.1:")
    );

    let path = scratch.join("state").join("ospp.json");
    let bytes = fs::read(&path).expect("pairing record bytes");
    let value: serde_json::Value = serde_json::from_slice(&bytes).expect("valid pairing JSON");
    let object = value.as_object().expect("pairing record is an object");
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(keys, ["account", "endpoint", "machine"]);
    assert_eq!(object["endpoint"].as_str(), Some(endpoint.as_str()));
    assert_eq!(object["account"].as_str(), Some("alice"));
    assert_eq!(object["machine"].as_str(), Some("dev-laptop"));
    assert_eq!(mode_of(&path), 0o600);
    assert_eq!(
        fs::read_dir(path.parent().expect("state directory"))
            .expect("state directory listing")
            .count(),
        1,
        "the auth journey writes exactly one file"
    );

    // Scan the bytes this real journey wrote: no signer material and no server secret.
    let text = String::from_utf8_lossy(&bytes).into_owned();
    for forbidden in [key_hex.as_str(), SERVER_SECRET, "private", "token", "claim"] {
        assert!(
            !text.contains(forbidden),
            "pairing record must never contain {forbidden:?}"
        );
    }

    // Atomic replace over a permissive legacy record: final mode is 0600 and no temp survives.
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644))
        .expect("permissive legacy pairing record");
    let store = PairingStore::new(path.clone());
    let saved = store.load().expect("load pairing").expect("saved pairing");
    store.record(&saved).expect("atomic replacement");
    assert_eq!(mode_of(&path), 0o600);
    assert_eq!(store.load().expect("reload pairing"), Some(saved));
    assert_eq!(
        fs::read_dir(path.parent().expect("state directory"))
            .expect("state directory listing")
            .count(),
        1
    );
}

#[test]
pub(super) fn boot_endpoint_prefers_the_flag_and_never_aborts_on_a_bad_record() {
    let scratch = disk_scratch("pairing-boot");
    let server = ReferenceOsppServer::start(ReferenceServerConfig::default());
    let endpoint = server.endpoint();
    let path = scratch.join("ospp.json");
    let store = PairingStore::new(path.clone());

    // No saved pairing: local-only, no warning.
    let absent = resolve_boot_endpoint(None, &scratch.join("missing.json")).expect("boot");
    assert!(absent.endpoint.is_none());
    assert!(absent.warning.is_none());

    // Flag absent + valid record: the daemon boots against the saved server endpoint.
    store
        .record(&PairingRecord {
            endpoint: endpoint.as_str().to_owned(),
            account: "alice".to_owned(),
            machine: "dev-laptop".to_owned(),
        })
        .expect("saved pairing");
    let saved = resolve_boot_endpoint(None, &path).expect("boot");
    assert_eq!(
        saved.endpoint.as_ref().map(|endpoint| endpoint.as_str()),
        Some(endpoint.as_str())
    );
    assert!(saved.warning.is_none());

    // Explicit `--ospp-endpoint` wins over the saved record.
    let explicit =
        resolve_boot_endpoint(Some("wss://explicit.example.test/_ospp/v1"), &path).expect("boot");
    assert_eq!(
        explicit.endpoint.as_ref().map(|endpoint| endpoint.as_str()),
        Some("wss://explicit.example.test/_ospp/v1")
    );
    assert!(explicit.warning.is_none());
    assert!(resolve_boot_endpoint(Some("http://not-a-ws-endpoint"), &path).is_err());

    // Corrupt record: one warning line, then local-only — never abort boot.
    fs::write(&path, b"{ not json").expect("corrupt pairing record");
    let corrupt = resolve_boot_endpoint(None, &path).expect("boot");
    assert!(corrupt.endpoint.is_none());
    let warning = corrupt.warning.expect("warning line");
    assert!(!warning.contains('\n'), "exactly one stderr warning line");
    assert!(warning.contains("booting local-only"));

    // Structurally valid JSON with an invalid endpoint behaves the same way.
    fs::write(
        &path,
        br#"{"endpoint":"nope","account":"alice","machine":"dev-laptop"}"#,
    )
    .expect("invalid endpoint record");
    let invalid = resolve_boot_endpoint(None, &path).expect("boot");
    assert!(invalid.endpoint.is_none());
    let warning = invalid.warning.expect("warning line");
    assert!(!warning.contains('\n'), "exactly one stderr warning line");
    assert!(warning.contains("booting local-only"));
}
