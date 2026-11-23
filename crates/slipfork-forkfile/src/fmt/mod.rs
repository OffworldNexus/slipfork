//! Canonical, comment-preserving rendering for `Forkfile` documents.

use crate::ast::*;
mod expr;
mod render;

use expr::{quote, render_expr};
use render::Renderer;

/// Result of comparing a source document against its canonical rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatCheck {
    /// The canonical rendering.
    pub formatted: String,
    /// A line diff from the original source to the canonical rendering.
    pub diff: String,
}

impl FormatCheck {
    /// Whether the source is already canonically formatted.
    #[must_use]
    pub fn is_formatted(&self) -> bool {
        self.diff.is_empty()
    }
}

/// Line diff from `original` to `formatted`, headed by the real `label`.
#[must_use]
pub fn render_diff(original: &str, formatted: &str, label: &str) -> String {
    crate::diff::line_diff(original, formatted, label)
}

/// Renders a parsed document canonically: 2-space indents, one blank line
/// between top-level blocks, comments and declaration order preserved.
#[must_use]
pub fn format(doc: &Forkfile) -> String {
    let mut renderer = Renderer::new();
    renderer.line(&format!("# forkfile: {}", doc.version));
    let block = &doc.items;
    for index in 0..=block.items.len() {
        let leading: Vec<&str> = block
            .comments
            .iter()
            .filter(|comment| comment.index == index && !comment.trailing)
            .map(|comment| comment.text.as_str())
            .collect();
        let trailing: Vec<&str> = block
            .comments
            .iter()
            .filter(|comment| comment.index == index && comment.trailing)
            .map(|comment| comment.text.as_str())
            .collect();
        if index < block.items.len() {
            if index > 0 {
                renderer.blank();
            }
            for comment in &leading {
                renderer.line(comment);
            }
            render_top(&mut renderer, &block.items[index]);
            for comment in &trailing {
                renderer.append(comment);
            }
        } else {
            if !leading.is_empty() {
                renderer.blank();
                for comment in &leading {
                    renderer.line(comment);
                }
            }
            for comment in &trailing {
                renderer.line(comment);
            }
        }
    }
    renderer.finish()
}

fn render_block<T>(
    renderer: &mut Renderer,
    block: &Block<T>,
    mut item: impl FnMut(&mut Renderer, &T),
) {
    for index in 0..=block.items.len() {
        for comment in block
            .comments
            .iter()
            .filter(|comment| comment.index == index && !comment.trailing)
        {
            renderer.line(&comment.text);
        }
        if let Some(node) = block.items.get(index) {
            item(renderer, node);
            for comment in block
                .comments
                .iter()
                .filter(|comment| comment.index == index && comment.trailing)
            {
                renderer.append(&comment.text);
            }
        } else {
            for comment in block
                .comments
                .iter()
                .filter(|comment| comment.index == index && comment.trailing)
            {
                renderer.line(&comment.text);
            }
        }
    }
}

fn render_on<I>(renderer: &mut Renderer, on: &On<I>, mut item: impl FnMut(&mut Renderer, &I)) {
    match &on.body {
        OnBody::Inline(node) => {
            let lines = inline_lines(&mut item, node);
            // An inline `on <expr>: <item>` that carries an `else` is canonicalized to the
            // block form, so the else branch can never be mistaken for a sibling item.
            if lines.len() > 1 || on.else_body.is_some() {
                renderer.line(&format!("on {} {{", render_expr(&on.cond)));
                renderer.indented(|renderer| {
                    for line in &lines {
                        renderer.line(line);
                    }
                });
                render_else_after_block(renderer, on.else_body.as_ref(), &mut item);
                return;
            }
            let line = format!("on {}: {}", render_expr(&on.cond), lines[0]);
            renderer.line(&line);
        }
        OnBody::Block(block) => {
            renderer.line(&format!("on {} {{", render_expr(&on.cond)));
            renderer.indented(|renderer| render_block(renderer, block, &mut item));
            render_else_after_block(renderer, on.else_body.as_ref(), &mut item);
        }
    }
}

/// Every rendered line of an inline item; a multi-line item falls back to block form.
fn inline_lines<I>(item: &mut impl FnMut(&mut Renderer, &I), node: &I) -> Vec<String> {
    let mut temporary = Renderer::new();
    item(&mut temporary, node);
    temporary
        .finish()
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>()
}

