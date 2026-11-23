//! Feature-aware runtime evaluation for `arg`, `env`, `stage`, and app domains.
//!
//! Pure AST evaluation: the caller injects the world name, the scheme, and secret/var resolvers;
//! this module never reads `std::env` or touches the filesystem.

mod env;
mod values;

use std::collections::{BTreeMap, BTreeSet};

use self::values::expand_feature;
use crate::ast::{ActiveBody, Forkfile, Image, ImageItem, On, PostgresItem, S3Item, TopItem};
use crate::expr::{Expr, ExprKind, GeneratorSpec};
use values::{BoundSymbol, eval_expr, is_tainted};

/// A component-scoped `secret(key, generator)` resolver.
pub type SecretResolver<'a> = dyn Fn(&str, &str, Option<&GeneratorSpec>) -> Option<String> + 'a;

/// A `var(key, generator)` resolver.
pub type VarResolver<'a> = dyn Fn(&str, Option<&GeneratorSpec>) -> Option<String> + 'a;

/// Injected secret and variable resolvers for one world evaluation.
pub struct Resolvers<'a> {
    /// Resolves `secret(key, generator)` for the component that declared it.
    pub secret: &'a SecretResolver<'a>,
    /// Resolves `var(key, generator)`.
    pub var: &'a VarResolver<'a>,
}

/// Active feature set (`declared == false` when the `Forkfile` has no `features` block).
#[derive(Debug, Clone)]
pub struct FeatureSet {
    declared: bool,
    active: BTreeSet<String>,
}

impl FeatureSet {
    /// The active feature names.
    #[must_use]
    pub fn active(&self) -> &BTreeSet<String> {
        &self.active
    }

    /// Evaluates an `on <cond>` expression; when no `features` block exists, bare features pass.
    #[must_use]
    pub fn matches(&self, cond: &Expr) -> bool {
        match &cond.kind {
            ExprKind::Bool(value) => *value,
            ExprKind::Feature(name) => !self.declared || self.active.contains(name),
            ExprKind::Not(inner) => !self.matches(inner),
            ExprKind::And(left, right) => self.matches(left) && self.matches(right),
            ExprKind::Or(left, right) => self.matches(left) || self.matches(right),
            _ => false,
        }
    }
}

/// The body an `on` node selects under `features`.
pub(super) fn active<'a, I>(on: &'a On<I>, features: &FeatureSet) -> Option<ActiveBody<'a, I>> {
    let mut matches = |expr: &Expr| features.matches(expr);
    on.active_body(&mut matches)
}

impl Forkfile {
    /// Expands `default` and its transitive feature groups.
    #[must_use]
    pub fn active_features(&self) -> FeatureSet {
        let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut declared = false;
        for item in &self.items.items {
            if let TopItem::Features(features) = item {
                declared = true;
                for group in &features.groups.items {
                    groups.insert(
                        group.name.clone(),
                        group.features.clone().unwrap_or_default(),
                    );
                }
            }
        }
        let mut active = BTreeSet::new();
        if groups.contains_key("default") {
            expand_feature("default", &groups, &mut active);
        }
        FeatureSet { declared, active }
    }

    /// Declared top-level app `domain <id>` names active under default features.
    #[must_use]
    pub fn app_domains(&self) -> Vec<&str> {
        let features = self.active_features();
        let mut out = Vec::new();
        for item in &self.items.items {
            collect_app_domains(item, &features, &mut out);
        }
        out
    }

    /// Evaluates build-time `arg` bindings for `image_id` in `world`.
    #[must_use]
    pub fn eval_image_args(
        &self,
        world: &str,
        scheme: &str,
        image_id: &str,
    ) -> BTreeMap<String, String> {
        let features = self.active_features();
        let symbols = self.bound_symbols(&features);
        let mut args = BTreeMap::new();
        if let Some(image) = find_image(&self.items.items, image_id, &features) {
            collect_image_args(
                &image.body.items,
                &features,
                &symbols,
                world,
                scheme,
                &mut args,
            );
        }
        args
    }

