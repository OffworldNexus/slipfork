//! Flat migration namespace collection and declaration spans.

use std::collections::BTreeMap;

use super::build::Builder;
use super::snow::Snow;
use super::spans;
use crate::diag::Diagnostic;
use crate::span::{Position, Span};

type SeenSymbols = BTreeMap<String, FirstDeclaration>;

#[derive(Debug, Clone)]
struct FirstDeclaration {
    kind: &'static str,
    line: u32,
    col: u32,
}

impl<'a> Builder<'a> {
    fn declare(
        &mut self,
        seen: &mut SeenSymbols,
        id: &str,
        kind: &'static str,
        container: &[&str],
    ) {
        let span = declaration_span(self.source, container, id);
        if let Some(first) = seen.get(id).cloned() {
            self.diags.push(Diagnostic::new(
                span,
                format!(
                    "duplicate symbol `{id}`: `{}` at snow.yml:{}:{} and `{kind}` at snow.yml:{}:{}",
                    first.kind,
                    first.line,
                    first.col,
                    span.line(),
                    span.col()
                ),
                "rename one declaration so every id is globally unique",
            ));
            return;
        }
        seen.insert(
            id.to_string(),
            FirstDeclaration {
                kind,
                line: span.line(),
                col: span.col(),
            },
        );
    }

    pub(crate) fn collect(&mut self, snow: &Snow) {
        let mut seen: SeenSymbols = BTreeMap::new();
        for cluster in &snow.resources.pg_clusters {
            self.declare(
                &mut seen,
                &cluster.id,
                "postgres",
                &["resources", "pg_clusters"],
            );
        }
        for database in &snow.resources.pg_databases {
            self.declare(
                &mut seen,
                &database.id,
                "database",
                &["resources", "pg_databases"],
            );
        }
        for cluster in &snow.resources.redis_clusters {
            self.declare(
                &mut seen,
                &cluster.id,
                "redis",
                &["resources", "redis_clusters"],
            );
        }
        for bucket in &snow.resources.s3_buckets {
            self.declare(&mut seen, bucket.id(), "s3", &["resources", "s3_buckets"]);
        }
        for domain in &snow.domains {
            self.declare(&mut seen, domain.id(), "domain", &["domains"]);
        }
        for image in &snow.images {
            self.declare(&mut seen, &image.id, "image", &["images"]);
        }
        for component in &snow.components {
            self.declare(&mut seen, &component.id, "service", &["components"]);
        }

        self.pg_databases = snow
            .resources
            .pg_databases
            .iter()
            .map(|database| database.id.clone())
            .collect();
        self.redis = snow
            .resources
            .redis_clusters
            .iter()
            .map(|cluster| cluster.id.clone())
            .collect();
        self.buckets = snow
            .resources
            .s3_buckets
            .iter()
            .map(|bucket| bucket.id().to_string())
            .collect();
        self.domains = snow
            .domains
            .iter()
            .map(|domain| domain.id().to_string())
            .collect();
        self.images = snow.images.iter().map(|image| image.id.clone()).collect();
        self.components = snow
            .components
            .iter()
            .map(|component| component.id.clone())
            .collect();
        self.env_groups = snow.envs.keys().cloned().collect();
        for mode in snow.modes.values() {
            self.env_groups.extend(mode.envs.keys().cloned());
        }
    }
}
fn declaration_span(source: &str, container: &[&str], id: &str) -> Span {
    let mut scalar: Vec<&str> = container.to_vec();
    scalar.push(id);
    if let Some(position) = spans::find(source, &scalar) {
        return Span::at(position);
    }
    let mut keyed: Vec<&str> = container.to_vec();
    keyed.push("id");
    if let Some(position) = spans::find(source, &keyed) {
        return Span::at(position);
    }
    Span::at(Position::new(1, 1, 0))
}

pub(crate) fn push_unique(list: &mut Vec<String>, value: &str) {
    if !list.iter().any(|existing| existing == value) {
        list.push(value.to_string());
    }
}

pub(crate) fn hook_keyword(when: &str) -> Option<&'static str> {
    match when {
        "before_start" | "before-start" => Some("before-start"),
        "pre_deploy" | "pre-deploy" => Some("pre-deploy"),
        "post_deploy" | "post-deploy" => Some("post-deploy"),
        "failed_deploy" | "failed-deploy" => Some("failed-deploy"),
        _ => None,
    }
}
