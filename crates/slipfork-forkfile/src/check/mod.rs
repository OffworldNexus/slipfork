//! Whole-file `Forkfile` semantics: all-branch `on` typing, build-time `arg`
//! secret rejection, and route/port rules over one flat symbol namespace.

mod expr;
mod policy;
mod services;
mod symbols;

use std::collections::{BTreeMap, BTreeSet};

use crate::ast::*;
use crate::boot::{PREQUEL_TOOLS, PrequelTool};
use crate::diag::{Diagnostic, Diagnostics};
use crate::expr::Command;
use crate::span::Span;

pub use symbols::{Symbol, SymbolKind, SymbolTable};

/// A document that passed whole-file static semantics.
#[derive(Debug, Clone)]
pub struct CheckedForkfile {
    /// The checked syntax tree every later stage evaluates.
    pub document: Box<Forkfile>,
    /// The flat symbol table, ready for evaluation by later stages.
    pub symbols: SymbolTable,
    /// Every declared feature name.
    pub features: BTreeSet<String>,
    /// Runtime `env` keys, including their declaration spans.
    pub env_keys: BTreeMap<String, Span>,
    /// Build-time `arg` keys, including their declaration spans.
    pub arg_keys: BTreeMap<String, Span>,
}

/// Type-checks every branch of a parsed document.
///
/// The symbol table and declaration map are returned even when checking fails,
/// so editors can keep resolving names in a document that has errors.
// The checked tree is deliberately returned on failure; boxing it would only move the allocation.
#[allow(clippy::result_large_err)]
pub fn check(doc: &Forkfile) -> Result<CheckedForkfile, (CheckedForkfile, Diagnostics)> {
    let features = declared_features(&doc.items.items);
    let mut env_keys = BTreeMap::new();
    let mut arg_keys = BTreeMap::new();
    collect_bindings(&doc.items.items, &mut env_keys, &mut arg_keys);
    let mut checker = Checker {
        diags: Diagnostics::new(),
    };
    let symbols = SymbolTable::from_document(doc, &mut checker.diags);
    let mut env_groups = BTreeSet::new();
    collect_env_names(&doc.items.items, &mut env_groups);
    for item in &doc.items.items {
        checker.check_top(item, &symbols, &features, &env_groups);
    }
    let checked = CheckedForkfile {
        document: Box::new(doc.clone()),
        symbols,
        features,
        env_keys,
        arg_keys,
    };
    if checker.diags.is_empty() {
        Ok(checked)
    } else {
        dedupe(&mut checker.diags);
        Err((checked, checker.diags))
    }
}

fn collect_bindings(
    items: &[TopItem],
    env_keys: &mut BTreeMap<String, Span>,
    arg_keys: &mut BTreeMap<String, Span>,
) {
    for item in items {
        match item {
            TopItem::Image(image) => {
                for item in &image.body.items {
                    collect_image_binding(item, env_keys, arg_keys);
                }
            }
            TopItem::On(on) => match &on.body {
                OnBody::Inline(inner) => {
                    collect_bindings(std::slice::from_ref(inner.as_ref()), env_keys, arg_keys)
                }
                OnBody::Block(block) => collect_bindings(&block.items, env_keys, arg_keys),
            },
            TopItem::Env(env) => collect_env_bindings(&env.entries.items, env_keys),
            TopItem::Features(_)
            | TopItem::Domain(_)
            | TopItem::Postgres(_)
            | TopItem::Redis(_)
            | TopItem::S3(_) => {}
        }
    }
}

