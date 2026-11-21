use super::*;

#[test]
fn declaration_and_expression_nodes_carry_spans() {
    let doc = parse(SAMPLE).expect("sample parses");
    assert_eq!(doc.span.line(), 1);
    let TopItem::Features(features) = &doc.items.items[0] else {
        panic!("expected features");
    };
    assert_eq!(
        (features.span.line(), features.groups.items[0].span.line()),
        (3, 4)
    );
    let image = first_image(&doc);
    assert_eq!(image.span.line(), 25);
    let service = first_service(image);
    assert_eq!(
        (service.span.line(), service_routes(service)[0].span.line()),
        (29, 31)
    );
    let env = doc
        .items
        .items
        .iter()
        .find_map(|item| match item {
            TopItem::Env(env) => Some(env),
            _ => None,
        })
        .expect("expected env");
    let EnvItem::Entry(entry) = &env.entries.items[0] else {
        panic!("expected an env entry");
    };
    assert_eq!((entry.span.line(), entry.value.span.line()), (16, 16));
}
#[test]
fn missing_header_is_reported_at_line_one() {
    let rendered = expect_render("domain main\n");
    assert!(rendered.contains("Forkfile:1:1"), "{rendered}");
    assert!(
        rendered.contains("missing `# forkfile: 0.1` header"),
        "{rendered}"
    );
    assert!(
        rendered.contains("next: add `# forkfile: 0.1` as line 1"),
        "{rendered}"
    );
}
#[test]
fn wrong_header_version_is_reported_with_a_caret() {
    let rendered = expect_render("# forkfile: 0.2\n\ndomain main\n");
    assert!(rendered.contains("Forkfile:1:1"), "{rendered}");
    assert!(
        rendered.contains("unsupported `Forkfile` format `0.2`"),
        "{rendered}"
    );
    assert!(
        rendered.contains("next: line 1 must read `# forkfile: 0.1`"),
        "{rendered}"
    );
    assert!(rendered.lines().any(|line| line == "^"), "{rendered}");
}
#[test]
fn unterminated_string_points_at_the_opening_quote() {
    let source = "# forkfile: 0.1\n\nenv common {\n  SECRET = \"oops\n}\n";
    let rendered = expect_render(source);
    assert!(
        rendered.contains("unterminated string literal"),
        "{rendered}"
    );
    assert!(rendered.contains("Forkfile:4:12"), "{rendered}");
    let caret = format!("{}^", " ".repeat(11));
    assert!(rendered.lines().any(|line| line == caret), "{rendered}");
}
#[test]
fn unknown_property_caret_lands_on_the_property() {
    let source = "# forkfile: 0.1\n\nimage backend . {\n  bogus thing\n}\n";
    let rendered = expect_render(source);
    assert!(
        rendered.contains("unknown image property `bogus`"),
        "{rendered}"
    );
    assert!(rendered.contains("Forkfile:4:3"), "{rendered}");
    let caret = format!("{}^", " ".repeat(2));
    assert!(rendered.lines().any(|line| line == caret), "{rendered}");
}
#[test]
fn missing_image_dir_uses_the_specific_remedy() {
    let rendered = expect_render("# forkfile: 0.1\n\nimage backend {\n}\n");
    assert!(
        rendered.contains("expected an image directory or `from <ref>`"),
        "{rendered}"
    );
    assert!(
        rendered.contains("write `image <id> <dir>` or `image <id> from <ref>`"),
        "{rendered}"
    );
}

#[test]
fn on_precedence_is_not_then_and_then_or() {
    let source =
        "# forkfile: 0.1\n\nimage backend . {\n  on !dev & mail | staging: stage prod\n}\n";
    let doc = parse(source).expect("parses");
    let on = first_image(&doc)
        .body
        .items
        .iter()
        .find_map(|item| match item {
            ImageItem::On(on) => Some(on),
            _ => None,
        })
        .expect("expected an on block");
    match &on.cond.kind {
        ExprKind::Or(left, right) => {
            assert!(matches!(&right.kind, ExprKind::Feature(name) if name == "staging"));
            match &left.kind {
                ExprKind::And(first, second) => {
                    assert!(matches!(
                        &first.kind,
                        ExprKind::Not(inner)
                            if matches!(&inner.kind, ExprKind::Feature(name) if name == "dev")
                    ));
                    assert!(matches!(&second.kind, ExprKind::Feature(name) if name == "mail"));
                }
                other => panic!("expected `&` on the left, found {other:?}"),
            }
        }
        other => panic!("expected `|` at the top, found {other:?}"),
    }
}

