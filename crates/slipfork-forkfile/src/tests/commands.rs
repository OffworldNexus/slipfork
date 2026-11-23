use super::*;

const DEV_SOURCE: &str = r#"# forkfile: 0.1

features {
  dev
  prod
  default = [dev]
}

image front ./web {
  before-start when changed(package.json, pnpm-lock.yaml) {
    pnpm install
    on !dev: pnpm run build
  }

  service front {
    port 5173
    run {
      on dev: @pnpm exec vite dev --host 0.0.0.0 --port $port
      on !dev {
        pnpm install
        node build
      }
    }
  }
}
"#;

const NESTED_SOURCE: &str = r#"# forkfile: 0.1

features {
  dev
  prod
  default = [dev, prod]
}

image backend . {
  service api {
    run {
      on dev {
        pnpm install
        on prod: node build
      }
      on prod: node serve
    }
  }
}
"#;

fn first_image(doc: &Forkfile) -> &Image {
    doc.items
        .items
        .iter()
        .find_map(|item| match item {
            TopItem::Image(image) => Some(image),
            _ => None,
        })
        .expect("expected an image")
}

fn first_service(image: &Image) -> &Service {
    image
        .body
        .items
        .iter()
        .find_map(|item| match item {
            ImageItem::Service(service) => Some(service),
            _ => None,
        })
        .expect("expected a service")
}

fn run_hook(doc: &Forkfile) -> &Hook {
    first_service(first_image(doc))
        .body
        .items
        .iter()
        .find_map(|item| match item {
            ServiceItem::Run(hook) => Some(hook),
            _ => None,
        })
        .expect("expected a run hook")
}

fn before_start_hook(doc: &Forkfile) -> &Hook {
    first_image(doc)
        .body
        .items
        .iter()
        .find_map(|item| match item {
            ImageItem::BeforeStart(hook) => Some(hook),
            _ => None,
        })
        .expect("expected a before-start hook")
}

fn texts(hook: &Hook, features: &FeatureSet) -> Vec<String> {
    hook.commands(features)
        .iter()
        .map(|command| command.text())
        .collect()
}

#[test]
fn nested_on_commands_round_trip_and_format_idempotently() {
    let doc = parse(DEV_SOURCE).expect("nested `on` commands parse");
    let formatted = format(&doc);
    assert!(
        formatted.contains("on dev: @pnpm exec vite dev --host 0.0.0.0 --port $port"),
        "{formatted}"
    );
    assert!(formatted.contains("on !dev {"), "{formatted}");
    assert!(formatted.contains("on !dev: pnpm run build"), "{formatted}");
    assert_eq!(doc, parse(&formatted).expect("formatted output reparses"));
    assert_eq!(formatted, format(&parse(&formatted).expect("reparses")));
}

#[test]
fn only_active_command_branches_reach_the_boot_plan() {
    let doc = parse(DEV_SOURCE).expect("parses");
    let dev = doc.active_features();
    assert_eq!(
        texts(run_hook(&doc), &dev),
        ["@pnpm exec vite dev --host 0.0.0.0 --port $port"]
    );
    assert_eq!(texts(before_start_hook(&doc), &dev), ["pnpm install"]);

    let flipped = parse(&DEV_SOURCE.replace("default = [dev]", "default = [prod]"))
        .expect("flipped features parse");
    let prod = flipped
        .resolve_features(true, &[FeatureEdit::Enable("prod".into())])
        .expect("`prod` alone resolves");
    assert!(!prod.contains("dev"), "no-default-features turns `dev` off");
    assert_eq!(
        texts(run_hook(&flipped), &prod),
        ["pnpm install", "node build"]
    );
    assert_eq!(
        texts(before_start_hook(&flipped), &prod),
        ["pnpm install", "pnpm run build"]
    );
}

#[test]
fn nested_on_blocks_flatten_recursively_in_declaration_order() {
    let doc = parse(NESTED_SOURCE).expect("parses");
    assert_eq!(
        texts(run_hook(&doc), &doc.active_features()),
        ["pnpm install", "node build", "node serve"]
    );
    let dev_only = parse(&NESTED_SOURCE.replace("[dev, prod]", "[dev]")).expect("parses");
    assert_eq!(
        texts(run_hook(&dev_only), &dev_only.active_features()),
        ["pnpm install"]
    );
}

#[test]
fn an_unknown_feature_inside_a_command_block_is_a_diagnostic() {
    let source = "# forkfile: 0.1\n\nfeatures {\n  dev\n}\n\nimage backend . {\n  service api {\n    run {\n      on ghost: pnpm run build\n    }\n  }\n}\n";
    let doc = parse(source).expect("parses");
    let (_, diags) = check(&doc).expect_err("unknown feature in a guarded command");
    let diagnostic = diags
        .items
        .iter()
        .find(|diagnostic| diagnostic.message.contains("unknown feature `ghost`"))
        .expect("feature diagnostic");
    assert_eq!((diagnostic.span.line(), diagnostic.span.col()), (10, 10));
}

#[test]
fn a_command_starting_with_on_stays_a_command() {
    let source = "# forkfile: 0.1\n\nimage backend . {\n  service api {\n    run {\n      on failure ./rollback.sh\n    }\n  }\n}\n";
    let doc = parse(source).expect("parses");
    let HookBody::Block(block) = &run_hook(&doc).body else {
        panic!("expected a command block");
    };
    assert_eq!(block.items.len(), 1, "{:?}", block.items);
    match &block.items[0] {
        CommandItem::Command(command) => assert_eq!(command.text(), "on failure ./rollback.sh"),
        CommandItem::On(_) => panic!("a bare `on` command must not become a guard"),
    }
}
