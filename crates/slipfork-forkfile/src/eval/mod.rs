//! Feature-aware runtime evaluation for `arg`, `env`, `stage`, and app domains.
//!
//! Pure AST evaluation: the caller injects the world name and an optional secret lookup closure;
//! this module never reads `std::env` or touches the filesystem.

mod values;

use std::collections::{BTreeMap, BTreeSet};

use self::values::expand_feature;
use crate::ast::{EnvItem, Forkfile, Image, ImageItem, OnBody, PostgresItem, S3Item, TopItem};
use crate::expr::{Expr, ExprKind};
use crate::gate::each_enabled;
use values::{BoundSymbol, eval_expr, is_tainted};

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
    pub fn eval_image_args(&self, world: &str, image_id: &str) -> BTreeMap<String, String> {
        let features = self.active_features();
        let symbols = self.bound_symbols(&features);
        let mut args = BTreeMap::new();
        if let Some(image) = find_image(&self.items.items, image_id, &features) {
            collect_image_args(&image.body.items, &features, &symbols, world, &mut args);
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
    pub fn eval_all_env(
        &self,
        world: &str,
        secret_lookup: &dyn Fn(&str, &str) -> Option<String>,
        var_lookup: &dyn Fn(&str) -> Option<String>,
    ) -> BTreeMap<String, BTreeMap<String, (String, bool)>> {
        let features = self.active_features();
        let symbols = self.bound_symbols(&features);
        let mut raw_groups: BTreeMap<String, Vec<(String, Expr)>> = BTreeMap::new();
        for item in &self.items.items {
            collect_env_groups(item, &features, &mut raw_groups);
        }
        let mut out: BTreeMap<String, BTreeMap<String, (String, bool)>> = BTreeMap::new();
        for (group, entries) in &raw_groups {
            let mut map = BTreeMap::new();
            for (key, expr) in entries {
                if let Some(val) = eval_expr(
                    expr,
                    world,
                    key,
                    &symbols,
                    &|k| secret_lookup(group, k),
                    var_lookup,
                ) {
                    map.insert(key.clone(), (val, is_tainted(expr, &symbols)));
                }
            }
            out.insert(group.clone(), map);
        }
        let context = EnvEval {
            features: &features,
            symbols: &symbols,
            groups: &raw_groups,
            world,
            secret_lookup,
            var_lookup,
        };
        context.collect_components(&self.items.items, &mut out);
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
        TopItem::On(on) if features.matches(&on.cond) => match &on.body {
            OnBody::Inline(inner) => collect_app_domains(inner, features, out),
            OnBody::Block(block) => {
                for inner in &block.items {
                    collect_app_domains(inner, features, out);
                }
            }
        },
        _ => {}
    }
}

fn find_image<'a>(items: &'a [TopItem], id: &str, features: &FeatureSet) -> Option<&'a Image> {
    for item in items {
        match item {
            TopItem::Image(image) if image.id == id => return Some(image),
            TopItem::On(on) if features.matches(&on.cond) => match &on.body {
                OnBody::Inline(inner) => {
                    if let Some(found) =
                        find_image(std::slice::from_ref(inner.as_ref()), id, features)
                    {
                        return Some(found);
                    }
                }
                OnBody::Block(block) => {
                    if let Some(found) = find_image(&block.items, id, features) {
                        return Some(found);
                    }
                }
            },
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
    args: &mut BTreeMap<String, String>,
) {
    for item in items {
        match item {
            ImageItem::Arg(decl) => {
                if let Some(val) =
                    eval_expr(&decl.value, world, &decl.key, symbols, &|_| None, &|_| None)
                {
                    args.insert(decl.key.clone(), val);
                }
            }
            ImageItem::ArgBlock(block) => {
                for decl in &block.args.items {
                    if let Some(val) =
                        eval_expr(&decl.value, world, &decl.key, symbols, &|_| None, &|_| None)
                    {
                        args.insert(decl.key.clone(), val);
                    }
                }
            }
            ImageItem::On(on) if features.matches(&on.cond) => match &on.body {
                OnBody::Inline(inner) => collect_image_args(
                    std::slice::from_ref(inner.as_ref()),
                    features,
                    symbols,
                    world,
                    args,
                ),
                OnBody::Block(block) => {
                    collect_image_args(&block.items, features, symbols, world, args);
                }
            },
            _ => {}
        }
    }
}

fn collect_image_stage(items: &[ImageItem], features: &FeatureSet, stage: &mut Option<String>) {
    for item in items {
        match item {
            ImageItem::Stage(name) => *stage = Some(name.clone()),
            ImageItem::On(on) if features.matches(&on.cond) => match &on.body {
                OnBody::Inline(inner) => {
                    collect_image_stage(std::slice::from_ref(inner.as_ref()), features, stage);
                }
                OnBody::Block(block) => collect_image_stage(&block.items, features, stage),
            },
            _ => {}
        }
    }
}

fn collect_env_groups(
    item: &TopItem,
    features: &FeatureSet,
    groups: &mut BTreeMap<String, Vec<(String, Expr)>>,
) {
    match item {
        TopItem::Env(env) => {
            let list = groups.entry(env.group.clone()).or_default();
            collect_env_entries(&env.entries.items, features, list);
        }
        TopItem::On(on) if features.matches(&on.cond) => match &on.body {
            OnBody::Inline(inner) => collect_env_groups(inner, features, groups),
            OnBody::Block(block) => {
                for inner in &block.items {
                    collect_env_groups(inner, features, groups);
                }
            }
        },
        _ => {}
    }
}

fn collect_env_entries(items: &[EnvItem], features: &FeatureSet, out: &mut Vec<(String, Expr)>) {
    for item in items {
        match item {
            EnvItem::Entry(entry) => out.push((entry.key.clone(), entry.value.clone())),
            EnvItem::On(on) if features.matches(&on.cond) => match &on.body {
                OnBody::Inline(inner) => {
                    collect_env_entries(std::slice::from_ref(inner.as_ref()), features, out);
                }
                OnBody::Block(block) => collect_env_entries(&block.items, features, out),
            },
            EnvItem::On(_) => {}
        }
    }
}

/// The shared evaluation context for component `env` collection.
struct EnvEval<'a> {
    features: &'a FeatureSet,
    symbols: &'a BTreeMap<String, BoundSymbol>,
    groups: &'a BTreeMap<String, Vec<(String, Expr)>>,
    world: &'a str,
    secret_lookup: &'a dyn Fn(&str, &str) -> Option<String>,
    var_lookup: &'a dyn Fn(&str) -> Option<String>,
}

