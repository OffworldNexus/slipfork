//! One status row's middle and target columns, and their colour.

use crate::ps::{PsBucketPolicy, PsKind, PsRoute, PsService};

/// The uncoloured columns of one row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Columns {
    /// State (`running`, `failed`, `starting`) or a resource descriptor.
    pub middle: String,
    /// Port/route summary, or `-`.
    pub target: String,
    /// ANSI code for a state, when the middle column is a state.
    pub state: Option<&'static str>,
}

/// Computes one row's columns; a stopped pod wins over its resource descriptor so `pg`, `cache`,
/// and `uploads` render red state instead of a healthy-looking version.
#[must_use]
pub fn columns(service: &PsService, booting: bool) -> Columns {
    if booting {
        return Columns {
            middle: "starting".to_owned(),
            target: target(service),
            state: Some("33"),
        };
    }
    if !service.running {
        return Columns {
            middle: "failed".to_owned(),
            target: target(service),
            state: Some("31"),
        };
    }
    if service.kind == PsKind::Service {
        return Columns {
            middle: "running".to_owned(),
            target: target(service),
            state: Some("32"),
        };
    }
    Columns {
        middle: descriptor(service),
        target: target(service),
        state: None,
    }
}

fn descriptor(service: &PsService) -> String {
    match service.kind {
        PsKind::Postgres => format!("postgres {}", service.version.as_deref().unwrap_or("18")),
        PsKind::Redis => format!("redis {}", service.version.as_deref().unwrap_or("7")),
        PsKind::S3 if service.policy == Some(PsBucketPolicy::PublicRead) => {
            "s3 (public-read)".to_owned()
        }
        PsKind::S3 => "s3 (private)".to_owned(),
        PsKind::Cdn => "cdn".to_owned(),
        PsKind::Image => "image".to_owned(),
        PsKind::Service => "running".to_owned(),
    }
}

fn target(service: &PsService) -> String {
    if service.kind == PsKind::S3
        && let Some(cdn) = &service.cdn
    {
        return format!("-> {cdn}.*");
    }
    let mut target = match service
        .port
        .or_else(|| service.routes.first().map(|route| route.port))
    {
        Some(port) => format!(":{port}"),
        None => "-".to_owned(),
    };
    if let Some(route) = service.routes.first() {
        let label = route_label(route);
        target = if target == "-" {
            format!("-> {label}")
        } else {
            format!("{target} -> {label}")
        };
    }
    target
}

fn route_label(route: &PsRoute) -> String {
    match &route.domain {
        Some(domain) => {
            let label = domain.label();
            format!("{label}{}", route.prefix)
        }
        None => route.prefix.clone(),
    }
}
