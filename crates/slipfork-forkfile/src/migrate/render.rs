//! Renders the migration model into a `Forkfile` AST and canonical text.

use super::build::{EnvEntries, ImageOut, Model, ServiceOut, SourceOut};
use crate::ast::*;
use crate::expr::{Command, CommandPart, Expr, ExprKind};
use crate::format;
use crate::span::{Position, Span};

pub(crate) fn render(model: &Model) -> String {
    format(&document(model))
}

fn document(model: &Model) -> Forkfile {
    let at = at();
    let mut items = Vec::new();
    if !model.features.is_empty() {
        let groups = model
            .features
            .iter()
            .map(|name| FeatureGroup {
                name: name.clone(),
                features: None,
                span: at,
            })
            .collect();
        items.push(TopItem::Features(Features {
            groups: Block {
                items: groups,
                comments: Vec::new(),
                blank_lines: Vec::new(),
            },
            span: at,
        }));
    }
    for postgres in &model.postgres {
        let mut body = Vec::new();
        if let Some(version) = &postgres.version {
            body.push(PostgresItem::Version(version.clone()));
        }
        for database in &postgres.databases {
            body.push(PostgresItem::Database(DatabaseDecl {
                id: database.clone(),
                id_span: at,
                span: at,
            }));
        }
        items.push(TopItem::Postgres(Postgres {
            id: postgres.id.clone(),
            id_span: at,
            body: Block {
                items: body,
                comments: Vec::new(),
                blank_lines: Vec::new(),
            },
            span: at,
        }));
    }
    for redis in &model.redis {
        let body = match &redis.version {
            Some(version) => Block {
                items: vec![RedisItem::Version(version.clone())],
                comments: Vec::new(),
                blank_lines: Vec::new(),
            },
            None => Block::default(),
        };
        items.push(TopItem::Redis(Redis {
            id: redis.id.clone(),
            id_span: at,
            body,
            span: at,
        }));
    }
    for bucket in &model.s3 {
        items.push(TopItem::S3(S3 {
            bucket: bucket.clone(),
            id_span: at,
            body: Block::default(),
            span: at,
        }));
    }
    for domain in &model.domains {
        items.push(TopItem::Domain(Domain {
            id: domain.clone(),
            id_span: at,
            span: at,
        }));
    }
    for (group, entries) in &model.envs {
        items.push(TopItem::Env(env(group, entries, at)));
    }
    for mode in &model.mode_envs {
        items.push(TopItem::On(On {
            cond: feature(&mode.mode, at),
            body: OnBody::Block(Block {
                items: vec![TopItem::Env(env(&mode.group, &mode.entries, at))],
                comments: Vec::new(),
                blank_lines: Vec::new(),
            }),
            else_body: None,
            span: at,
        }));
    }
    for image in &model.images {
        items.push(TopItem::Image(image_ast(image, at)));
    }
    // The single leading comment renders first, then every top-level block keeps
    // exactly one blank line between it and the previous block.
    let mut blank_lines = vec![false; items.len() + 1];
    for blank in blank_lines.iter_mut().skip(2) {
        *blank = true;
    }
    Forkfile {
        version: "0.1".to_string(),
        span: at,
        items: Block {
            items,
            comments: vec![Comment {
                index: 0,
                text: "# migrated from snow.yml".to_string(),
                trailing: false,
            }],
            blank_lines,
        },
    }
}

fn env(group: &str, entries: &EnvEntries, at: Span) -> Env {
    Env {
        group: group.to_string(),
        entries: Block {
            items: entries
                .iter()
                .map(|(key, value)| {
                    EnvItem::Entry(EnvEntry {
                        key: key.clone(),
                        value: value.clone(),
                        span: at,
                    })
                })
                .collect(),
            comments: Vec::new(),
            blank_lines: Vec::new(),
        },
        span: at,
    }
}

fn image_ast(image: &ImageOut, at: Span) -> Image {
    let mut body = Vec::new();
    if let Some(dockerfile) = &image.dockerfile {
        body.push(ImageItem::Dockerfile(dockerfile.clone()));
    }
    for group in &image.uses {
        body.push(ImageItem::UseEnv(group.clone()));
    }
    for mode in &image.mode_uses {
        for group in &mode.groups {
            body.push(ImageItem::On(On {
                cond: feature(&mode.mode, at),
                body: OnBody::Inline(Box::new(ImageItem::UseEnv(group.clone()))),
                else_body: None,
                span: at,
            }));
        }
    }
    for service in &image.services {
        body.push(ImageItem::Service(service_ast(service, at)));
    }
    Image {
        id: image.id.clone(),
        id_span: at,
        source: match &image.source {
            SourceOut::Dir(dir) => ImageSource::Dir(dir.clone()),
            SourceOut::From(reference) => ImageSource::From(reference.clone()),
        },
        body: Block {
            items: body,
            comments: Vec::new(),
            blank_lines: Vec::new(),
        },
        span: at,
    }
}

fn service_ast(service: &ServiceOut, at: Span) -> Service {
    let mut items = Vec::new();
    if let Some(port) = service.port {
        items.push(ServiceItem::Port(port));
    }
    if !service.internal_ports.is_empty() {
        items.push(ServiceItem::InternalPorts(service.internal_ports.clone()));
    }
    for path in &service.routes {
        items.push(ServiceItem::Route(Route {
            path: Some(path.clone()),
            kind: RouteKind::Preserve,
            domain: None,
            span: at,
        }));
    }
    for (keyword, command) in &service.hooks {
        items.push(match keyword.as_str() {
            "before-start" => ServiceItem::BeforeStart(hook(command, at)),
            "pre-deploy" => ServiceItem::PreDeploy(hook(command, at)),
            "post-deploy" => ServiceItem::PostDeploy(hook(command, at)),
            "failed-deploy" => ServiceItem::FailedDeploy(hook(command, at)),
            _ => ServiceItem::Run(hook(command, at)),
        });
    }
    items.push(ServiceItem::Run(hook(&service.run, at)));
    Service {
        id: service.id.clone(),
        id_span: at,
        body: Block {
            items,
            comments: Vec::new(),
            blank_lines: Vec::new(),
        },
        span: at,
    }
}

fn hook(command: &str, at: Span) -> Hook {
    Hook {
        when_changed: None,
        body: HookBody::Inline(Command {
            tool: None,
            parts: vec![CommandPart::Text(command.to_string())],
            span: at,
        }),
        span: at,
    }
}

fn feature(name: &str, at: Span) -> Expr {
    Expr {
        kind: ExprKind::Feature(name.to_string()),
        span: at,
    }
}

fn at() -> Span {
    Span::at(Position::new(1, 1, 0))
}
