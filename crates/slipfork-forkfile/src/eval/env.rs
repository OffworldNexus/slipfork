//! Component `env` collection and evaluation.

use std::collections::BTreeMap;

use crate::ast::{ActiveBody, EnvItem, ImageItem, TopItem};
use crate::expr::{Expr, GeneratorSpec};

use super::values::{BoundSymbol, eval_expr, is_tainted};
use crate::gate::each_enabled;

use super::{FeatureSet, Resolvers, active};

#[allow(clippy::too_many_arguments)]
pub(super) fn collect_components(
    items: &[TopItem],
    features: &FeatureSet,
    symbols: &BTreeMap<String, BoundSymbol>,
    groups: &BTreeMap<String, Vec<(String, Expr)>>,
    world: &str,
    scheme: &str,
    resolvers: &Resolvers<'_>,
    out: &mut BTreeMap<String, BTreeMap<String, (String, bool)>>,
) {
    let context = EnvEval {
        features,
        symbols,
        groups,
        world,
        scheme,
        resolvers,
    };
    context.collect_components(items, out);
}

pub(super) fn collect_env_groups(
    item: &TopItem,
    features: &FeatureSet,
    groups: &mut BTreeMap<String, Vec<(String, Expr)>>,
) {
    match item {
        TopItem::Env(env) => {
            let list = groups.entry(env.group.clone()).or_default();
            collect_env_entries(&env.entries.items, features, list);
        }
        TopItem::On(on) => {
            if let Some(body) = active(on, features) {
                match body {
                    ActiveBody::Inline(inner) => collect_env_groups(inner, features, groups),
                    ActiveBody::Block(block) => {
                        for inner in &block.items {
                            collect_env_groups(inner, features, groups);
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

pub(super) fn collect_env_entries(
    items: &[EnvItem],
    features: &FeatureSet,
    out: &mut Vec<(String, Expr)>,
) {
    for item in items {
        match item {
            EnvItem::Entry(entry) => out.push((entry.key.clone(), entry.value.clone())),
            EnvItem::On(on) => {
                if let Some(body) = active(on, features) {
                    match body {
                        ActiveBody::Inline(inner) => {
                            collect_env_entries(std::slice::from_ref(inner), features, out);
                        }
                        ActiveBody::Block(block) => {
                            collect_env_entries(&block.items, features, out);
                        }
                    }
                }
            }
        }
    }
}

/// The shared evaluation context for component `env` collection.
struct EnvEval<'a> {
    features: &'a FeatureSet,
    symbols: &'a BTreeMap<String, BoundSymbol>,
    groups: &'a BTreeMap<String, Vec<(String, Expr)>>,
    world: &'a str,
    scheme: &'a str,
    resolvers: &'a Resolvers<'a>,
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
            let secret = |secret_name: &str, generator: Option<&GeneratorSpec>| {
                (self.resolvers.secret)(component, secret_name, generator).or_else(|| {
                    origin
                        .as_deref()
                        .and_then(|group| (self.resolvers.secret)(group, secret_name, generator))
                })
            };
            let var = |var_key: &str, generator: Option<&GeneratorSpec>| {
                (self.resolvers.var)(var_key, generator)
            };
            let resolved = eval_expr(expr, self.world, self.scheme, self.symbols, &secret, &var);
            if let Some(val) = resolved {
                map.insert(key.clone(), (val, is_tainted(expr, self.symbols)));
            }
        }
        map
    }
}

pub(super) fn collect_image_env_items(
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
            ImageItem::On(on) => {
                if let Some(body) = active(on, features) {
                    match body {
                        ActiveBody::Inline(inner) => collect_image_env_items(
                            std::slice::from_ref(inner),
                            features,
                            groups,
                            entries,
                            services,
                        ),
                        ActiveBody::Block(block) => {
                            collect_image_env_items(
                                &block.items,
                                features,
                                groups,
                                entries,
                                services,
                            );
                        }
                    }
                }
            }
            _ => {}
        }
    }
}
