//! Whole-file `Forkfile` semantics: all-branch `on` typing, build-time `arg`
//! secret rejection, and route/port rules over one flat symbol namespace.

mod expr;
mod services;
mod symbols;

use std::collections::BTreeSet;

use crate::ast::*;
use crate::diag::{Diagnostic, Diagnostics};
use crate::expr::Command;
use crate::span::Span;

pub use symbols::{Symbol, SymbolKind, SymbolTable};

/// Every tool that may prefix a command with `@`.
pub const AUTO_PREQUEL_TOOLS: &[&str] = &[
    "uv", "poetry", "pnpm", "npm", "yarn", "bun", "composer", "bundle", "go", "cargo",
];

/// A document that passed whole-file static semantics.
#[derive(Debug, Clone)]
pub struct CheckedForkfile {
    /// The flat symbol table, ready for evaluation by later stages.
    pub symbols: SymbolTable,
    /// Every declared feature name.
    pub features: BTreeSet<String>,
}

/// Type-checks every branch of a parsed document.
pub fn check(doc: &Forkfile) -> Result<CheckedForkfile, Diagnostics> {
    let features = declared_features(&doc.items.items);
    let mut checker = Checker {
        diags: Diagnostics::new(),
    };
    let symbols = SymbolTable::from_document(doc, &mut checker.diags);
    let mut env_groups = BTreeSet::new();
    collect_env_names(&doc.items.items, &mut env_groups);
    for item in &doc.items.items {
        checker.check_top(item, &symbols, &features, &env_groups);
    }
    if checker.diags.is_empty() {
        Ok(CheckedForkfile { symbols, features })
    } else {
        dedupe(&mut checker.diags);
        Err(checker.diags)
    }
}

fn declared_features(items: &[TopItem]) -> BTreeSet<String> {
    let mut features = BTreeSet::new();
    for item in items {
        match item {
            TopItem::Features(block) => {
                for group in &block.groups.items {
                    features.insert(group.name.clone());
                    if let Some(list) = &group.features {
                        features.extend(list.iter().cloned());
                    }
                }
            }
            TopItem::On(on) => match &on.body {
                OnBody::Inline(inner) => {
                    features.extend(declared_features(std::slice::from_ref(inner.as_ref())));
                }
                OnBody::Block(block) => features.extend(declared_features(&block.items)),
            },
            _ => {}
        }
    }
    features
}

fn collect_env_names(items: &[TopItem], out: &mut BTreeSet<String>) {
    for item in items {
        match item {
            TopItem::Env(env) => {
                out.insert(env.group.clone());
            }
            TopItem::On(on) => match &on.body {
                OnBody::Inline(inner) => {
                    collect_env_names(std::slice::from_ref(inner.as_ref()), out);
                }
                OnBody::Block(block) => collect_env_names(&block.items, out),
            },
            _ => {}
        }
    }
}

fn dedupe(diags: &mut Diagnostics) {
    let mut seen = BTreeSet::new();
    diags.items.retain(|diagnostic| {
        seen.insert((
            diagnostic.span.line(),
            diagnostic.span.col(),
            diagnostic.message.clone(),
        ))
    });
}

struct Checker {
    diags: Diagnostics,
}

impl Checker {
    fn check_top(
        &mut self,
        item: &TopItem,
        symbols: &SymbolTable,
        features: &BTreeSet<String>,
        env_groups: &BTreeSet<String>,
    ) {
        match item {
            TopItem::Features(_) | TopItem::Domain(_) => {}
            TopItem::Image(image) => {
                for item in &image.body.items {
                    self.check_image_item(item, symbols, features, env_groups, image.span);
                }
            }
            TopItem::On(on) => {
                expr::check_condition(&on.cond, features, &mut self.diags);
                match &on.body {
                    OnBody::Inline(inner) => {
                        self.check_top(inner, symbols, features, env_groups);
                    }
                    OnBody::Block(block) => {
                        for item in &block.items {
                            self.check_top(item, symbols, features, env_groups);
                        }
                    }
                }
            }
            TopItem::Postgres(postgres) => {
                self.check_postgres_items(&postgres.body.items, features);
            }
            TopItem::Redis(redis) => self.check_redis_items(&redis.body.items, features),
            TopItem::S3(s3) => self.check_s3_items(&s3.body.items, features),
            TopItem::Env(env) => {
                for item in &env.entries.items {
                    self.check_env_item(item, symbols, features);
                }
            }
        }
    }

