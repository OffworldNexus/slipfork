//! `s3` bucket-policy pairing and RFC 1123 authority validation.

use std::collections::BTreeSet;

use crate::ast::{BucketPolicy, Domain, DomainFilter, Route, S3, S3Item};
use crate::diag::{Diagnostic, Diagnostics};
use crate::dns::DnsLabel;
use crate::expr::{Expr, ExprKind};
use crate::resources::{for_each_active_s3_item, for_each_s3_item, s3_cdn_branches};
use crate::span::Span;

const MAX_ENUMERATED_FEATURES: usize = 12;

pub(super) fn check_domain(domain: &Domain, diags: &mut Diagnostics) {
    check_label(&domain.id, domain.id_span, "domain <id>", false, diags);
}

pub(super) fn check_route_prefix(route: &Route, diags: &mut Diagnostics) {
    let Some(DomainFilter::Prefix { prefix, span }) = &route.domain else {
        return;
    };
    check_label(prefix, *span, "route ... domain <prefix>.*", false, diags);
}

/// Validates a bucket name, every `cdn` id, and the `cdn`/`policy public-read` pairing
/// across each `on` branch.
pub(super) fn check_s3(s3: &S3, features: &BTreeSet<String>, diags: &mut Diagnostics) {
    check_label(&s3.bucket, s3.id_span, "s3 <bucket>", true, diags);

    let mut cdns = Vec::new();
    let mut names = BTreeSet::new();
    for_each_s3_item(&s3.body, &mut |item| match item {
        S3Item::Cdn(cdn) => cdns.push(cdn),
        S3Item::On(on) => {
            super::expr::check_condition(&on.cond, features, diags);
            collect_condition(&on.cond, &mut names);
        }
        S3Item::Policy(_) => {}
    });
    for cdn in &cdns {
        check_label(&cdn.id, cdn.id_span, "cdn <id>", false, diags);
    }

    let names: Vec<String> = names.into_iter().collect();
    if names.len() <= MAX_ENUMERATED_FEATURES {
        for mask in 0..(1u32 << names.len()) {
            let active: BTreeSet<String> = names
                .iter()
                .enumerate()
                .filter(|(index, _)| mask & (1u32 << index) != 0)
                .map(|(_, name)| name.clone())
                .collect();
            check_branch(s3, &active, diags);
        }
    } else {
        check_branch(s3, &BTreeSet::new(), diags);
        for (_, enclosing) in s3_cdn_branches(&s3.body) {
            check_branch(s3, &enclosing, diags);
        }
    }
}

fn check_branch(s3: &S3, active: &BTreeSet<String>, diags: &mut Diagnostics) {
    if s3.policy_at(active) == BucketPolicy::PublicRead {
        return;
    }
    for_each_active_s3_item(&s3.body, active, &mut |item| {
        if let S3Item::Cdn(cdn) = item {
            diags.push(Diagnostic::new(
                cdn.span,
                format!(
                    "'cdn {}' inside 's3 {}' requires 'policy public-read'",
                    cdn.id, s3.bucket
                ),
                format!(
                    "add 'policy public-read' inside 's3 {}' to enable 'cdn {}'",
                    s3.bucket, cdn.id
                ),
            ));
        }
    });
}

fn collect_condition(expr: &Expr, out: &mut BTreeSet<String>) {
    match &expr.kind {
        ExprKind::Feature(name) => {
            out.insert(name.clone());
        }
        ExprKind::Not(inner) => collect_condition(inner, out),
        ExprKind::And(left, right) | ExprKind::Or(left, right) => {
            collect_condition(left, out);
            collect_condition(right, out);
        }
        ExprKind::Fallback(left, _) => collect_condition(left, out),
        _ => {}
    }
}

fn check_label(value: &str, span: Span, slot: &str, bucket: bool, diags: &mut Diagnostics) {
    let result = if bucket {
        DnsLabel::bucket(value)
    } else {
        DnsLabel::new(value)
    };
    if let Err(error) = result {
        diags.push(Diagnostic::new(
            span,
            error.message(value),
            error.next(slot),
        ));
    }
}