    /// Evaluates target `stage <name>` for `image_id`, when active.
    #[must_use]
    pub fn eval_image_stage(&self, image_id: &str) -> Option<String> {
        let features = self.active_features();
        let image = find_image(&self.items.items, image_id, &features)?;
        let mut stage = None;
        collect_image_stage(&image.body.items, &features, &mut stage);
        stage
    }

    /// Evaluates every runtime `env` variable for every component (`group`, `image`, `service`).
    #[must_use]
    pub fn eval_all_env_with(
        &self,
        world: &str,
        scheme: &str,
        resolvers: &Resolvers<'_>,
    ) -> BTreeMap<String, BTreeMap<String, (String, bool)>> {
        let features = self.active_features();
        let symbols = self.bound_symbols(&features);
        let mut raw_groups: BTreeMap<String, Vec<(String, Expr)>> = BTreeMap::new();
        for item in &self.items.items {
            env::collect_env_groups(item, &features, &mut raw_groups);
        }
        let mut out: BTreeMap<String, BTreeMap<String, (String, bool)>> = BTreeMap::new();
        for (group, entries) in &raw_groups {
            let mut map = BTreeMap::new();
            let secret = |key: &str, generator: Option<&GeneratorSpec>| {
                (resolvers.secret)(group, key, generator)
            };
            let var =
                |key: &str, generator: Option<&GeneratorSpec>| (resolvers.var)(key, generator);
            for (key, expr) in entries {
                if let Some(val) = eval_expr(expr, world, scheme, &symbols, &secret, &var) {
                    map.insert(key.clone(), (val, is_tainted(expr, &symbols)));
                }
            }
            out.insert(group.clone(), map);
        }
        env::collect_components(
            &self.items.items,
            &features,
            &symbols,
            &raw_groups,
            world,
            scheme,
            resolvers,
            &mut out,
        );
        out
    }

    fn bound_symbols(&self, features: &FeatureSet) -> BTreeMap<String, BoundSymbol> {
        let mut map = BTreeMap::new();
        let mut saw_domain = false;
        for item in &self.items.items {
            collect_symbols(item, features, &mut saw_domain, &mut map);
        }
        map
    }
}