#[test]
fn route_kinds_strip_prefix_preserve_exact_and_rewrite() {
    let source = "# forkfile: 0.1\n\nimage backend . {\n  service api {\n    port 8000\n    route /api strip-prefix\n    route /api\n    route exact /healthz\n    route /api rewrite /v1\n  }\n}\n";
    let doc = parse(source).expect("parses");
    let routes = service_routes(first_service(first_image(&doc)));
    assert_eq!(routes.len(), 4);
    assert_eq!(routes[0].path, Some("/api".to_string()));
    assert_eq!(routes[0].kind, RouteKind::StripPrefix);
    assert_eq!(routes[1].kind, RouteKind::Preserve);
    assert_eq!(routes[2].path, None);
    assert_eq!(routes[2].kind, RouteKind::Exact("/healthz".to_string()));
    assert_eq!(routes[3].kind, RouteKind::Rewrite("/v1".to_string()));
}

#[test]
fn s3_cdn_child_resource_is_parsed() {
    let source = "# forkfile: 0.1\n\ns3 assets { cdn static_cdn }\n";
    let doc = parse(source).expect("parses");
    let TopItem::S3(bucket) = &doc.items.items[0] else {
        panic!("expected an s3 bucket");
    };
    assert_eq!(bucket.bucket, "assets");
    let Some(S3Item::Cdn(cdn)) = bucket.body.items.first() else {
        panic!("expected a cdn");
    };
    assert_eq!(cdn.id, "static_cdn");
}

#[test]
fn nested_on_block_guards_service_items() {
    let source = "# forkfile: 0.1\n\nimage backend . {\n  service api {\n    on dev {\n      port 5173\n      run: serve --port $port\n    }\n  }\n}\n";
    let doc = parse(source).expect("parses");
    let service = first_service(first_image(&doc));
    let Some(ServiceItem::On(on)) = service.body.items.first() else {
        panic!("expected an on block");
    };
    let OnBody::Block(block) = &on.body else {
        panic!("expected an on block body");
    };
    assert_eq!(block.items.len(), 2);
    assert!(matches!(block.items[0], ServiceItem::Port(5173)));
    assert!(
        matches!(&block.items[1], ServiceItem::Run(hook) if matches!(hook.body, HookBody::Inline(_)))
    );
    assert!(matches!(&on.cond.kind, ExprKind::Feature(name) if name == "dev"));
}

#[test]
fn before_start_changed_patterns_and_run_block_hook() {
    let source = "# forkfile: 0.1\n\nimage backend . {\n  service api {\n    before-start when changed(prisma/schema.prisma, package.json): @pnpm exec prisma generate\n    run {\n      echo one\n      echo two\n    }\n  }\n}\n";
    let doc = parse(source).expect("parses");
    let service = first_service(first_image(&doc));
    let before = service
        .body
        .items
        .iter()
        .find_map(|item| match item {
            ServiceItem::BeforeStart(hook) => Some(hook),
            _ => None,
        })
        .expect("expected before-start");
    assert_eq!(
        before.when_changed,
        Some(vec![
            "prisma/schema.prisma".to_string(),
            "package.json".to_string()
        ])
    );
    assert!(matches!(before.body, HookBody::Inline(_)));
    let run = service
        .body
        .items
        .iter()
        .find_map(|item| match item {
            ServiceItem::Run(hook) => Some(hook),
            _ => None,
        })
        .expect("expected run");
    let HookBody::Block(block) = &run.body else {
        panic!("expected a run block");
    };
    assert_eq!(block.items.len(), 2);
    let formatted = format(&doc);
    assert_eq!(formatted, format(&parse(&formatted).expect("reparses")));
    assert_eq!(doc, parse(&formatted).expect("reparses"));
}