/// Closes a block-bodied `on`, rendering `} else {`, `} else on … {`, or `} else: <item>`.
fn render_else_after_block<I>(
    renderer: &mut Renderer,
    else_body: Option<&OnElse<I>>,
    item: &mut impl FnMut(&mut Renderer, &I),
) {
    match else_body {
        None => renderer.line("}"),
        Some(OnElse::Body(OnBody::Block(block))) => {
            renderer.line("} else {");
            renderer.indented(|renderer| render_block(renderer, block, &mut *item));
            renderer.line("}");
        }
        Some(OnElse::Body(OnBody::Inline(node))) => {
            let lines = inline_lines(&mut *item, node);
            if lines.len() > 1 {
                renderer.line("} else {");
                renderer.indented(|renderer| {
                    for line in &lines {
                        renderer.line(line);
                    }
                });
                renderer.line("}");
            } else {
                renderer.line(&format!("}} else: {}", lines[0]));
            }
        }
        Some(OnElse::Chain(on)) => match &on.body {
            OnBody::Block(block) => {
                renderer.line(&format!("}} else on {} {{", render_expr(&on.cond)));
                renderer.indented(|renderer| render_block(renderer, block, &mut *item));
                render_else_after_block(renderer, on.else_body.as_ref(), item);
            }
            OnBody::Inline(node) => {
                let lines = inline_lines(&mut *item, node);
                renderer.line(&format!("}} else on {} {{", render_expr(&on.cond)));
                renderer.indented(|renderer| {
                    for line in &lines {
                        renderer.line(line);
                    }
                });
                render_else_after_block(renderer, on.else_body.as_ref(), &mut *item);
            }
        },
    }
}

fn render_top(renderer: &mut Renderer, item: &TopItem) {
    match item {
        TopItem::Features(features) => {
            renderer.line("features {");
            renderer.indented(|renderer| {
                render_block(renderer, &features.groups, |renderer, group| {
                    let line = match &group.features {
                        Some(list) => format!("{} = [{}]", group.name, list.join(", ")),
                        None => group.name.clone(),
                    };
                    renderer.line(&line);
                });
            });
            renderer.line("}");
        }
        TopItem::Image(image) => render_image(renderer, image),
        TopItem::On(on) => render_on(renderer, on, render_top),
        TopItem::Domain(domain) => renderer.line(&format!("domain {}", domain.id)),
        TopItem::Postgres(postgres) => {
            if postgres.body.items.is_empty() && postgres.body.comments.is_empty() {
                renderer.line(&format!("postgres {}", postgres.id));
            } else {
                renderer.line(&format!("postgres {} {{", postgres.id));
                renderer.indented(|renderer| {
                    render_block(renderer, &postgres.body, render_postgres_item);
                });
                renderer.line("}");
            }
        }
        TopItem::Redis(redis) => {
            if redis.body.items.is_empty() && redis.body.comments.is_empty() {
                renderer.line(&format!("redis {}", redis.id));
            } else {
                renderer.line(&format!("redis {} {{", redis.id));
                renderer
                    .indented(|renderer| render_block(renderer, &redis.body, render_redis_item));
                renderer.line("}");
            }
        }
        TopItem::S3(s3) => {
            if s3.body.items.is_empty() && s3.body.comments.is_empty() {
                renderer.line(&format!("s3 {}", s3.bucket));
            } else {
                renderer.line(&format!("s3 {} {{", s3.bucket));
                renderer.indented(|renderer| render_s3_body(renderer, &s3.body));
                renderer.line("}");
            }
        }
        TopItem::Env(env) => {
            renderer.line(&format!("env {} {{", env.group));
            renderer.indented(|renderer| render_block(renderer, &env.entries, render_env_item));
            renderer.line("}");
        }
    }
}

fn render_image(renderer: &mut Renderer, image: &Image) {
    let source = match &image.source {
        ImageSource::Dir(dir) => dir.clone(),
        ImageSource::From(reference) => format!("from {reference}"),
    };
    renderer.line(&format!("image {} {} {{", image.id, source));
    renderer.indented(|renderer| render_block(renderer, &image.body, render_image_item));
    renderer.line("}");
}

fn render_image_item(renderer: &mut Renderer, item: &ImageItem) {
    match item {
        ImageItem::Dockerfile(path) => renderer.line(&format!("dockerfile {path}")),
        ImageItem::Stage(stage) => renderer.line(&format!("stage {stage}")),
        ImageItem::Workdir(path) => renderer.line(&format!("workdir {path}")),
        ImageItem::Arg(arg) => {
            renderer.line(&format!("arg {} = {}", arg.key, render_expr(&arg.value)))
        }
        ImageItem::ArgBlock(block) => {
            renderer.line("arg {");
            renderer.indented(|renderer| {
                render_block(renderer, &block.args, |renderer, arg| {
                    renderer.line(&format!("{} = {}", arg.key, render_expr(&arg.value)));
                });
            });
            renderer.line("}");
        }
        ImageItem::UseEnv(group) => renderer.line(&format!("use env {group}")),
        ImageItem::Env(env) => {
            renderer.line("env {");
            renderer.indented(|renderer| render_block(renderer, &env.entries, render_env_item));
            renderer.line("}");
        }
        ImageItem::BeforeStart(hook) => render_hook(renderer, "before-start", hook),
        ImageItem::PreDeploy(hook) => render_hook(renderer, "pre-deploy", hook),
        ImageItem::PostDeploy(hook) => render_hook(renderer, "post-deploy", hook),
        ImageItem::FailedDeploy(hook) => render_hook(renderer, "failed-deploy", hook),
        ImageItem::Service(service) => render_service(renderer, service),
        ImageItem::On(on) => render_on(renderer, on, render_image_item),
    }
}