impl EnvEval<'_> {
    fn collect_components(
        &self,
        items: &[TopItem],
        out: &mut BTreeMap<String, BTreeMap<String, (String, bool)>>,
    ) {
        each_enabled(items, self.features, &mut |item| {
            if let TopItem::Image(image) = item {
                let mut entries: Vec<(Option<String>, String, Expr)> = Vec::new();
                let mut services = Vec::new();
                collect_image_env_items(
                    &image.body.items,
                    self.features,
                    self.groups,
                    &mut entries,
                    &mut services,
                );
                out.insert(image.id.clone(), self.eval(&image.id, &entries));
                for service_id in services {
                    out.insert(service_id.clone(), self.eval(&service_id, &entries));
                }
            }
        });
    }

    fn eval(
        &self,
        component: &str,
        entries: &[(Option<String>, String, Expr)],
    ) -> BTreeMap<String, (String, bool)> {
        let mut map = BTreeMap::new();
        for (origin, key, expr) in entries {
            let resolved = eval_expr(
                expr,
                self.world,
                key,
                self.symbols,
                &|secret_name| {
                    self.secret_lookup(component, secret_name).or_else(|| {
                        origin
                            .as_deref()
                            .and_then(|group| self.secret_lookup(group, secret_name))
                    })
                },
                self.var_lookup,
            );
            if let Some(val) = resolved {
                map.insert(key.clone(), (val, is_tainted(expr, self.symbols)));
            }
        }
        map
    }

    fn secret_lookup(&self, component: &str, key: &str) -> Option<String> {
        (self.secret_lookup)(component, key)
    }
}

fn collect_image_env_items(
    items: &[ImageItem],
    features: &FeatureSet,
    groups: &BTreeMap<String, Vec<(String, Expr)>>,
    entries: &mut Vec<(Option<String>, String, Expr)>,
    services: &mut Vec<String>,
) {
    for item in items {
        match item {
            ImageItem::UseEnv(group) => {
                if let Some(inherited) = groups.get(group) {
                    for (k, v) in inherited {
                        entries.push((Some(group.clone()), k.clone(), v.clone()));
                    }
                }
            }
            ImageItem::Env(block) => {
                let mut inline = Vec::new();
                collect_env_entries(&block.entries.items, features, &mut inline);
                for (k, v) in inline {
                    entries.push((None, k, v));
                }
            }
            ImageItem::Service(service) => services.push(service.id.clone()),
            ImageItem::On(on) if features.matches(&on.cond) => match &on.body {
                OnBody::Inline(inner) => collect_image_env_items(
                    std::slice::from_ref(inner.as_ref()),
                    features,
                    groups,
                    entries,
                    services,
                ),
                OnBody::Block(block) => {
                    collect_image_env_items(&block.items, features, groups, entries, services);
                }
            },
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
            collect_s3_cdns(&s3.body.items, features, map);
        }
        TopItem::On(on) if features.matches(&on.cond) => match &on.body {
            OnBody::Inline(inner) => collect_symbols(inner, features, saw_domain, map),
            OnBody::Block(block) => {
                for inner in &block.items {
                    collect_symbols(inner, features, saw_domain, map);
                }
            }
        },
        _ => {}
    }
}

fn collect_pg_dbs(items: &[PostgresItem], features: &FeatureSet, out: &mut Vec<String>) {
    for item in items {
        match item {
            PostgresItem::Database(db) => out.push(db.id.clone()),
            PostgresItem::On(on) if features.matches(&on.cond) => match &on.body {
                OnBody::Inline(inner) => {
                    collect_pg_dbs(std::slice::from_ref(inner.as_ref()), features, out);
                }
                OnBody::Block(block) => collect_pg_dbs(&block.items, features, out),
            },
            _ => {}
        }
    }
}

fn collect_s3_cdns(
    items: &[S3Item],
    features: &FeatureSet,
    map: &mut BTreeMap<String, BoundSymbol>,
) {
    for item in items {
        match item {
            S3Item::Cdn(cdn) => {
                map.insert(cdn.id.clone(), BoundSymbol::Cdn { id: cdn.id.clone() });
            }
            S3Item::On(on) if features.matches(&on.cond) => match &on.body {
                OnBody::Inline(inner) => {
                    collect_s3_cdns(std::slice::from_ref(inner.as_ref()), features, map);
                }
                OnBody::Block(block) => collect_s3_cdns(&block.items, features, map),
            },
            S3Item::On(_) | S3Item::Policy(_) => {}
        }
    }
}