    fn check_image_item(
        &mut self,
        item: &ImageItem,
        symbols: &SymbolTable,
        features: &BTreeSet<String>,
        env_groups: &BTreeSet<String>,
        owner: Span,
    ) {
        match item {
            ImageItem::Arg(arg) => {
                expr::check_value(&arg.value, expr::ValueMode::Build, symbols, &mut self.diags);
            }
            ImageItem::ArgBlock(block) => {
                for arg in &block.args.items {
                    expr::check_value(&arg.value, expr::ValueMode::Build, symbols, &mut self.diags);
                }
            }
            ImageItem::Env(env) => {
                for entry in &env.entries.items {
                    self.check_env_item(entry, symbols, features);
                }
            }
            ImageItem::UseEnv(group) => self.check_env_group(owner, group, env_groups),
            ImageItem::BeforeStart(hook)
            | ImageItem::PreDeploy(hook)
            | ImageItem::PostDeploy(hook)
            | ImageItem::FailedDeploy(hook) => self.check_hook(hook),
            ImageItem::Service(service) => {
                services::check_service(service, symbols, &mut self.diags);
                self.check_service_items(&service.body.items);
            }
            ImageItem::On(on) => {
                expr::check_condition(&on.cond, features, &mut self.diags);
                match &on.body {
                    OnBody::Inline(inner) => {
                        self.check_image_item(inner, symbols, features, env_groups, owner);
                    }
                    OnBody::Block(block) => {
                        for item in &block.items {
                            self.check_image_item(item, symbols, features, env_groups, owner);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn check_service_items(&mut self, items: &[ServiceItem]) {
        for item in items {
            match item {
                ServiceItem::BeforeStart(hook)
                | ServiceItem::PreDeploy(hook)
                | ServiceItem::PostDeploy(hook)
                | ServiceItem::FailedDeploy(hook)
                | ServiceItem::Run(hook) => self.check_hook(hook),
                ServiceItem::On(on) => match &on.body {
                    OnBody::Inline(inner) => {
                        self.check_service_items(std::slice::from_ref(inner.as_ref()));
                    }
                    OnBody::Block(block) => self.check_service_items(&block.items),
                },
                _ => {}
            }
        }
    }

    fn check_env_item(
        &mut self,
        item: &EnvItem,
        symbols: &SymbolTable,
        features: &BTreeSet<String>,
    ) {
        match item {
            EnvItem::Entry(entry) => {
                expr::check_value(
                    &entry.value,
                    expr::ValueMode::Runtime,
                    symbols,
                    &mut self.diags,
                );
            }
            EnvItem::On(on) => {
                expr::check_condition(&on.cond, features, &mut self.diags);
                match &on.body {
                    OnBody::Inline(inner) => self.check_env_item(inner, symbols, features),
                    OnBody::Block(block) => {
                        for item in &block.items {
                            self.check_env_item(item, symbols, features);
                        }
                    }
                }
            }
        }
    }

    fn check_env_group(&mut self, span: Span, group: &str, env_groups: &BTreeSet<String>) {
        if !env_groups.contains(group) {
            self.diags.push(Diagnostic::new(
                span,
                format!("unknown env group `{group}`"),
                format!("declare it with `env {group} {{ ... }}`"),
            ));
        }
    }

    fn check_hook(&mut self, hook: &Hook) {
        if let Some(paths) = &hook.when_changed
            && paths.is_empty()
        {
            self.diags.push(Diagnostic::new(
                hook.span,
                "`when changed(...)` needs at least one path",
                "write `when changed(prisma/schema.prisma)`",
            ));
        }
        match &hook.body {
            HookBody::Inline(command) => self.check_command(command),
            HookBody::Block(block) => {
                for command in &block.items {
                    self.check_command(command);
                }
            }
        }
    }

    fn check_command(&mut self, command: &Command) {
        if let Some(tool) = &command.tool
            && !AUTO_PREQUEL_TOOLS.contains(&tool.as_str())
        {
            self.diags.push(Diagnostic::new(
                command.span,
                format!("unknown `@{tool}` auto-prequel"),
                format!("use one of `@{}`", AUTO_PREQUEL_TOOLS.join("`, `@")),
            ));
        }
    }

    fn check_postgres_items(&mut self, items: &[PostgresItem], features: &BTreeSet<String>) {
        for item in items {
            if let PostgresItem::On(on) = item {
                expr::check_condition(&on.cond, features, &mut self.diags);
                match &on.body {
                    OnBody::Inline(inner) => {
                        self.check_postgres_items(std::slice::from_ref(inner.as_ref()), features);
                    }
                    OnBody::Block(block) => self.check_postgres_items(&block.items, features),
                }
            }
        }
    }

    fn check_redis_items(&mut self, items: &[RedisItem], features: &BTreeSet<String>) {
        for item in items {
            if let RedisItem::On(on) = item {
                expr::check_condition(&on.cond, features, &mut self.diags);
                match &on.body {
                    OnBody::Inline(inner) => {
                        self.check_redis_items(std::slice::from_ref(inner.as_ref()), features);
                    }
                    OnBody::Block(block) => self.check_redis_items(&block.items, features),
                }
            }
        }
    }

    fn check_s3_items(&mut self, items: &[S3Item], features: &BTreeSet<String>) {
        for item in items {
            if let S3Item::On(on) = item {
                expr::check_condition(&on.cond, features, &mut self.diags);
                match &on.body {
                    OnBody::Inline(inner) => {
                        self.check_s3_items(std::slice::from_ref(inner.as_ref()), features);
                    }
                    OnBody::Block(block) => self.check_s3_items(&block.items, features),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;

    #[test]
    fn every_on_branch_reports_taint_at_its_own_span() {
        let source = "# forkfile: 0.1\n\nfeatures {\n  dev\n}\n\nimage backend . {\n  on dev: arg A = secret(A)\n  on !dev: arg B = secret(B)\n}\n";
        let diags = check(&parse(source).expect("parses")).expect_err("both branches tainted");
        let tainted: Vec<&Diagnostic> = diags
            .items
            .iter()
            .filter(|diagnostic| diagnostic.message.contains("bake a secret"))
            .collect();
        assert_eq!(tainted.len(), 2, "{diags:?}");
        assert_eq!(tainted[0].span.line(), 8);
        assert_eq!(tainted[1].span.line(), 9);
    }

    #[test]
    fn tools_changed_patterns_and_env_groups_have_exact_remedies() {
        let source = "# forkfile: 0.1\n\nimage backend . {\n  before-start when changed(): @python run\n  use env nope\n}\n";
        let diags = check(&parse(source).expect("parses")).expect_err("three diagnostics");
        let tool = diags
            .items
            .iter()
            .find(|diagnostic| diagnostic.message.contains("unknown `@python`"))
            .expect("tool diagnostic");
        assert_eq!(
            tool.next,
            "use one of `@uv`, `@poetry`, `@pnpm`, `@npm`, `@yarn`, `@bun`, `@composer`, `@bundle`, `@go`, `@cargo`"
        );
        let changed = diags
            .items
            .iter()
            .find(|diagnostic| diagnostic.message.contains("when changed(...)"))
            .expect("changed diagnostic");
        assert_eq!(changed.next, "write `when changed(prisma/schema.prisma)`");
        let env = diags
            .items
            .iter()
            .find(|diagnostic| diagnostic.message.contains("unknown env group `nope`"))
            .expect("env diagnostic");
        assert_eq!(env.next, "declare it with `env nope { ... }`");
    }
}