#[test]
fn image_knobs_parse_and_round_trip() {
    let source = "# forkfile: 0.1\n\nimage backend . {\n  dockerfile Dockerfile.dev\n  workdir ./app\n  arg {\n    A = var(KEY) ?? \"x\"\n    B = random_base64(16)\n    C = random_alnum(8)\n    D = random_uuid()\n  }\n  env {\n    E = secret(S)\n  }\n}\n";
    let doc = parse(source).expect("parses");
    let image = first_image(&doc);
    assert!(
        image
            .body
            .items
            .iter()
            .any(|item| matches!(item, ImageItem::Dockerfile(path) if path == "Dockerfile.dev"))
    );
    assert!(
        image
            .body
            .items
            .iter()
            .any(|item| matches!(item, ImageItem::Workdir(path) if path == "./app"))
    );
    let args = image
        .body
        .items
        .iter()
        .find_map(|item| match item {
            ImageItem::ArgBlock(block) => Some(block),
            _ => None,
        })
        .expect("expected an arg block");
    assert_eq!(args.args.items.len(), 4);
    assert!(matches!(
        &args.args.items[0].value.kind,
        ExprKind::Fallback(..)
    ));
    assert!(matches!(
        &args.args.items[1].value.kind,
        ExprKind::Random { kind, n } if *kind == RandomKind::Base64 && *n == Some(16)
    ));
    assert!(matches!(
        &args.args.items[2].value.kind,
        ExprKind::Random { kind, n } if *kind == RandomKind::Alnum && *n == Some(8)
    ));
    assert!(matches!(
        &args.args.items[3].value.kind,
        ExprKind::Random {
            kind: RandomKind::Uuid,
            n: None
        }
    ));
    assert!(
        image
            .body
            .items
            .iter()
            .any(|item| matches!(item, ImageItem::Env(_)))
    );
    let formatted = format(&doc);
    assert!(
        formatted.contains("dockerfile Dockerfile.dev"),
        "{formatted}"
    );
    assert!(formatted.contains("workdir ./app"), "{formatted}");
    assert!(formatted.contains("random_uuid()"), "{formatted}");
    assert_eq!(doc, parse(&formatted).expect("reparses"));
}

#[test]
fn recovery_stops_at_the_end_of_the_bad_line() {
    let source =
        "# forkfile: 0.1\n\nimage backend . {\n  stage\n  service api {\n    port 8000\n  }\n}\n";
    let diagnostics = parse(source).expect_err("expected one diagnostic");
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert!(
        diagnostics.items[0].message.contains("a stage name"),
        "{diagnostics:?}"
    );
}

#[test]
fn internal_ports_and_rewrite_routes_parse() {
    let source = "# forkfile: 0.1\n\nimage backend . {\n  service api {\n    port 8000\n    internal_ports [8081, 8082]\n    route /api rewrite /v1 domain admin\n  }\n}\n";
    let doc = parse(source).expect("parses");
    let service = first_service(first_image(&doc));
    assert!(
        matches!(service.body.items[1], ServiceItem::InternalPorts(ref ports) if ports == &[8081, 8082])
    );
    let ServiceItem::Route(route) = &service.body.items[2] else {
        panic!("expected a route");
    };
    assert_eq!(route.kind, RouteKind::Rewrite("/v1".to_string()));
    assert_eq!(
        route.domain,
        Some(DomainFilter::Domain("admin".to_string()))
    );
}

#[test]
fn conflicting_route_modifiers_report_the_second_modifier() {
    let source = "# forkfile: 0.1\n\nimage backend . {\n  service api {\n    port 8000\n    route /api strip-prefix rewrite /v1\n  }\n}\n";
    let diagnostics = parse(source).expect_err("conflicting modifiers");
    let first = diagnostics.first().expect("one diagnostic");
    assert_eq!(first.message, "duplicate route modifier");
    assert_eq!(
        first.next,
        "keep only one of `strip-prefix`, `rewrite`, or `exact`"
    );
    assert_eq!(first.span.line(), 6);
}