fn collect_image_binding(
    item: &ImageItem,
    env_keys: &mut BTreeMap<String, Span>,
    arg_keys: &mut BTreeMap<String, Span>,
) {
    match item {
        ImageItem::Arg(arg) => {
            arg_keys.entry(arg.key.clone()).or_insert(arg.key_span);
        }
        ImageItem::ArgBlock(block) => {
            for arg in &block.args.items {
                arg_keys.entry(arg.key.clone()).or_insert(arg.key_span);
            }
        }
        ImageItem::Env(env) => collect_env_bindings(&env.entries.items, env_keys),
        ImageItem::On(on) => match &on.body {
            OnBody::Inline(inner) => collect_image_binding(inner, env_keys, arg_keys),
            OnBody::Block(block) => {
                for item in &block.items {
                    collect_image_binding(item, env_keys, arg_keys);
                }
            }
        },
        ImageItem::Dockerfile(_)
        | ImageItem::Stage(_)
        | ImageItem::Workdir(_)
        | ImageItem::UseEnv(_)
        | ImageItem::BeforeStart(_)
        | ImageItem::PreDeploy(_)
        | ImageItem::PostDeploy(_)
        | ImageItem::FailedDeploy(_)
        | ImageItem::Service(_) => {}
    }
}

fn collect_env_bindings(items: &[EnvItem], env_keys: &mut BTreeMap<String, Span>) {
    for item in items {
        match item {
            EnvItem::Entry(entry) => {
                env_keys.entry(entry.key.clone()).or_insert(entry.span);
            }
            EnvItem::On(on) => match &on.body {
                OnBody::Inline(inner) => {
                    collect_env_bindings(std::slice::from_ref(inner.as_ref()), env_keys);
                }
                OnBody::Block(block) => collect_env_bindings(&block.items, env_keys),
            },
        }
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
            TopItem::Features(_) => {}
            TopItem::Domain(domain) => policy::check_domain(domain, &mut self.diags),
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
            TopItem::S3(s3) => policy::check_s3(s3, features, &mut self.diags),
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
            | ImageItem::FailedDeploy(hook) => self.check_hook(hook, features),
            ImageItem::Service(service) => {
                services::check_service(service, symbols, &mut self.diags);
                self.check_service_items(&service.body.items, features);
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

    fn check_service_items(&mut self, items: &[ServiceItem], features: &BTreeSet<String>) {
        for item in items {
            match item {
                ServiceItem::BeforeStart(hook)
                | ServiceItem::PreDeploy(hook)
                | ServiceItem::PostDeploy(hook)
                | ServiceItem::FailedDeploy(hook)
                | ServiceItem::Run(hook) => self.check_hook(hook, features),
                ServiceItem::On(on) => match &on.body {
                    OnBody::Inline(inner) => {
                        self.check_service_items(std::slice::from_ref(inner.as_ref()), features);
                    }
                    OnBody::Block(block) => self.check_service_items(&block.items, features),
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

    fn check_hook(&mut self, hook: &Hook, features: &BTreeSet<String>) {
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
            HookBody::Block(block) => self.check_command_items(&block.items, features),
        }
    }

    /// Visits every command of a block, validating each nested `on` condition.
    fn check_command_items(&mut self, items: &[CommandItem], features: &BTreeSet<String>) {
        for item in items {
            match item {
                CommandItem::Command(command) => self.check_command(command),
                CommandItem::On(on) => {
                    expr::check_condition(&on.cond, features, &mut self.diags);
                    match &on.body {
                        OnBody::Inline(inner) => {
                            self.check_command_items(std::slice::from_ref(inner.as_ref()), features)
                        }
                        OnBody::Block(block) => self.check_command_items(&block.items, features),
                    }
                }
            }
        }
    }

    fn check_command(&mut self, command: &Command) {
        if let Some(tool) = &command.tool
            && PrequelTool::lookup(&tool.name).is_none()
        {
            let names = PREQUEL_TOOLS
                .iter()
                .map(|tool| format!("@{}", tool.name))
                .collect::<Vec<_>>()
                .join("`, `");
            self.diags.push(Diagnostic::new(
                command.span,
                format!("unknown `@{}` auto-prequel", tool.name),
                format!("use one of `{names}`"),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;

    #[test]
    fn every_on_branch_reports_taint_at_its_own_span() {
        let source = "# forkfile: 0.1\n\nfeatures {\n  dev\n}\n\nimage backend . {\n  on dev: arg A = secret(A)\n  on !dev: arg B = secret(B)\n}\n";
        let (_, diags) = check(&parse(source).expect("parses")).expect_err("both branches tainted");
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
        let (_, diags) = check(&parse(source).expect("parses")).expect_err("three diagnostics");
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
