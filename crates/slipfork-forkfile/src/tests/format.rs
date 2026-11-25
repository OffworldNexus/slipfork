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
fn inline_on_with_else_round_trips_through_canonical_block_form() {
    let cases = [
        // top-level block body + block else
        "# forkfile: 0.1\n\non dev {\n  domain main\n} else {\n  domain alt\n}\n",
        // top-level inline item + next-line inline else
        "# forkfile: 0.1\n\non dev: domain main\nelse: domain alt\n",
        // chained else after an inline item
        "# forkfile: 0.1\n\non dev: domain main\nelse on mail: domain alt\nelse: domain third\n",
        // image item
        "# forkfile: 0.1\n\nimage app . {\n  on dev: stage dev\n  else: stage prod\n}\n",
        // service item with a next-line block else
        "# forkfile: 0.1\n\nimage app . {\n  service web {\n    on dev: run: /bin/echo dev\n    else {\n      run: /bin/echo prod\n    }\n  }\n}\n",
        // s3 item
        "# forkfile: 0.1\n\ns3 uploads {\n  on dev: policy public-read\n  else: policy private\n}\n",
        // env item
        "# forkfile: 0.1\n\nenv common {\n  on dev: A = \"1\"\n  else: A = \"2\"\n}\n",
        // command block
        "# forkfile: 0.1\n\nimage app . {\n  service web {\n    run {\n      on dev: /bin/echo dev\n      else: /bin/echo prod\n    }\n  }\n}\n",
    ];
    for source in cases {
        let document = parse(source).unwrap_or_else(|diagnostics| {
            panic!("parse failed for {source:?}: {:?}", diagnostics.items)
        });
        let once = format(&document);
        let reparsed = parse(&once).unwrap_or_else(|diagnostics| {
            panic!(
                "formatted output failed to parse: {once}\n{:?}",
                diagnostics.items
            )
        });
        let twice = format(&reparsed);
        assert_eq!(once, twice, "formatting is not idempotent:\n{once}");
    }

    let inline = "# forkfile: 0.1\n\nimage app . {\n  service web {\n    on dev: run: /bin/echo dev\n    else {\n      run: /bin/echo prod\n    }\n  }\n}\n";
    let formatted = format(&parse(inline).expect("inline else parses"));
    assert!(
        formatted.contains("        on dev {\n            run: /bin/echo dev\n        } else {"),
        "inline `on` + else must render the canonical block form:\n{formatted}"
    );
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

const DEMO: &str = include_str!("../../../../support/demo/Forkfile");

#[test]
fn header_blank_is_preserved_when_authored_and_flush_when_not() {
    let spaced = "# forkfile: 0.1\n\ns3 assets\n";
    assert_eq!(format(&parse(spaced).expect("parses")), spaced);
    assert_eq!(
        format(&parse(&format(&parse(spaced).expect("parses"))).expect("reparses")),
        spaced
    );

    let flush = "# forkfile: 0.1\ns3 assets {\n  policy public-read\n}\n";
    let expected = "# forkfile: 0.1\ns3 assets {\n    policy public-read\n}\n";
    assert_eq!(format(&parse(flush).expect("parses")), expected);
    assert_eq!(format(&parse(expected).expect("reparses")), expected);

    let commented = "# forkfile: 0.1\n\n# note\ndomain main\n";
    assert_eq!(format(&parse(commented).expect("parses")), commented);
}

#[test]
fn authored_blank_between_services_is_preserved_byte_for_byte() {
    let source = "# forkfile: 0.1\n\nimage app . {\n  service alpha {\n    run: one\n  }\n\n  service beta {\n    run: two\n  }\n}\n";
    let expected = "# forkfile: 0.1\n\nimage app . {\n    service alpha {\n        run: one\n    }\n\n    service beta {\n        run: two\n    }\n}\n";
    assert_eq!(format(&parse(source).expect("parses")), expected);
}

#[test]
fn port_only_services_and_leaf_items_keep_authored_blanks() {
    let source = "# forkfile: 0.1\n\nimage app . {\n  service alpha {\n    port 8000\n  }\n\n  service beta {\n    port 9000\n  }\n\n  stage prod\n}\n\nenv common {\n  A = \"1\"\n}\n";
    let expected = "# forkfile: 0.1\n\nimage app . {\n    service alpha {\n        port 8000\n    }\n\n    service beta {\n        port 9000\n    }\n\n    stage prod\n}\n\nenv common {\n    A = \"1\"\n}\n";
    assert_eq!(format(&parse(source).expect("parses")), expected);

    let leaves = "# forkfile: 0.1\n\nimage app . {\n  dockerfile Dockerfile.dev\n\n  workdir ./app\n\n  use env common\n}\n";
    let expected = "# forkfile: 0.1\n\nimage app . {\n    dockerfile Dockerfile.dev\n\n    workdir ./app\n\n    use env common\n}\n";
    assert_eq!(format(&parse(leaves).expect("parses")), expected);

    let service = "# forkfile: 0.1\n\nimage app . {\n  use env common\n\n  service web {\n    run: one\n  }\n}\n";
    let expected = "# forkfile: 0.1\n\nimage app . {\n    use env common\n\n    service web {\n        run: one\n    }\n}\n";
    assert_eq!(format(&parse(service).expect("parses")), expected);
}

#[test]
fn port_and_run_separated_by_a_blank_stay_separated() {
    let source = "# forkfile: 0.1\n\nimage app . {\n  service web {\n    port 8000\n\n    run: one\n  }\n}\n";
    let expected = "# forkfile: 0.1\n\nimage app . {\n    service web {\n        port 8000\n\n        run: one\n    }\n}\n";
    assert_eq!(format(&parse(source).expect("parses")), expected);
}

#[test]
fn run_block_bodies_keep_one_authored_blank() {
    let source = "# forkfile: 0.1\n\nimage app . {\n  service web {\n    run {\n      /bin/true\n\n      on dev: echo dev\n    }\n  }\n}\n";
    let expected = "# forkfile: 0.1\n\nimage app . {\n    service web {\n        run {\n            /bin/true\n\n            on dev: echo dev\n        }\n    }\n}\n";
    assert_eq!(format(&parse(source).expect("parses")), expected);
}

#[test]
fn authored_blank_between_env_entries_is_preserved_and_runs_collapse() {
    let spaced = "# forkfile: 0.1\n\nenv common {\n  A = \"1\"\n\n  B = \"2\"\n}\n";
    let expected = "# forkfile: 0.1\n\nenv common {\n    A = \"1\"\n\n    B = \"2\"\n}\n";
    assert_eq!(format(&parse(spaced).expect("parses")), expected);

    let run = "# forkfile: 0.1\n\nenv common {\n  A = \"1\"\n\n\n\n  B = \"2\"\n}\n";
    assert_eq!(format(&parse(run).expect("parses")), expected);
}

#[test]
fn adjacent_items_without_a_source_gap_get_no_blank() {
    let source = "# forkfile: 0.1\n\nenv common {\n  A = \"1\"\n  B = \"2\"\n}\n";
    let expected = "# forkfile: 0.1\n\nenv common {\n    A = \"1\"\n    B = \"2\"\n}\n";
    assert_eq!(format(&parse(source).expect("parses")), expected);

    let adjacent_top = "# forkfile: 0.1\n\ndomain a\ndomain b\n";
    assert_eq!(
        format(&parse(adjacent_top).expect("parses")),
        "# forkfile: 0.1\n\ndomain a\ndomain b\n"
    );
    let spaced_top = "# forkfile: 0.1\n\ndomain a\n\ndomain b\n";
    assert_eq!(
        format(&parse(spaced_top).expect("parses")),
        "# forkfile: 0.1\n\ndomain a\n\ndomain b\n"
    );
}

#[test]
fn comment_adjacency_and_authored_gaps_are_byte_exact() {
    let attached = "# forkfile: 0.1\n\nimage app . {\n  # the service\n  service api {\n    run: one\n  }\n}\n";
    let expected = "# forkfile: 0.1\n\nimage app . {\n    # the service\n    service api {\n        run: one\n    }\n}\n";
    assert_eq!(format(&parse(attached).expect("parses")), expected);

    let gapped = "# forkfile: 0.1\n\nimage app . {\n  # the service\n\n  service api {\n    run: one\n  }\n}\n";
    let expected = "# forkfile: 0.1\n\nimage app . {\n    # the service\n\n    service api {\n        run: one\n    }\n}\n";
    assert_eq!(format(&parse(gapped).expect("parses")), expected);

    let before_comment =
        "# forkfile: 0.1\n\nenv common {\n  A = \"1\"\n\n  # note\n  B = \"2\"\n}\n";
    let expected =
        "# forkfile: 0.1\n\nenv common {\n    A = \"1\"\n\n    # note\n    B = \"2\"\n}\n";
    assert_eq!(format(&parse(before_comment).expect("parses")), expected);
}

#[test]
fn trailing_blanks_inside_a_body_and_at_eof_are_removed() {
    let source = "# forkfile: 0.1\n\nenv common {\n  A = \"1\"\n\n\n}\n\n\n";
    assert_eq!(
        format(&parse(source).expect("parses")),
        "# forkfile: 0.1\n\nenv common {\n    A = \"1\"\n}\n"
    );
}

#[test]
fn demo_forkfile_round_trips_and_stays_clean_after_one_pass() {
    let once = format(&parse(DEMO).expect("demo parses"));
    assert_eq!(once, DEMO, "canonical demo must not churn");
    let twice = format(&parse(&once).expect("formatted demo parses"));
    assert_eq!(once, twice);
    let check = format_check(&once).expect("formatted demo parses");
    assert!(check.is_formatted(), "{}", check.diff);
}

#[test]
fn s3_bodies_honour_authored_blanks_through_the_policy_sort() {
    let canonical = "# forkfile: 0.1\n\ns3 assets {\n  policy public-read\n\n  cdn c1\n}\n";
    let expected = "# forkfile: 0.1\n\ns3 assets {\n    policy public-read\n\n    cdn c1\n}\n";
    assert_eq!(format(&parse(canonical).expect("parses")), expected);
    assert_eq!(format(&parse(expected).expect("reparses")), expected);

    let reorder = "# forkfile: 0.1\n\ns3 assets {\n  cdn c1\n\n  policy public-read\n}\n";
    let once = "# forkfile: 0.1\n\ns3 assets {\n    policy public-read\n    cdn c1\n}\n";
    assert_eq!(format(&parse(reorder).expect("parses")), once);
    let check = format_check(once).expect("canonical parses");
    assert!(check.is_formatted(), "{}", check.diff);
}

#[test]
fn run_block_keeps_blank_before_a_second_on_after_a_comment() {
    let source = "# forkfile: 0.1\n\nimage app . {\n    service w {\n        run {\n            /bin/true\n\n            # note\n            on dev: echo d\n\n            on prod: echo p\n        }\n    }\n}\n";
    assert_eq!(format(&parse(source).expect("parses")), source);
}

#[test]
fn consecutive_inline_on_commands_keep_exactly_one_authored_blank() {
    let spaced = "# forkfile: 0.1\n\nimage app . {\n    service w {\n        run {\n            on dev: echo d\n\n\n            on prod: echo p\n        }\n    }\n}\n";
    let expected = "# forkfile: 0.1\n\nimage app . {\n    service w {\n        run {\n            on dev: echo d\n\n            on prod: echo p\n        }\n    }\n}\n";
    assert_eq!(format(&parse(spaced).expect("parses")), expected);
    assert_eq!(format(&parse(expected).expect("reparses")), expected);

    let flush = "# forkfile: 0.1\n\nimage app . {\n    service w {\n        run {\n            on dev: echo d\n            on prod: echo p\n        }\n    }\n}\n";
    assert_eq!(format(&parse(flush).expect("parses")), flush);
}