fn collect_app_domains<'a>(item: &'a TopItem, features: &FeatureSet, out: &mut Vec<&'a str>) {
    match item {
        TopItem::Domain(domain) => out.push(&domain.id),
        TopItem::On(on) => {
            if let Some(body) = active(on, features) {
                match body {
                    ActiveBody::Inline(inner) => collect_app_domains(inner, features, out),
                    ActiveBody::Block(block) => {
                        for inner in &block.items {
                            collect_app_domains(inner, features, out);
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

fn find_image<'a>(items: &'a [TopItem], id: &str, features: &FeatureSet) -> Option<&'a Image> {
    for item in items {
        match item {
            TopItem::Image(image) if image.id == id => return Some(image),
            TopItem::On(on) => {
                if let Some(body) = active(on, features) {
                    match body {
                        ActiveBody::Inline(inner) => {
                            if let Some(found) =
                                find_image(std::slice::from_ref(inner), id, features)
                            {
                                return Some(found);
                            }
                        }
                        ActiveBody::Block(block) => {
                            if let Some(found) = find_image(&block.items, id, features) {
                                return Some(found);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    None
}

fn collect_image_args(
    items: &[ImageItem],
    features: &FeatureSet,
    symbols: &BTreeMap<String, BoundSymbol>,
    world: &str,
    scheme: &str,
    args: &mut BTreeMap<String, String>,
) {
    let secret = |_: &str, _: Option<&GeneratorSpec>| None;
    let var = |_: &str, _: Option<&GeneratorSpec>| None;
    for item in items {
        match item {
            ImageItem::Arg(decl) => {
                if let Some(val) = eval_expr(&decl.value, world, scheme, symbols, &secret, &var) {
                    args.insert(decl.key.clone(), val);
                }
            }
            ImageItem::ArgBlock(block) => {
                for decl in &block.args.items {
                    if let Some(val) = eval_expr(&decl.value, world, scheme, symbols, &secret, &var)
                    {
                        args.insert(decl.key.clone(), val);
                    }
                }
            }
            ImageItem::On(on) => {
                if let Some(body) = active(on, features) {
                    match body {
                        ActiveBody::Inline(inner) => collect_image_args(
                            std::slice::from_ref(inner),
                            features,
                            symbols,
                            world,
                            scheme,
                            args,
                        ),
                        ActiveBody::Block(block) => {
                            collect_image_args(
                                &block.items,
                                features,
                                symbols,
                                world,
                                scheme,
                                args,
                            );
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn collect_image_stage(items: &[ImageItem], features: &FeatureSet, stage: &mut Option<String>) {
    for item in items {
        match item {
            ImageItem::Stage(name) => *stage = Some(name.clone()),
            ImageItem::On(on) => {
                if let Some(body) = active(on, features) {
                    match body {
                        ActiveBody::Inline(inner) => {
                            collect_image_stage(std::slice::from_ref(inner), features, stage);
                        }
                        ActiveBody::Block(block) => {
                            collect_image_stage(&block.items, features, stage);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn collect_symbols(
    item: &TopItem,
    features: &FeatureSet,
    saw_domain: &mut bool,
    map: &mut BTreeMap<String, BoundSymbol>,
) {
    match item {
        TopItem::Domain(domain) => {
            let primary = !*saw_domain;
            *saw_domain = true;
            map.insert(
                domain.id.clone(),
                BoundSymbol::Domain {
                    id: domain.id.clone(),
                    primary,
                },
            );
        }
        TopItem::Postgres(pg) => {
            let mut dbs = Vec::new();
            collect_pg_dbs(&pg.body.items, features, &mut dbs);
            for db in &dbs {
                map.insert(db.clone(), BoundSymbol::Database { name: db.clone() });
            }
            let first_db = dbs.into_iter().next().unwrap_or_else(|| pg.id.clone());
            map.insert(pg.id.clone(), BoundSymbol::Postgres { first_db });
        }
        TopItem::Redis(redis) => {
            map.insert(redis.id.clone(), BoundSymbol::Redis);
        }
        TopItem::S3(s3) => {
            map.insert(
                s3.bucket.clone(),
                BoundSymbol::S3 {
                    bucket: s3.bucket.clone(),
                    policy: s3.policy_at(features.active()),
                },
            );
            collect_s3_cdns(&s3.body.items, features, &s3.bucket, map);
        }
        TopItem::On(on) => {
            if let Some(body) = active(on, features) {
                match body {
                    ActiveBody::Inline(inner) => {
                        collect_symbols(inner, features, saw_domain, map);
                    }
                    ActiveBody::Block(block) => {
                        for inner in &block.items {
                            collect_symbols(inner, features, saw_domain, map);
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

fn collect_pg_dbs(items: &[PostgresItem], features: &FeatureSet, out: &mut Vec<String>) {
    for item in items {
        match item {
            PostgresItem::Database(db) => out.push(db.id.clone()),
            PostgresItem::On(on) => {
                if let Some(body) = active(on, features) {
                    match body {
                        ActiveBody::Inline(inner) => {
                            collect_pg_dbs(std::slice::from_ref(inner), features, out);
                        }
                        ActiveBody::Block(block) => {
                            collect_pg_dbs(&block.items, features, out);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn collect_s3_cdns(
    items: &[S3Item],
    features: &FeatureSet,
    bucket: &str,
    map: &mut BTreeMap<String, BoundSymbol>,
) {
    for item in items {
        match item {
            S3Item::Cdn(cdn) => {
                map.insert(
                    cdn.id.clone(),
                    BoundSymbol::Cdn {
                        bucket: bucket.to_owned(),
                    },
                );
            }
            S3Item::On(on) => {
                if let Some(body) = active(on, features) {
                    match body {
                        ActiveBody::Inline(inner) => {
                            collect_s3_cdns(std::slice::from_ref(inner), features, bucket, map);
                        }
                        ActiveBody::Block(block) => {
                            collect_s3_cdns(&block.items, features, bucket, map);
                        }
                    }
                }
            }
            S3Item::Policy(_) => {}
        }
    }
}
