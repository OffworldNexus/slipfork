use super::*;

#[test]
fn spec_sample_typechecks_and_resolves_cdn_properties() {
    let document = parse(SAMPLE).expect("sample parses");
    let checked = crate::check::check(&document).expect("sample typechecks");
    assert!(
        checked
            .symbols
            .get("static_cdn")
            .is_some_and(|symbol| symbol.kind == crate::check::SymbolKind::Cdn)
    );
    assert!(
        checked
            .symbols
            .get("uploads")
            .is_some_and(|symbol| symbol.kind == crate::check::SymbolKind::S3)
    );
}
