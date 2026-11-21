use super::*;

#[test]
fn nested_block_comments_survive_format() {
    let source = "# forkfile: 0.1\n\nenv common {\n  # runtime vars\n  A = \"1\"\n}\n\nimage backend . {\n  service api {\n    # the api service\n    port 8000\n  }\n}\n";
    let doc = parse(source).expect("parses");
    let formatted = format(&doc);
    assert!(formatted.contains("# runtime vars"), "{formatted}");
    assert!(formatted.contains("# the api service"), "{formatted}");
    assert_eq!(doc, parse(&formatted).expect("reparses"));
}

#[test]
fn trailing_comments_stay_on_their_item_line() {
    let source = "# forkfile: 0.1\n\nimage backend . {\n  stage prod # note\n  service api {\n    port 8000\n  }\n}\n";
    let doc = parse(source).expect("parses");
    let formatted = format(&doc);
    assert!(formatted.contains("stage prod # note"), "{formatted}");
    assert_eq!(doc, parse(&formatted).expect("reparses"));
}

#[test]
fn full_spec_sample_round_trips_through_format() {
    let doc = parse(SAMPLE).expect("sample parses");
    let formatted = format(&doc);
    let reparsed = parse(&formatted).expect("formatted sample parses");
    assert_eq!(
        doc, reparsed,
        "formatted output changed the tree:\n{formatted}"
    );
}

#[test]
fn format_is_idempotent() {
    let once = format(&parse(SAMPLE).expect("sample parses"));
    let twice = format(&parse(&once).expect("formatted sample parses"));
    assert_eq!(once, twice);
}

#[test]
fn format_preserves_comments_at_every_level() {
    let source = "# forkfile: 0.1\n\n# backing services\ns3 uploads\n\nimage backend . {\n  # build step\n  stage prod\n}\n";
    let doc = parse(source).expect("parses");
    assert!(
        doc.items
            .comments
            .iter()
            .any(|comment| comment.text.contains("backing services"))
    );
    let formatted = format(&doc);
    assert!(formatted.contains("# backing services"), "{formatted}");
    assert!(formatted.contains("# build step"), "{formatted}");
    assert_eq!(doc, parse(&formatted).expect("formatted parses"));
}

#[test]
fn format_check_reports_a_diff_until_canonical() {
    let check = format_check(SAMPLE).expect("sample parses");
    assert_eq!(check.formatted, format(&parse(SAMPLE).expect("parses")));
    assert!(!check.is_formatted());
    assert!(check.diff.contains("--- Forkfile"), "{}", check.diff);
    let canonical = format_check(&check.formatted).expect("formatted parses");
    assert!(canonical.is_formatted());
    assert!(canonical.diff.is_empty());
}

#[test]
fn missing_final_newline_still_produces_a_real_hunk() {
    let source = "# forkfile: 0.1\ndomain main";
    let check = format_check(source).expect("parses");
    assert!(!check.is_formatted());
    assert!(
        check.diff.lines().any(|line| line == "-domain main"),
        "{}",
        check.diff
    );
    assert!(
        check.diff.lines().any(|line| line == "+domain main"),
        "{}",
        check.diff
    );
}