fn render_service(renderer: &mut Renderer, service: &Service) {
    renderer.line(&format!("service {} {{", service.id));
    renderer.indented(|renderer| render_block(renderer, &service.body, render_service_item));
    renderer.line("}");
}

fn render_service_item(renderer: &mut Renderer, item: &ServiceItem) {
    match item {
        ServiceItem::Port(port) => renderer.line(&format!("port {port}")),
        ServiceItem::InternalPorts(ports) => {
            let list = ports
                .iter()
                .map(u16::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            renderer.line(&format!("internal_ports [{list}]"));
        }
        ServiceItem::Route(route) => render_route(renderer, route),
        ServiceItem::BeforeStart(hook) => render_hook(renderer, "before-start", hook),
        ServiceItem::PreDeploy(hook) => render_hook(renderer, "pre-deploy", hook),
        ServiceItem::PostDeploy(hook) => render_hook(renderer, "post-deploy", hook),
        ServiceItem::FailedDeploy(hook) => render_hook(renderer, "failed-deploy", hook),
        ServiceItem::Run(hook) => render_hook(renderer, "run", hook),
        ServiceItem::On(on) => render_on(renderer, on, render_service_item),
    }
}

fn render_hook(renderer: &mut Renderer, keyword: &str, hook: &Hook) {
    let mut head = keyword.to_string();
    if let Some(paths) = &hook.when_changed {
        head.push_str(" when changed(");
        head.push_str(&paths.join(", "));
        head.push(')');
    }
    match &hook.body {
        HookBody::Inline(command) => {
            renderer.line(&format!("{head}: {}", command.text()));
        }
        HookBody::Block(block) => {
            renderer.line(&format!("{head} {{"));
            renderer.indented(|renderer| {
                render_block(renderer, block, render_command_item);
            });
            renderer.line("}");
        }
    }
}

fn render_command_item(renderer: &mut Renderer, item: &CommandItem) {
    match item {
        CommandItem::Command(command) => renderer.line(&command.text()),
        CommandItem::On(on) => render_on(renderer, on, render_command_item),
    }
}

fn render_route(renderer: &mut Renderer, route: &Route) {
    let mut line = String::from("route");
    if let Some(path) = &route.path {
        line.push(' ');
        line.push_str(path);
    }
    match &route.kind {
        RouteKind::Preserve => {}
        RouteKind::StripPrefix => line.push_str(" strip-prefix"),
        RouteKind::Rewrite(target) => {
            line.push_str(" rewrite ");
            line.push_str(target);
        }
        RouteKind::Exact(path) => {
            line.push_str(" exact ");
            line.push_str(path);
        }
    }
    match &route.domain {
        Some(DomainFilter::Domain { name: domain, .. }) => {
            line.push_str(" domain ");
            line.push_str(domain);
        }
        None => {}
    }
    renderer.line(&line);
}

fn render_env_item(renderer: &mut Renderer, item: &EnvItem) {
    match item {
        EnvItem::Entry(entry) => {
            renderer.line(&format!("{} = {}", entry.key, render_expr(&entry.value)));
        }
        EnvItem::On(on) => render_on(renderer, on, render_env_item),
    }
}

fn render_postgres_item(renderer: &mut Renderer, item: &PostgresItem) {
    match item {
        PostgresItem::Database(database) => renderer.line(&format!("database {}", database.id)),
        PostgresItem::Version(version) => {
            renderer.line(&format!("version {}", render_version(version)));
        }
        PostgresItem::On(on) => render_on(renderer, on, render_postgres_item),
    }
}

fn render_redis_item(renderer: &mut Renderer, item: &RedisItem) {
    match item {
        RedisItem::Version(version) => {
            renderer.line(&format!("version {}", render_version(version)));
        }
        RedisItem::On(on) => render_on(renderer, on, render_redis_item),
    }
}

fn render_s3_body(renderer: &mut Renderer, body: &Block<S3Item>) {
    if body.comments.is_empty() {
        let mut ordered: Vec<&S3Item> = body.items.iter().collect();
        ordered.sort_by_key(|item: &&S3Item| !matches!(**item, S3Item::Policy(_)));
        for item in ordered {
            render_s3_item(renderer, item);
        }
    } else {
        render_block(renderer, body, render_s3_item);
    }
}

fn render_s3_item(renderer: &mut Renderer, item: &S3Item) {
    match item {
        S3Item::Policy(decl) => renderer.line(&format!("policy {}", decl.policy.keyword())),
        S3Item::Cdn(cdn) => renderer.line(&format!("cdn {}", cdn.id)),
        S3Item::On(on) => render_on(renderer, on, render_s3_item),
    }
}

fn render_version(version: &str) -> String {
    if !version.is_empty() && version.chars().all(|character| character.is_ascii_digit()) {
        version.to_string()
    } else {
        quote(version)
    }
}
