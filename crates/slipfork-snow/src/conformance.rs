//! Shared conformance suite executed against every [`SecretProvider`] implementation.

use crate::provider::{Namespace, SecretProvider};

/// Verify that `make_provider` satisfies the full [`SecretProvider`] contract:
/// isolated namespaces, `None` on missing keys, and write-once `ensure_secret` idempotence.
pub fn assert_secret_provider_conformance<P: SecretProvider>(make_provider: impl Fn() -> P) {
    let mut p = make_provider();
    assert_eq!(
        p.get(Namespace::Secrets, "SECRET_KEY")
            .expect("get missing"),
        None
    );
    assert_eq!(
        p.get(Namespace::Vars, "SECRET_KEY").expect("get vars"),
        None
    );

    // First ensure writes the proposed secret.
    let first = p
        .ensure_secret("SECRET_KEY", "first-generated-value")
        .expect("first ensure");
    assert_eq!(first, "first-generated-value");
    assert_eq!(
        p.get(Namespace::Secrets, "SECRET_KEY")
            .expect("read back")
            .as_deref(),
        Some("first-generated-value")
    );

    // Second ensure with a DIFFERENT proposed value MUST preserve the first (`put_if_absent`).
    let second = p
        .ensure_secret("SECRET_KEY", "second-different-value")
        .expect("second ensure");
    assert_eq!(second, "first-generated-value");

    // `secrets::SECRET_KEY` never leaks into `vars::SECRET_KEY`.
    assert_eq!(
        p.get(Namespace::Vars, "SECRET_KEY").expect("vars isolated"),
        None
    );
}
